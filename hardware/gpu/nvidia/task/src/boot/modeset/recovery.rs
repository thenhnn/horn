// SPDX-License-Identifier: GPL-2.0-only
//! One recovery for a newly reconnected sink, strictly before any publication.
//! Failed RPCs are never retried. Active-head recovery is deliberately absent.
use super::*;
use link_probe::Observation;

const TOTAL_US: u64 = 30_000_000;
const RECONNECT_US: u64 = 8_000_000;
const QUIET_US: u64 = 2_000_000;

fn idle(ctx: &mut Ad106<Native>) -> Result<()> {
    ctx.with_platform(|p| {
        if p.display_wake.publication_started {
            return Err(Error::State);
        }
        for base in [gsp_disp::CORE_USER, raw::HORN_WINDOW_USER] {
            if p.read32(base + gsp_disp::PUT)? != 0 || p.read32(base + gsp_disp::GET)? != 0 {
                return Err(Error::State);
            }
        }
        Ok(())
    })
}

fn current(ctx: &mut Ad106<Native>, deadline: u64, revision: u32) -> Result<()> {
    if ctx.time_us()? >= deadline {
        return Err(Error::Timeout);
    }
    ctx.with_platform(|p| {
        let w = p.display_wake;
        if w.publication_started || w.connected != Some(true) || w.revision != revision {
            return Err(Error::State);
        }
        Ok(())
    })
}

fn drain<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    libos: Span,
    version: u32,
    deadline: u64,
    revision: u32,
) -> Result<()> {
    display_event(ctx, queue, libos, version, deadline)?;
    current(ctx, deadline, revision)
}

fn read_aux<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    libos: Span,
    version: u32,
    deadline: u64,
    plan: &Link,
    revision: u32,
    address: u32,
    data: &mut [u8],
) -> Result<()> {
    drain(ctx, queue, libos, version, deadline, revision)?;
    aux(
        &mut rm_api(ctx, queue, HANDLE_DISP_CLIENT, libos, version, deadline),
        plan.display_id,
        address,
        true,
        data,
    )?;
    drain(ctx, queue, libos, version, deadline, revision)
}

/// Successful replies may report a lost link. Only this observation, never an
/// RPC/AUX error, can contribute to recovery eligibility.
fn link_ok<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    libos: Span,
    version: u32,
    deadline: u64,
    plan: &Link,
    revision: u32,
) -> Result<bool> {
    let mut config = [0; 7];
    let mut status = [0; 6];
    read_aux(
        ctx,
        queue,
        libos,
        version,
        deadline,
        plan,
        revision,
        0x100,
        &mut config,
    )?;
    read_aux(
        ctx,
        queue,
        libos,
        version,
        deadline,
        plan,
        revision,
        0x202,
        &mut status,
    )?;
    let configured = config[0] == plan.rate
        && config[1] & 31 == plan.lanes
        && (config[1] & 0x80 != 0) == plan.enhanced
        && config[2] & 0x27 == 0;
    let equalized = plan.check_status(&status).is_ok();
    sel4::debug_println!(
        "NVIDIA_DISPLAY_PREACT_LINK clock_us={} revision={} config={:02x?} status={:02x?} configured={} equalized={} updates_submitted=false",
        ctx.time_us()?,
        revision,
        config,
        status,
        configured,
        equalized
    );
    if !configured || !equalized {
        ctx.with_platform(|p| {
            p.display_wake.link_invalid = true;
            Ok(())
        })?;
    }
    Ok(configured && equalized)
}

fn wait_reconnect<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    libos: Span,
    version: u32,
    deadline: u64,
    previous: u32,
) -> Result<u32> {
    let deadline = ctx
        .time_us()?
        .checked_add(RECONNECT_US)
        .ok_or(Error::Range)?
        .min(deadline);
    let w = ctx.with_platform(|p| Ok(p.display_wake))?;
    if !w.unplugged_since(previous)
        || !w.single_unplug_since_training()
        || w.recovery_used
        || w.publication_started
    {
        sel4::debug_println!(
            "NVIDIA_DISPLAY_RECOVERY_DENIED reason=no-new-unplug-or-budget previous={} revision={} recovery_used={} publication_started={}",
            previous,
            w.revision,
            w.recovery_used,
            w.publication_started
        );
        return Err(Error::State);
    }
    sel4::debug_println!(
        "NVIDIA_DISPLAY_RECOVERY_WAIT clock_us={} deadline_us={} previous={} updates_submitted=false",
        ctx.time_us()?,
        deadline,
        previous
    );
    loop {
        display_event(ctx, queue, libos, version, deadline)?;
        let now = ctx.time_us()?;
        if now >= deadline {
            return Err(Error::Timeout);
        }
        let w = ctx.with_platform(|p| Ok(p.display_wake))?;
        if !w.single_unplug_since_training() {
            return Err(Error::State);
        }
        if w.reconnected_since(previous) && w.quiet_us(now)? >= QUIET_US {
            return Ok(w.revision);
        }
        ctx.sleep_until(now.saturating_add(10_000).min(deadline))?;
    }
}

/// Read through RM after SOR acquisition. RAW EDID explicitly bypasses cache;
/// bufferSize is output-only in the SDK. Do not invent I2C-over-AUX sequences.
fn validate_sink<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    libos: Span,
    version: u32,
    deadline: u64,
    report: &crate::display::Report,
    plan: &Link,
    revision: u32,
) -> Result<[u8; 16]> {
    drain(ctx, queue, libos, version, deadline, revision)?;
    sink::connected(ctx, queue, libos, version, deadline, plan.display_id, false)?;
    drain(ctx, queue, libos, version, deadline, revision)?;
    let mut dpcd = [0; 16];
    read_aux(
        ctx, queue, libos, version, deadline, plan, revision, 0, &mut dpcd,
    )?;
    let timing = Timing::preferred(&report.monitor)?;
    timing.sink_link(&dpcd)?;
    // Keep the prebuilt mode/route/stream recipe, or fail closed. No fallback,
    // altered post-adjust policy, MST transition or alternate lane/rate plan.
    if dpcd != report.dpcd {
        return Err(Error::Unsupported);
    }
    let mut power = [0];
    read_aux(
        ctx, queue, libos, version, deadline, plan, revision, 0x600, &mut power,
    )?;
    if power[0] & 3 != 1 {
        return Err(Error::State);
    } // no additional wake write
    let reply = rm_api(ctx, queue, HANDLE_DISP_CLIENT, libos, version, deadline)
        .control::<api::DisplayEdid>(
            rm::HANDLE_DISP_COMMON,
            &raw::NV0073_CTRL_SPECIFIC_GET_EDID_V2_PARAMS {
                displayId: plan.display_id,
                flags: raw::HORN_EDID_RAW,
                ..Default::default()
            },
        )?;
    drain(ctx, queue, libos, version, deadline, revision)?;
    let length = reply.bufferSize as usize;
    if reply.subDeviceInstance != 0
        || reply.displayId != plan.display_id
        || reply.flags != raw::HORN_EDID_RAW
        || length < 128
        || length > reply.edidBuffer.len()
        || length != 128 * (usize::from(reply.edidBuffer[126]) + 1)
    {
        return Err(Error::Malformed);
    }
    let monitor = horn_nvidia_gsp::edid::parse(&reply.edidBuffer[..length])?;
    if Timing::preferred(&monitor)? != timing {
        return Err(Error::Unsupported);
    }
    sel4::debug_println!(
        "NVIDIA_DISPLAY_RECOVERY_SINK_VALIDATED clock_us={} revision={} edid_bytes={} preferred_unchanged=true dpcd_unchanged=true power={:#x} updates_submitted=false",
        ctx.time_us()?,
        revision,
        length,
        power[0]
    );
    Ok(dpcd)
}

pub(crate) fn prepare_link<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    libos: Span,
    version: u32,
    boot_deadline: u64,
    report: &crate::display::Report,
    plan: &Link,
) -> Result<()> {
    let deadline = ctx
        .time_us()?
        .checked_add(TOTAL_US)
        .ok_or(Error::Range)?
        .min(boot_deadline);
    idle(ctx)?;
    train(ctx, queue, libos, version, deadline, &report.dpcd, plan)?;
    let previous = ctx.with_platform(|p| p.display_wake.trained_revision.ok_or(Error::State))?;
    match link_probe::after_training(ctx, queue, libos, version, deadline, plan)? {
        Observation::Stable => {
            // Re-read after the observation, not only immediately after training.
            if !link_ok(ctx, queue, libos, version, deadline, plan, previous)? {
                return Err(Error::State);
            }
        }
        Observation::Invalidated => {
            idle(ctx)?;
            let revision = wait_reconnect(ctx, queue, libos, version, deadline, previous)?;
            let dpcd = validate_sink(ctx, queue, libos, version, deadline, report, plan, revision)?;
            if link_ok(ctx, queue, libos, version, deadline, plan, revision)? {
                // Conservative scope: do not reinterpret a changed but already
                // trained connection as the original snapshot, or train blindly.
                sel4::debug_println!(
                    "NVIDIA_DISPLAY_RECOVERY_DENIED reason=link-already-good revision={}",
                    revision
                );
                return Err(Error::State);
            }
            idle(ctx)?;
            current(ctx, deadline, revision)?;
            ctx.with_platform(|p| p.display_wake.begin_recovery(previous, revision))?;
            sel4::debug_println!(
                "NVIDIA_DISPLAY_RECOVERY_TRAIN_BEGIN clock_us={} previous={} revision={} allowance_consumed=true updates_submitted=false",
                ctx.time_us()?,
                previous,
                revision
            );
            // Same RM DP_CTRL as nouveau r535_dp_train(retrain=true), whose GSP
            // implementation ignores the bool and calls the same train_target.
            // No second attempt if this RPC, validation or observation fails.
            drain(ctx, queue, libos, version, deadline, revision)?;
            train(ctx, queue, libos, version, deadline, &dpcd, plan)?;
            if !matches!(
                link_probe::after_training(ctx, queue, libos, version, deadline, plan)?,
                Observation::Stable
            ) {
                return Err(Error::State);
            }
            if !link_ok(ctx, queue, libos, version, deadline, plan, revision)? {
                return Err(Error::State);
            }
            sel4::debug_println!(
                "NVIDIA_DISPLAY_RECOVERY_READY clock_us={} revision={} updates_submitted=false",
                ctx.time_us()?,
                revision
            );
        }
    }
    idle(ctx)?;
    let revision = ctx.with_platform(|p| p.display_wake.trained_revision.ok_or(Error::State))?;
    drain(ctx, queue, libos, version, deadline, revision)?;
    activation_ready(ctx)
}
