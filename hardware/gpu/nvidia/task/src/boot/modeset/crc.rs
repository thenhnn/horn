// SPDX-License-Identifier: GPL-2.0-only
//! Bounded one-shot SF CRC. Fresh owned VRAM/context, prebuilt detach, no flip,
//! ring rewrite, context reuse or implicit free (even when DONE is observed).
use super::*;
use gsp_disp::{crc as layout, pramin::Region};

pub(crate) fn prepare<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    libos: Span,
    version: u32,
    deadline: u64,
) -> Result<scanout::Surface> {
    // Match nouveau nv50_crc_ctx_init's VRAM placement and RW context DMA.
    let params = rm::vram_request(HANDLE_DISP_CLIENT, layout::BYTES as u64, 0x10000)?;
    let reply = display_memory(
        &mut rm_api(ctx, queue, HANDLE_DISP_CLIENT, libos, version, deadline),
        0x0044_0000,
        &params,
    )?;
    gsp_disp::surface::validate_memory(params.size, &reply)?;
    let region = Region::new(reply.offset, reply.size)?;
    let mut zero = Vec::new();
    zero.try_reserve_exact(0x10000 / 4)
        .map_err(|_| Error::Exhausted)?;
    zero.resize(0x10000 / 4, 0);
    for offset in (0..reply.size).step_by(0x10000) {
        let words = ((reply.size - offset) as usize / 4).min(zero.len());
        ctx.with_platform(|p| region.write(p, offset, &zero[..words], deadline))?;
        display_event(ctx, queue, libos, version, deadline)?;
    }
    ctx.with_platform(|p| {
        if p.display_crc.is_some() {
            return Err(Error::State);
        }
        p.display_crc = Some(Region::new(reply.offset, layout::BYTES as u64)?);
        Ok(())
    })?;
    sel4::debug_println!(
        "NVIDIA_DISPLAY_CRC_PREPARED vram={:#x} bytes={} descriptor_bytes={} handle={:#x} source=sf recycled=false",
        reply.offset,
        reply.size,
        layout::BYTES,
        layout::HANDLE
    );
    Ok(scanout::Surface {
        base: reply.offset,
        bytes: layout::BYTES,
    })
}

fn read(ctx: &mut Ad106<Native>, deadline: u64, offset: u64) -> Result<u32> {
    let region = ctx.with_platform(|p| p.display_crc.ok_or(Error::State))?;
    let mut word = [0];
    ctx.with_platform(|p| region.read(p, offset, &mut word, deadline))?;
    Ok(word[0])
}

pub(super) fn snapshot(
    ctx: &mut Ad106<Native>,
    deadline: u64,
    stage: &str,
    require_done: bool,
) -> Result<()> {
    let status = read(ctx, deadline, raw::NVC37D_NOTIFIER_CRC_STATUS_0 as u64 * 4)?;
    let count = layout::count(status)?;
    let done = status & raw::HORN_CRC_DONE != 0;
    let overflow = status & raw::HORN_CRC_OVERFLOW;
    sel4::debug_println!(
        "NVIDIA_DISPLAY_CRC_STATUS clock_us={} stage={} source=sf status={:#x} count={} done={} overflow={:#x} provisional={} visible=operator-unconfirmed",
        ctx.time_us()?,
        stage,
        status,
        count,
        done,
        overflow,
        !done
    );
    if require_done && !done {
        return Err(Error::State);
    }
    // First entries can be inspected while capture is live, as nouveau does.
    // Zero does not establish missing pixels; it may mean not written yet.
    let limit = if done { count.min(4) } else { 4 };
    for index in 0..limit {
        let mut fields = [0; 3];
        for (field, value) in fields.iter_mut().enumerate() {
            *value = read(ctx, deadline, layout::field_offset(index, field)?)?;
        }
        sel4::debug_println!(
            "NVIDIA_DISPLAY_CRC_ENTRY stage={} index={} compositor={:#x} rg={:#x} sf={:#x} provisional={} expected_crc=unknown",
            stage,
            index,
            fields[0],
            fields[1],
            fields[2],
            !done
        );
    }
    if done && count > 4 {
        // The first frames may precede window publication. Also record a small
        // tail to distinguish settled capture from initial transitional frames.
        for index in count.saturating_sub(4).max(4)..count {
            let sf = read(ctx, deadline, layout::field_offset(index, 2)?)?;
            sel4::debug_println!(
                "NVIDIA_DISPLAY_CRC_LAST stage={} index={} sf={:#x} expected_crc=unknown",
                stage,
                index,
                sf
            );
        }
    }
    Ok(())
}

pub(super) fn finish<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    libos: Span,
    version: u32,
    boot_deadline: u64,
    notifier: &DmaBlock,
    core: &gsp_disp::commands::Core,
) -> Result<()> {
    let deadline = ctx
        .time_us()?
        .checked_add(200_000)
        .ok_or(Error::Range)?
        .min(boot_deadline);
    if core.capture_stop_put != core.push.put() || core.capture_stop_put <= core.modeset_put {
        return Err(Error::State);
    }
    ctx.with_platform(|p| {
        let expected = gsp_disp::put_word(core.modeset_put)?;
        if p.read32(gsp_disp::CORE_USER + gsp_disp::GET)? != expected
            || p.read32(gsp_disp::CORE_USER + gsp_disp::PUT)? != expected
        {
            return Err(Error::State);
        }
        Ok(())
    })?;
    core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
    ctx.with_platform(|p| {
        p.write32(
            gsp_disp::CORE_USER + gsp_disp::PUT,
            gsp_disp::put_word(core.capture_stop_put)?,
        )
    })?;
    sel4::debug_println!(
        "NVIDIA_DISPLAY_CRC_DETACH_SUBMIT clock_us={} put={} notifier_offset={} modeset_retry=false",
        ctx.time_us()?,
        core.capture_stop_put,
        layout::STOP_NOTIFIER
    );
    loop {
        display_event(ctx, queue, libos, version, deadline)?;
        let fetched = ctx.with_platform(|p| {
            let put = p.read32(gsp_disp::CORE_USER + gsp_disp::PUT)?;
            let get = p.read32(gsp_disp::CORE_USER + gsp_disp::GET)?;
            let words = gsp_disp::get_dwords(get);
            if put != gsp_disp::put_word(core.capture_stop_put)?
                || get == u32::MAX
                || words < core.modeset_put
                || words > core.capture_stop_put
            {
                return Err(Error::State);
            }
            Ok(words == core.capture_stop_put)
        })?;
        let complete =
            gsp_disp::notifier_finished(unsafe { notifier.read32(layout::STOP_NOTIFIER)? })?;
        let status = read(ctx, deadline, raw::NVC37D_NOTIFIER_CRC_STATUS_0 as u64 * 4)?;
        if fetched && complete && status & raw::HORN_CRC_DONE != 0 {
            snapshot(ctx, deadline, "detached", true)?;
            sel4::debug_println!(
                "NVIDIA_DISPLAY_CRC_DETACHED clock_us={} backing=retained reused=false",
                ctx.time_us()?
            );
            return Ok(());
        }
        let now = ctx.time_us()?;
        if now >= deadline {
            return Err(Error::Timeout);
        }
        ctx.sleep_until(now.saturating_add(1000).min(deadline))?;
    }
}
