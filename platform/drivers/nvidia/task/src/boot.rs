// SPDX-License-Identifier: GPL-2.0-only
//! Executable AD106 cold boot. No host driver/reset operations.
use crate::{
    Reader,
    native::{DmaBlock, Native},
    p,
    shared::Shared,
    time::Time,
};
use alloc::{vec, vec::Vec};
use horn_devmgr::protocol::*;
use horn_driver::{DeviceAccess, DeviceSession};
use horn_ipc::*;
use horn_nvidia_gsp::{
    Error, Result,
    boot::{self, PAGE, Span},
    falcon::{Ad106, Handoff, PinnedSource, Platform},
    firmware::Bundle,
    queue::{Memory, Queue},
    vbios::{Command, Fwsec},
    wire,
};
mod images {
    include!(concat!(env!("OUT_DIR"), "/images.rs"));
}
fn span(block: &DmaBlock) -> Result<Span> {
    Span::new(block.iova, block.bytes)
}
fn stage(ctx: &mut Ad106<Native>, data: &[u8], access: DeviceAccess) -> Result<DmaBlock> {
    let mut block = ctx.with_platform(|p| p.allocate(data.len(), access))?;
    // The fresh mapping is not published; Read grants are immutable after copy.
    unsafe {
        block.initialize(0, data)?;
    }
    Ok(block)
}
fn send<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    function: u32,
    payload: &[u8],
    deadline: u64,
) -> Result<()> {
    loop {
        let now = ctx.time_us()?;
        if now >= deadline {
            return Err(Error::Timeout);
        }
        if let Some(_publication) = queue.try_send(function, 0, [payload, &[]])? {
            ctx.notify_queue()?;
            return Ok(());
        }
        ctx.sleep_until((now + 1000).min(deadline))?;
    }
}
fn wait<M: Memory>(
    ctx: &mut Ad106<Native>,
    queue: &mut Queue<M>,
    function: u32,
    libos: Span,
    version: u32,
    deadline: u64,
) -> Result<Vec<u8>> {
    // Function correlation only: r570 does not echo request sequence on every
    // reply. There is one outstanding request and a fresh queue per boot epoch.
    for _ in 0..32768 {
        let now = ctx.time_us()?;
        if now >= deadline {
            return Err(Error::Timeout);
        }
        if let Some(bytes) = queue.try_receive()? {
            let packet = wire::Packet::decode(&bytes)?;
            if packet.header.function == function {
                return Ok(bytes);
            }
            if packet.header.function == wire::CPU_SEQUENCE {
                sel4::debug_println!(
                    "NVIDIA_GSP_SEQUENCE bytes={} rpc_sequence={}",
                    packet.payload.len(),
                    packet.header.rpc_sequence
                );
                unsafe {
                    ctx.run_sequence(packet.payload, libos, version, deadline)?;
                }
            } else if wire::fatal_event(packet.header.function) {
                return Err(Error::FirmwareStatus(packet.header.function));
            } else if packet.header.function < 4096 {
                packet.success()?;
            }
        } else {
            ctx.sleep_until((now + 1000).min(deadline))?;
        }
    }
    Err(Error::Exhausted)
}
fn bar(config: &[u32; 64], index: usize) -> Result<u64> {
    let at = 4 + index;
    let low = config[at];
    if low & 1 != 0 {
        return Err(Error::Unsupported);
    }
    let high = if low & 6 == 4 {
        *config.get(at + 1).ok_or(Error::Range)? as u64
    } else {
        0
    };
    Ok((high << 32) | u64::from(low & !15))
}
pub fn run(session: DeviceSession, time: Time, resources: PciResources) -> ! {
    let mut ctx = Ad106::new(Native::new(session, time));
    let result = (|| -> Result<()> {
        let deadline = ctx.time_us()?.checked_add(28_000_000).ok_or(Error::Range)?;
        sel4::debug_println!("NVIDIA_GSP_BOOT_BEGIN abi=570.144 dma_limit=67108864");
        ctx.probe(deadline)?;
        let prom = ctx.with_platform(Native::prom)?;
        let fwsec = Fwsec::from_prom(&prom)?;
        let bundle = Bundle::parse(
            images::META,
            images::IMAGE,
            images::BOOTLOADER,
            images::LOAD,
            images::UNLOAD,
        )?;
        let fb = ctx.framebuffer()?;
        sel4::debug_println!(
            "NVIDIA_GSP_LAYOUT fb={} frts={:#x} wpr={:#x}",
            fb.bytes(),
            fb.frts().start,
            fb.wpr().start
        );
        let fuse = ctx.signature_fuse(fwsec.engine_mask, fwsec.ucode_id)?;
        let mut frts_cpu = vec![0; fwsec.ucode.len()];
        let mut sb_cpu = vec![0; fwsec.ucode.len()];
        let range = fb.frts();
        let frts = fwsec.prepare(
            fuse,
            Command::Frts {
                address: range.start,
                bytes: range.end - range.start,
            },
            &mut frts_cpu,
        )?;
        let sb = fwsec.prepare(fuse, Command::Sb, &mut sb_cpu)?;
        let load_fuse = ctx.signature_fuse(bundle.loader.engine_mask, bundle.loader.ucode_id)?;
        let unload_fuse =
            ctx.signature_fuse(bundle.unloader.engine_mask, bundle.unloader.ucode_id)?;
        let mut load_cpu = vec![0; bundle.loader.blob.len()];
        let mut unload_cpu = vec![0; bundle.unloader.blob.len()];
        let loader = bundle.loader.prepare(load_fuse, &mut load_cpu)?;
        let unloader = bundle.unloader.prepare(unload_fuse, &mut unload_cpu)?;
        // All four signed sources, including cleanup, exist before first reset.
        let frts_dma = stage(&mut ctx, frts.bytes(), DeviceAccess::Read)?;
        let sb_dma = stage(&mut ctx, sb.bytes(), DeviceAccess::Read)?;
        let load_dma = stage(&mut ctx, loader.bytes(), DeviceAccess::Read)?;
        let unload_dma = stage(&mut ctx, unloader.bytes(), DeviceAccess::Read)?;
        let frts_pin = unsafe { PinnedSource::from_grant(&frts, frts_dma.iova)? };
        let sb_pin = unsafe { PinnedSource::from_grant(&sb, sb_dma.iova)? };
        let load_pin = unsafe { PinnedSource::from_grant(&loader, load_dma.iova)? };
        let unload_pin = unsafe { PinnedSource::from_grant(&unloader, unload_dma.iova)? };
        sel4::debug_println!(
            "NVIDIA_GSP_STAGE image_bytes={} cleanup_prepared=true",
            bundle.image.len()
        );
        let image = stage(&mut ctx, bundle.image, DeviceAccess::Read)?;
        let mut radix_cpu = vec![0; boot::radix_bytes(bundle.image.len())?];
        let mut radix = ctx.with_platform(|p| p.allocate(radix_cpu.len(), DeviceAccess::Read))?;
        boot::radix3(span(&image)?, span(&radix)?, &mut radix_cpu)?;
        unsafe {
            radix.initialize(0, &radix_cpu)?;
        }
        let bootloader = stage(&mut ctx, bundle.bootloader.blob, DeviceAccess::Read)?;
        let signatures = stage(&mut ctx, bundle.signatures, DeviceAccess::Read)?;
        let mut page = [0; PAGE];
        fb.wpr_metadata(
            &bundle,
            span(&radix)?,
            span(&bootloader)?,
            span(&signatures)?,
            &mut page,
        )?;
        let wpr_meta = stage(&mut ctx, &page, DeviceAccess::Read)?;
        let mut queue_cpu = vec![0; boot::QUEUE_BYTES];
        let mut queue_dma =
            ctx.with_platform(|p| p.allocate(boot::QUEUE_BYTES, DeviceAccess::ReadWrite))?;
        boot::queues(span(&queue_dma)?, &mut queue_cpu)?;
        unsafe {
            queue_dma.initialize(0, &queue_cpu)?;
        }
        boot::rmargs(span(&queue_dma)?, &mut page)?;
        let rmargs = stage(&mut ctx, &page, DeviceAccess::Read)?;
        let mut logs = Vec::new();
        for _ in 0..3 {
            let mut block =
                ctx.with_platform(|p| p.allocate(boot::LOG_BYTES, DeviceAccess::ReadWrite))?;
            let mut bytes = vec![0; boot::LOG_BYTES];
            boot::log_buffer(span(&block)?, &mut bytes)?;
            unsafe {
                block.initialize(0, &bytes)?;
            }
            logs.push(block);
        }
        boot::libos(
            [span(&logs[0])?, span(&logs[1])?, span(&logs[2])?],
            span(&rmargs)?,
            &mut page,
        )?;
        let libos = stage(&mut ctx, &page, DeviceAccess::Read)?;
        let flush = ctx.with_platform(|p| p.allocate(PAGE, DeviceAccess::ReadWrite))?;
        let inputs = Handoff {
            framebuffer: &fb,
            flush: span(&flush)?,
            libos: span(&libos)?,
            wpr_meta: span(&wpr_meta)?,
            frts: &frts_pin,
            sb: &sb_pin,
            loader: &load_pin,
            unloader: &unload_pin,
            app_version: bundle.bootloader.app_version,
        };
        let mut queue = unsafe { Queue::attach(Shared::new(&mut queue_dma))? };
        ctx.with_platform(Native::bind_irq)?;
        let cfg = &resources.configuration.data;
        let bdf = (u64::from(resources.address.segment) << 16)
            | (u64::from(resources.address.bus) << 8)
            | (u64::from(resources.address.device) << 3)
            | u64::from(resources.address.function);
        let system = boot::system_info(
            [bar(cfg, 0)?, bar(cfg, 1)?, bar(cfg, 3)?],
            bdf,
            [cfg[0], cfg[11], cfg[2] & 255],
        )?;
        send(
            &mut ctx,
            &mut queue,
            wire::SET_SYSTEM_INFO,
            &system,
            deadline,
        )?;
        send(
            &mut ctx,
            &mut queue,
            wire::SET_REGISTRY,
            &boot::registry()?,
            deadline,
        )?;
        // Remapped IRQ and all mappings are committed; only now authorize DMA.
        ctx.with_platform(|p| unsafe { p.activate() })?;
        sel4::debug_println!("NVIDIA_GSP_HANDOFF_BEGIN bus_master=true");
        unsafe {
            ctx.handoff(&inputs, deadline)?;
        }
        sel4::debug_println!("NVIDIA_GSP_RISCV_ACTIVE init_done=false");
        let init = wait(
            &mut ctx,
            &mut queue,
            wire::INIT_DONE,
            inputs.libos,
            inputs.app_version,
            deadline,
        )?;
        wire::Packet::decode(&init)?.init_done()?;
        sel4::debug_println!("NVIDIA_GSP_INIT_DONE");
        send(
            &mut ctx,
            &mut queue,
            wire::STATIC_INFO,
            &vec![0; wire::STATIC_INFO_BYTES],
            deadline,
        )?;
        let reply = wait(
            &mut ctx,
            &mut queue,
            wire::STATIC_INFO,
            inputs.libos,
            inputs.app_version,
            deadline,
        )?;
        let packet = wire::Packet::decode(&reply)?;
        sel4::debug_println!(
            "NVIDIA_GSP_STATIC_INFO bytes={} status={:#x} observed_fb={}",
            packet.payload.len(),
            packet.header.status,
            fb.bytes()
        );
        let info = wire::StaticInfo::parse(&packet, fb.bytes())?;
        ctx.with_platform(Native::enable_gsp_events)?;
        // Clear-before-enable can leave old messages without a fresh interrupt.
        while let Some(bytes) = queue.try_receive()? {
            let p = wire::Packet::decode(&bytes)?;
            if wire::fatal_event(p.header.function) {
                return Err(Error::FirmwareStatus(p.header.function));
            }
        }
        let snapshot = p::ProbeSnapshot {
            boot0: ctx.with_platform(|p| p.read32(0))?,
            boot42: ctx.with_platform(|p| p.read32(0xa00))?,
            gfw_state: p::GfwState::Complete,
            gfw_progress: 255,
            bus_master: true,
            gsp_booted: true,
            vbios_state: p::VbiosState::FwsecParsed,
            fwsec_bytes: fwsec.ucode.len() as u32,
            fwsec_engines: fwsec.engine_mask as u32,
            fwsec_ucode: fwsec.ucode_id as u32,
            fwsec_signature_versions: fwsec.signature_versions as u32,
        };
        sel4::debug_println!(
            "NVIDIA_GSP_READY name={} fb={} regions={} transport_sequence={} rpc_sequence={}",
            info.name,
            info.framebuffer_bytes,
            info.regions,
            packet.header.transport_sequence,
            packet.header.rpc_sequence
        );
        horn_runtime::notify_ready(DEVICE_DRIVER_CAPS_READY_SLOT);
        let endpoint = sel4::cap::Endpoint::from_bits(DEVICE_DRIVER_CAPS_SERVICE_SLOT as _);
        let reply_cap = sel4::cap::Reply::from_bits(DEVICE_DRIVER_CAPS_REPLY_SLOT as _);
        let receive = sel4::cap::CNode::from_bits(DEVICE_DRIVER_CAPS_SELF_SLOT as _)
            .absolute_cptr(sel4::CPtr::from_bits(crate::SLOTS.public_receive() as _));
        loop {
            sel4::with_ipc_buffer_mut(|ipc| ipc.set_recv_slot(&receive));
            let (message, badge) = endpoint.recv(reply_cap);
            if badge & horn_supervisor::STOP != 0 {
                let stop_deadline = ctx.time_us()?.checked_add(800_000).ok_or(Error::Range)?;
                ctx.with_platform(Native::disable_gsp_events)?;
                let mut shutdown = [0u8; 8]; // PowerStateLevel0, bInPMTransition=false.
                shutdown[4..].copy_from_slice(&0u32.to_le_bytes());
                send(
                    &mut ctx,
                    &mut queue,
                    wire::UNLOADING,
                    &shutdown,
                    stop_deadline,
                )?;
                let bytes = wait(
                    &mut ctx,
                    &mut queue,
                    wire::UNLOADING,
                    inputs.libos,
                    inputs.app_version,
                    stop_deadline,
                )?;
                wire::Packet::decode(&bytes)?.success()?;
                ctx.wait_suspended(stop_deadline)?;
                unsafe {
                    ctx.unload(&inputs, stop_deadline)?;
                }
                drop(queue);
                ctx.with_platform(Native::close)?;
                sel4::cap::Notification::from_bits(DEVICE_DRIVER_CAPS_STOPPED_SLOT as _).signal();
                loop {
                    sel4::cap::Notification::from_bits(DEVICE_DRIVER_CAPS_WAKE_SLOT as _).wait();
                }
            }
            if badge & horn_supervisor::IRQ != 0 {
                ctx.with_platform(Native::service_gsp_event)?;
                while let Some(bytes) = queue.try_receive()? {
                    let packet = wire::Packet::decode(&bytes)?;
                    if wire::fatal_event(packet.header.function) {
                        return Err(Error::FirmwareStatus(packet.header.function));
                    }
                }
            } else if badge == crate::CLIENT {
                ctx.with_platform(|native| {
                    let request = {
                        let root = sel4::cap::CNode::from_bits(DEVICE_DRIVER_CAPS_SELF_SLOT as _);
                        let mut server =
                            Sel4Server::new(root, endpoint, receive, &mut native.transport().space);
                        server.decode_received_dynamic(
                            message,
                            badge,
                            reply_cap,
                            p::nvidia_probe_object_request_layout,
                            p::NvidiaProbeObjectRequest::decode,
                        )
                    };
                    if let Ok((_, request, reply)) = request {
                        let response = p::nvidia_probe_object_dispatch(
                            request,
                            &mut Reader {
                                snapshot: &snapshot,
                                stopped: false,
                            },
                        );
                        p::nvidia_probe_object_reply(
                            reply,
                            &mut native.transport().space,
                            &response,
                        )
                        .map_err(|_| Error::Backend)?;
                    }
                    Ok(())
                })?;
            }
            receive.delete().map_err(|_| Error::Backend)?;
        }
    })();
    let error = result.err().unwrap_or(Error::State);
    let _ = ctx.quarantine(error);
    sel4::debug_println!(
        "NVIDIA_GSP_FAILED error={:?} diagnostic={:?} dma_retained=true",
        error,
        ctx.diagnostic()
    );
    // Panic aborts this TCB; supervisor removes publication and devmgr must
    // acknowledge SID isolation/drain before process memory can be reclaimed.
    panic!("AD106 GSP boot/runtime failed; no reset/retry");
}
