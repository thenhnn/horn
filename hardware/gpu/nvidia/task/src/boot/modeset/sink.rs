// SPDX-License-Identifier: GPL-2.0-only
//! One bounded pre-modeset wake gate, not hotplug recovery for an active head.
use super::*;

const QUIET_US: u64 = 2_000_000;
const MIN_WAKE_US: u64 = 3_000_000;
const BUDGET_US: u64 = 8_000_000;
const SAMPLE_US: u64 = 200_000;

fn connected<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    libos: Span,
    version: u32,
    deadline: u64,
    id: u32,
    cached: bool,
) -> Result<()> {
    let flags = if cached { raw::HORN_CONNECT_CACHED } else { 0 };
    let reply = rm_api(ctx, queue, HANDLE_DISP_CLIENT, libos, version, deadline)
        .control::<api::DisplayConnected>(
        rm::HANDLE_DISP_COMMON,
        &raw::NV0073_CTRL_SYSTEM_GET_CONNECT_STATE_PARAMS {
            displayMask: id,
            flags,
            ..Default::default()
        },
    )?;
    if reply.subDeviceInstance != 0
        || reply.flags != flags
        || reply.displayMask & !id != 0
        || reply.retryTimeMs != 0
    {
        return Err(Error::Malformed);
    }
    let now = ctx.time_us()?;
    ctx.with_platform(|p| p.display_wake.sample(id, reply.displayMask == id, now))?;
    sel4::debug_println!(
        "NVIDIA_DISPLAY_CONNECT clock_us={} id={:#x} cached={} connected={} retry_ms=0",
        now,
        id,
        cached,
        reply.displayMask == id
    );
    Ok(())
}

pub(super) fn ensure_settled(ctx: &mut Ad106<Native>, plan: &Link) -> Result<()> {
    let now = ctx.time_us()?;
    let watch = ctx.with_platform(|p| Ok(p.display_wake))?;
    if !watch.settled(plan.display_id, now, QUIET_US)? {
        sel4::debug_println!(
            "NVIDIA_DISPLAY_WAKE_INVALIDATED clock_us={} connected={:?} quiet_us={} no_retry=true",
            now,
            watch.connected,
            watch.quiet_us(now)?
        );
        return Err(Error::State);
    }
    Ok(())
}

/// D0 read/conditional write follows nouveau_dp_train, preserving other bits.
/// Three seconds minimum and two seconds without relevant HPD/DP_IRQ are an
/// explicit experiment policy, not a DisplayPort guarantee of panel readiness.
/// Successful connection samples are observations, not retries of failed RPCs.
pub(crate) fn settle<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    libos: Span,
    version: u32,
    boot_deadline: u64,
    plan: &Link,
) -> Result<()> {
    let start = ctx.time_us()?;
    let deadline = start
        .checked_add(BUDGET_US)
        .ok_or(Error::Range)?
        .min(boot_deadline);
    ctx.with_platform(|p| p.display_wake.begin(plan.display_id, start))?;
    sel4::debug_println!(
        "NVIDIA_DISPLAY_WAKE_BEGIN clock_us={} deadline_us={} id={:#x} minimum_us={} quiet_us={} training_submitted=false updates_submitted=false",
        start,
        deadline,
        plan.display_id,
        MIN_WAKE_US,
        QUIET_US
    );
    let mut power = [0];
    aux(
        &mut rm_api(ctx, queue, HANDLE_DISP_CLIENT, libos, version, deadline),
        plan.display_id,
        0x600,
        true,
        &mut power,
    )?;
    let previous = power[0];
    let wrote = previous & 3 != 1;
    if wrote {
        power[0] = (previous & !3) | 1;
        aux(
            &mut rm_api(ctx, queue, HANDLE_DISP_CLIENT, libos, version, deadline),
            plan.display_id,
            0x600,
            false,
            &mut power,
        )?;
    }
    let powered = ctx.time_us()?;
    sel4::debug_println!(
        "NVIDIA_DISPLAY_WAKE_POWER clock_us={} previous={:#x} requested={:#x} wrote={} panel_awake=unproven",
        powered,
        previous,
        power[0],
        wrote
    );
    connected(ctx, queue, libos, version, deadline, plan.display_id, false)?;
    let earliest = powered.checked_add(MIN_WAKE_US).ok_or(Error::Range)?;
    let mut next_sample = ctx.time_us()?.checked_add(SAMPLE_US).ok_or(Error::Range)?;
    loop {
        display_event(ctx, queue, libos, version, deadline)?;
        let now = ctx.time_us()?;
        if now >= deadline {
            let watch = ctx.with_platform(|p| Ok(p.display_wake))?;
            sel4::debug_println!(
                "NVIDIA_DISPLAY_WAKE_TIMEOUT clock_us={} connected={:?} quiet_us={} training_submitted=false updates_submitted=false",
                now,
                watch.connected,
                watch.quiet_us(now)?
            );
            return Err(Error::Timeout);
        }
        if now >= next_sample {
            connected(ctx, queue, libos, version, deadline, plan.display_id, true)?;
            next_sample = ctx.time_us()?.checked_add(SAMPLE_US).ok_or(Error::Range)?;
        }
        let now = ctx.time_us()?;
        let watch = ctx.with_platform(|p| Ok(p.display_wake))?;
        if now >= earliest && watch.settled(plan.display_id, now, QUIET_US)? {
            // Fresh full detection, not only cached connected state. It may
            // itself produce events; consume pending notifications afterwards.
            connected(ctx, queue, libos, version, deadline, plan.display_id, false)?;
            let mut power = [0];
            aux(
                &mut rm_api(ctx, queue, HANDLE_DISP_CLIENT, libos, version, deadline),
                plan.display_id,
                0x600,
                true,
                &mut power,
            )?;
            if power[0] & 3 != 1 {
                return Err(Error::State); // Never issue a second wake write.
            }
            display_event(ctx, queue, libos, version, deadline)?;
            let now = ctx.time_us()?;
            if now >= deadline {
                return Err(Error::Timeout);
            }
            ensure_settled(ctx, plan)?;
            sel4::debug_println!(
                "NVIDIA_DISPLAY_WAKE_READY clock_us={} elapsed_us={} dpcd_power={:#x} training_submitted=false updates_submitted=false panel_awake=unproven",
                now,
                now.saturating_sub(start),
                power[0]
            );
            return Ok(());
        }
        ctx.sleep_until(now.saturating_add(10_000).min(next_sample).min(deadline))?;
    }
}
