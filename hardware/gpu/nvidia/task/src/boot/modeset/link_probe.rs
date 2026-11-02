// SPDX-License-Identifier: GPL-2.0-only
//! Deferred, bounded link observations. Never issue nested RPCs from a callback,
//! never train, acknowledge IRQ vectors, change power, or publish UPDATE here.
use super::*;

const QUIET_US: u64 = 200_000;
const PROBE_BUDGET: u8 = 4;

pub(crate) fn pending<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    libos: Span,
    version: u32,
    boot_deadline: u64,
) -> Result<()> {
    let Some(plan) = ctx.with_platform(|p| Ok(p.display_link))? else {
        return Ok(());
    };
    let watch = ctx.with_platform(|p| Ok(p.display_wake))?;
    if !watch.probe_pending || watch.connected != Some(true) {
        return Ok(());
    }
    let deadline = ctx
        .time_us()?
        .checked_add(2_000_000)
        .ok_or(Error::Range)?
        .min(boot_deadline);
    // Let a replug/IRQ burst settle before AUX. This does not retry an RPC.
    loop {
        display_event(ctx, queue, libos, version, deadline)?;
        let now = ctx.time_us()?;
        let watch = ctx.with_platform(|p| Ok(p.display_wake))?;
        if watch.connected != Some(true) {
            return Ok(());
        }
        if watch.quiet_us(now)? >= QUIET_US {
            break;
        }
        if now >= deadline {
            return Err(Error::Timeout);
        }
        ctx.sleep_until(now.saturating_add(10_000).min(deadline))?;
    }
    let revision = ctx.with_platform(|p| {
        p.display_wake.probe_pending = false;
        if p.display_wake.probes >= PROBE_BUDGET {
            return Ok(None);
        }
        p.display_wake.probes += 1;
        Ok(Some(p.display_wake.revision))
    })?;
    let Some(revision) = revision else {
        sel4::debug_println!(
            "NVIDIA_DISPLAY_LINK_PROBE_SKIPPED reason=session-budget limit={} recovery=none",
            PROBE_BUDGET
        );
        return Ok(());
    };
    sel4::debug_println!(
        "NVIDIA_DISPLAY_LINK_PROBE_BEGIN clock_us={} revision={} recovery=none",
        ctx.time_us()?,
        revision
    );
    // A fresh full detection, not GET_ACTIVE and not a cached physical claim.
    sink::connected(ctx, queue, libos, version, deadline, plan.display_id, false)?;
    display_event(ctx, queue, libos, version, deadline)?;
    let watch = ctx.with_platform(|p| Ok(p.display_wake))?;
    if watch.connected != Some(true) || watch.revision != revision {
        sel4::debug_println!(
            "NVIDIA_DISPLAY_LINK_PROBE_INVALIDATED clock_us={} revision={} current={} aux_submitted=false",
            ctx.time_us()?,
            revision,
            watch.revision
        );
        return Ok(());
    }
    // The first optional post-commit AUX used to fail during unplug. Only
    // attempt this on a fresh connected generation; any RM error still stops.
    let mut config = [0; 7];
    let mut status = [0; 6];
    {
        let mut api = rm_api(ctx, queue, HANDLE_DISP_CLIENT, libos, version, deadline);
        aux(&mut api, plan.display_id, 0x100, true, &mut config)?;
    }
    display_event(ctx, queue, libos, version, deadline)?;
    let watch = ctx.with_platform(|p| Ok(p.display_wake))?;
    if watch.connected != Some(true) || watch.revision != revision {
        sel4::debug_println!(
            "NVIDIA_DISPLAY_LINK_PROBE_INVALIDATED clock_us={} between_aux=true",
            ctx.time_us()?
        );
        return Ok(());
    }
    aux(
        &mut rm_api(ctx, queue, HANDLE_DISP_CLIENT, libos, version, deadline),
        plan.display_id,
        0x202,
        true,
        &mut status,
    )?;
    display_event(ctx, queue, libos, version, deadline)?;
    let current = ctx.with_platform(|p| Ok(p.display_wake))?;
    let current_generation = current.connected == Some(true) && current.revision == revision;
    let configured = config[0] == plan.rate
        && config[1] & 31 == plan.lanes
        && (config[1] & 0x80 != 0) == plan.enhanced
        && config[2] & 0x27 == 0;
    let equalized = plan.check_status(&status).is_ok();
    if current_generation && (!configured || !equalized) {
        // Sticky within this epoch: a later quiet plug cannot rehabilitate the
        // original trained snapshot. Observation can continue, activation cannot.
        ctx.with_platform(|p| {
            p.display_wake.link_invalid = true;
            Ok(())
        })?;
    }
    sel4::debug_println!(
        "NVIDIA_DISPLAY_LINK_PROBE_RESULT clock_us={} revision={} current_generation={} config={:02x?} status={:02x?} configured={} equalized={} retrain_candidate={} recovery=none",
        ctx.time_us()?,
        revision,
        current_generation,
        config,
        status,
        configured,
        equalized,
        current_generation && (!configured || !equalized)
    );
    // Valid replies describing a broken link are observations, not malformed
    // transport and not authorization to recover this active DMA epoch.
    Ok(())
}

pub(crate) fn after_training<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    libos: Span,
    version: u32,
    boot_deadline: u64,
    plan: &Link,
) -> Result<()> {
    let start = ctx.time_us()?;
    let end = start.checked_add(3_000_000).ok_or(Error::Range)?;
    if end >= boot_deadline {
        return Err(Error::Timeout);
    }
    let deadline = end
        .checked_add(2_000_000)
        .ok_or(Error::Range)?
        .min(boot_deadline);
    let revision = ctx.with_platform(|p| Ok(p.display_wake.revision))?;
    sel4::debug_println!(
        "NVIDIA_DISPLAY_TRAIN_ONLY_BEGIN clock_us={} until_us={} revision={} updates_submitted=false",
        start,
        end,
        revision
    );
    stream_snapshot(ctx, deadline, "trained-no-update")?;
    loop {
        display_event(ctx, queue, libos, version, deadline)?;
        pending(ctx, queue, libos, version, deadline)?;
        let now = ctx.time_us()?;
        if now >= end {
            break;
        }
        ctx.sleep_until(now.saturating_add(10_000).min(end))?;
    }
    let watch = ctx.with_platform(|p| Ok(p.display_wake))?;
    sel4::debug_println!(
        "NVIDIA_DISPLAY_TRAIN_ONLY_END clock_us={} elapsed_us={} revision={} unchanged={} updates_submitted=false",
        ctx.time_us()?,
        ctx.time_us()?.saturating_sub(start),
        watch.revision,
        watch.revision == revision
    );
    if watch.revision != revision {
        return Err(Error::State);
    }
    sink::ensure_settled(ctx, plan)
}
