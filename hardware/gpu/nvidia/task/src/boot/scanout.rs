// SPDX-License-Identifier: GPL-2.0-only
//! Prepare one bounded, linear VRAM surface and window 0 assembly.
//! Preload assembly and final UPDATE, but initially publish assembly only.
use super::{
    Ad106, DISP_PB_BYTES, DISP_PB_SIZE, DeviceAccess, Error, HANDLE_DISP_CLIENT, Memory, Native,
    Queue, Result, Rm, Span, Vec, api, display_event, display_memory, display_pushbuffer, gsp_disp,
    raw, rm, rm_api,
};
use gsp_disp::{commands, pramin::Region, surface, timing::Timing};
use horn_nvidia_gsp::falcon::Platform;

const ALLOCATION_HANDLE: u32 = 0x0042_0000;
pub(super) const IMAGE_HANDLE: u32 = 0x2000;
pub(super) struct Surface {
    pub base: u64,
    pub bytes: usize,
}

pub(super) fn prepare<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    libos: Span,
    version: u32,
    deadline: u64,
    timing: &Timing,
) -> Result<Surface> {
    let params = surface::request(HANDLE_DISP_CLIENT, timing)?;
    let reply = display_memory(
        &mut rm_api(ctx, queue, HANDLE_DISP_CLIENT, libos, version, deadline),
        ALLOCATION_HANDLE,
        &params,
    )?;
    surface::validate_allocation(timing, &reply).map_err(|error| {
        sel4::debug_println!("NVIDIA_DISPLAY_SCANOUT_REJECTED error={:?} requested_bytes={} size={:#x} offset={:#x} limit={:#x} kind={:#x} attr={:#x} attr_mask={:#x} expected_attr={:#x}",
            error, params.size, reply.size, reply.offset, reply.limit, reply.format,
            reply.attr, raw::HORN_SURFACE_ATTR_MASK, raw::HORN_SURFACE_ATTR);
        error
    })?;
    let bytes = usize::try_from(reply.size).map_err(|_| Error::Range)?;
    let region = Region::new(reply.offset, reply.size)?;
    // Validate descriptor geometry before the first write; no allocation release on failure.
    gsp_disp::instance::vram_descriptor(reply.offset, bytes)?;
    let mut line = Vec::new();
    line.try_reserve_exact(timing.pitch as usize / 4)
        .map_err(|_| Error::Exhausted)?;
    line.resize(timing.pitch as usize / 4, 0);
    sel4::debug_println!(
        "NVIDIA_DISPLAY_SCANOUT_BEGIN vram={:#x} bytes={} width={} height={} pitch={} format=argb8888 target=linear-vram",
        reply.offset,
        bytes,
        timing.width,
        timing.height,
        timing.pitch
    );
    let start = ctx.time_us()?;
    for y in 0..timing.height {
        surface::row(timing, y, &mut line)?;
        ctx.with_platform(|p| {
            region.write(p, u64::from(y) * u64::from(timing.pitch), &line, deadline)
        })
        .map_err(|error| {
            sel4::debug_println!("NVIDIA_DISPLAY_SCANOUT_FAILED row={} error={:?}", y, error);
            error
        })?;
        display_event(ctx, queue, libos, version, deadline)?;
        if (y + 1) % 128 == 0 {
            sel4::debug_println!(
                "NVIDIA_DISPLAY_SCANOUT_PROGRESS rows={}/{} elapsed_us={}",
                y + 1,
                timing.height,
                ctx.time_us()?.saturating_sub(start)
            );
        }
    }
    // RM may round above the frame. Initialize *all* descriptor-visible backing.
    line.fill(0);
    let mut offset = timing.bytes;
    while offset < bytes {
        let words = ((bytes - offset) / 4).min(line.len());
        ctx.with_platform(|p| region.write(p, offset as u64, &line[..words], deadline))?;
        offset += words * 4;
        display_event(ctx, queue, libos, version, deadline)?;
    }
    sel4::debug_println!(
        "NVIDIA_DISPLAY_SCANOUT_READY vram={:#x} bytes={} pattern=bars-checker-border readback=verified scanout=not-enabled",
        reply.offset,
        bytes
    );
    Ok(Surface {
        base: reply.offset,
        bytes,
    })
}

pub(super) struct Window {
    pub(super) program: commands::Push,
    pub(super) assembly_put: u32,
    _pb: super::DmaBlock,
}

pub(super) fn window<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    rm: &Rm,
    libos: Span,
    version: u32,
    boot_deadline: u64,
    timing: &Timing,
) -> Result<Window> {
    let mut program = commands::window(timing, IMAGE_HANDLE)?;
    let assembly_put = program.put();
    gsp_disp::head::append_window(&mut program, super::DISP_NOTIFIER_HANDLE)?;
    let mut pb = ctx.with_platform(|p| p.allocate(DISP_PB_BYTES as usize, DeviceAccess::Read))?;
    // Entire ring preloaded once, before RM is given its IOVA. No recycling.
    unsafe {
        pb.zero()?;
        for (i, &word) in program.words().iter().enumerate() {
            pb.write32(i * 4, word)?;
        }
    }
    let class = raw::NVC67E_WINDOW_CHANNEL_DMA;
    let handle = rm::disp_chan_handle(class, 0);
    {
        let mut api = rm_api(
            ctx,
            queue,
            HANDLE_DISP_CLIENT,
            libos,
            version,
            boot_deadline,
        );
        display_pushbuffer(&mut api, rm, pb.iova, class)?;
        let reply = api.alloc::<api::DisplayWindow>(
            rm::HANDLE_DISP_ROOT,
            handle,
            &raw::NV50VAIO_CHANNELDMA_ALLOCATION_PARAMETERS {
                channelPBSize: DISP_PB_SIZE,
                subDeviceId: 1,
                ..Default::default()
            },
        )?;
        if reply.offset != 0 || reply.channelPBSize != DISP_PB_SIZE || reply.subDeviceId != 1 {
            return Err(Error::State);
        }
        sel4::debug_println!(
            "NVIDIA_DISPLAY_WINDOW_ALLOC_OK window=0 head=0 handle={:#x} pb={:#x} bytes={}",
            handle,
            pb.iova,
            pb.bytes
        );
    }
    Ok(Window {
        program,
        assembly_put,
        _pb: pb,
    })
}

pub(super) fn submit_window<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    libos: Span,
    version: u32,
    boot_deadline: u64,
    window: &Window,
) -> Result<()> {
    let deadline = ctx
        .time_us()?
        .checked_add(2_000_000)
        .ok_or(Error::Range)?
        .min(boot_deadline);
    let read = |p: &mut Native| -> Result<(u32, u32)> {
        Ok((
            p.read32(raw::HORN_WINDOW_USER + gsp_disp::PUT)?,
            p.read32(raw::HORN_WINDOW_USER + gsp_disp::GET)?,
        ))
    };
    let (put, get) = ctx.with_platform(read)?;
    if put != 0 || get != 0 {
        return Err(Error::State);
    }
    let mut previous = 0;
    for (kind, target) in [("nop", 4), ("assembly", window.assembly_put)] {
        if ctx.time_us()? >= deadline {
            return Err(Error::Timeout);
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        let value = gsp_disp::put_word(target)?;
        ctx.with_platform(|p| p.write32(raw::HORN_WINDOW_USER + gsp_disp::PUT, value))?;
        sel4::debug_println!(
            "NVIDIA_DISPLAY_WINDOW_SUBMIT kind={} put={} put_raw={:#x}",
            kind,
            target,
            value
        );
        loop {
            display_event(ctx, queue, libos, version, deadline)?;
            let (put, get_raw) = ctx.with_platform(read)?;
            let get = gsp_disp::get_dwords(get_raw);
            let now = ctx.time_us()?;
            if put != value || get_raw == u32::MAX || get < previous || get > target {
                return Err(Error::State);
            }
            if now >= deadline {
                sel4::debug_println!(
                    "NVIDIA_DISPLAY_WINDOW_TIMEOUT kind={} put={} get={} get_raw={:#x}",
                    kind,
                    target,
                    get,
                    get_raw
                );
                return Err(Error::Timeout);
            }
            if get == target {
                sel4::debug_println!(
                    "NVIDIA_DISPLAY_WINDOW_FETCHED kind={} put={} get={} update=not-submitted",
                    kind,
                    target,
                    get
                );
                previous = get;
                break;
            }
            ctx.sleep_until(now.saturating_add(1000).min(deadline))?;
        }
    }
    sel4::debug_println!(
        "NVIDIA_DISPLAY_PIPELINE_PREPARED head=0 window=0 image_handle={:#x} dp=untrained modeset=not-submitted",
        IMAGE_HANDLE
    );
    Ok(())
}
