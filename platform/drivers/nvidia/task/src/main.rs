// SPDX-License-Identifier: BSD-2-Clause
//! Supervised AD106 driver; optional pinned native GSP cold boot.
#![no_std]
#![no_main]
extern crate alloc;
#[cfg(gsp_boot)]
mod boot;
#[allow(dead_code)] // Optional helpers also compile in the read-only build.
mod native;
mod prom;
#[allow(dead_code)] // Optional live queue adapter, unused in read-only mode.
mod shared;
mod time;

use core::ptr::NonNull;
use horn_devmgr::{interrupt, protocol::*};
use horn_driver::DeviceSession;
use horn_ipc::*;
use horn_nvidia::{
    Error,
    probe::{Ad106, GfwStatus, PALIT_RTX_4060_TI, PciIdentity, ReadBar0},
};
#[allow(dead_code)] // Generated client + server surface; this task uses the server half.
mod p {
    include!(concat!(env!("OUT_DIR"), "/protocol.rs"));
}
const CLIENT: u64 = 16;
const SLOTS: horn_runtime::TaskSlots = horn_runtime::TaskSlots::new(
    DEVICE_DRIVER_CAPS_FIRST_FREE_SLOT,
    DEVICE_DRIVER_CAPS_SIZE_BITS,
);
horn_runtime::task!(main, vm: horn_runtime::VmBootstrap {
    cnode: DEVICE_DRIVER_CAPS_SELF_SLOT,
    factory: DEVICE_DRIVER_CAPS_VM_SLOT,
    address_space: DEVICE_DRIVER_CAPS_ADDRESS_SPACE_SLOT,
    slots: SLOTS,
});
fn target<T: CapType>(slot: u32) -> Cap<T> {
    horn_runtime::bootstrap_cap(
        sel4::cap::CNode::from_bits(DEVICE_DRIVER_CAPS_SELF_SLOT as _),
        slot,
    )
}

struct SnapshotConfig<'a>(&'a [u32; 64]);
impl interrupt::Config for SnapshotConfig<'_> {
    fn read32(&mut self, offset: u16) -> u32 {
        self.0[offset as usize / 4]
    }
    fn write(&mut self, _: u16, _: u32, _: u32) {
        panic!("immutable PCI snapshot");
    }
}
/// Two UC grants, not a fabricated contiguous 16-MiB CPU mapping.
/// Used only during initial probe, while the session retains both leases.
struct ProbeBar0 {
    boot: NonNull<u8>,
    gfw: NonNull<u8>,
    bytes: usize,
}
impl ReadBar0 for ProbeBar0 {
    fn length(&self) -> usize {
        self.bytes
    }
    fn read32(&mut self, offset: usize) -> Result<u32, Error> {
        if offset & 3 != 0 {
            return Err(Error::InvalidRange);
        }
        let (pointer, within) = if offset < 4096 {
            (self.boot, offset)
        } else if (0x118000..0x119000).contains(&offset) {
            (self.gfw, offset - 0x118000)
        } else {
            return Err(Error::InvalidRange);
        };
        // SAFETY: these two grants remain UC-mapped in the live session. All
        // accesses are serialized on this TCB, before entering the STOP loop.
        Ok(u32::from_le(unsafe {
            pointer.as_ptr().add(within).cast::<u32>().read_volatile()
        }))
    }
}
struct Reader<'a> {
    snapshot: &'a p::ProbeSnapshot,
    stopped: bool,
}
impl p::NvidiaProbe for Reader<'_> {
    fn snapshot(&mut self) -> Result<p::ProbeSnapshot, p::ProbeError> {
        if self.stopped {
            Err(p::ProbeError::Stopped)
        } else {
            Ok(self.snapshot.clone())
        }
    }
}
#[allow(unreachable_code)]
fn main() -> ! {
    let root = sel4::cap::CNode::from_bits(DEVICE_DRIVER_CAPS_SELF_SLOT as _);
    let transport = horn_runtime::transport(root, SLOTS.application()).expect("NVIDIA transport");
    let mut session = DeviceSession::new(
        transport,
        target(DEVICE_DRIVER_CAPS_VM_SLOT),
        target(DEVICE_DRIVER_CAPS_ADDRESS_SPACE_SLOT),
        target(DEVICE_DRIVER_CAPS_DEVICE_SLOT),
    );
    let resources = session
        .control()
        .resources()
        .expect("assigned NVIDIA resources");
    let config = &resources.configuration.data;
    let actual = PciIdentity {
        vendor: config[0] as u16,
        device: (config[0] >> 16) as u16,
        subsystem_vendor: config[11] as u16,
        subsystem_device: (config[11] >> 16) as u16,
    };
    // Check the exact board BEFORE granting/touching any MMIO.
    assert_eq!(actual, PALIT_RTX_4060_TI.0, "unexpected NVIDIA board");
    assert_eq!(config[1] & 4, 0, "probe must not enable bus mastering");
    let caps = interrupt::Capabilities::discover(&mut SnapshotConfig(config))
        .expect("PCI interrupt layout");
    let msi = caps.msi.expect("target board single MSI");
    assert!(caps.msix.is_none(), "unexpected MSI-X board");
    assert_eq!(
        (config[msi.offset as usize / 4] >> 16) & 0x1ff,
        0x80,
        "expected disabled, 64-bit single MSI without vector masking"
    );
    let bytes = usize::try_from(resources.bar_lengths.data[0]).expect("BAR0 size");
    assert_eq!(bytes, horn_nvidia::probe::BAR0_BYTES, "unexpected BAR0");
    let mut time = time::Time::new(&mut session).expect("named clock/alarm");
    let now = time.now(&mut session).expect("monotonic time");
    time.sleep_until(&mut session, now.checked_add(100).expect("alarm deadline"))
        .expect("monotonic alarm");
    sel4::debug_println!("NVIDIA_CLOCK source=timerd alarm=one-shot gsp_booted=false");
    sel4::debug_println!(
        "NVIDIA_PCI bdf={:02x}:{:02x}.{} board=10de:2803/1569:f306 bar0={} msi_cap={:#x}",
        resources.address.bus,
        resources.address.device,
        resources.address.function,
        bytes,
        msi.offset
    );
    #[cfg(gsp_boot)]
    boot::run(session, time, resources);
    let boot_window = session.mmio_window(0, 0, 4096);
    let gfw_window = session.mmio_window(0, 0x118000, 4096);
    let (gpu, gfw) = {
        let mut bar = ProbeBar0 {
            boot: boot_window.pointer(),
            gfw: gfw_window.pointer(),
            bytes,
        };
        let gpu = Ad106::identify(
            PALIT_RTX_4060_TI,
            actual,
            (config[2] >> 16) as u16,
            &mut bar,
        )
        .expect("AD106 identification");
        let gfw = gpu.poll_gfw(&mut bar).expect("GFW snapshot");
        (gpu, gfw)
    };
    session.close_window(boot_window);
    session.close_window(gfw_window);
    // PROM is unusable until GFW completes. This diagnostic still does not wait
    // or imply GSP readiness when the initial sample is locked/in progress.
    let mut vbios_state = p::VbiosState::SkippedGfw;
    let (mut fwsec_bytes, mut fwsec_engines, mut fwsec_ucode, mut fwsec_signature_versions) =
        (0, 0, 0, 0);
    if matches!(gfw, GfwStatus::Complete) {
        let prom = prom::snapshot(&mut session, bytes).expect("bounded PROM snapshot");
        match horn_nvidia_gsp::vbios::Fwsec::from_prom(&prom) {
            Ok(fwsec) => {
                vbios_state = p::VbiosState::FwsecParsed;
                fwsec_bytes = fwsec.ucode.len() as u32;
                fwsec_engines = fwsec.engine_mask as u32;
                fwsec_ucode = fwsec.ucode_id as u32;
                fwsec_signature_versions = fwsec.signature_versions as u32;
                sel4::debug_println!(
                    "NVIDIA_VBIOS_PARSED snapshot_bytes={} fwsec_bytes={} engines={:#x} ucode={} signatures={:#x} authenticated=false",
                    prom.len(),
                    fwsec_bytes,
                    fwsec_engines,
                    fwsec_ucode,
                    fwsec_signature_versions
                );
            }
            Err(error) => {
                vbios_state = p::VbiosState::Rejected;
                sel4::debug_println!(
                    "NVIDIA_VBIOS_REJECTED error={:?} firmware_executed=false",
                    error
                );
            }
        }
    } else {
        sel4::debug_println!("NVIDIA_VBIOS_SKIPPED reason=gfw-not-complete");
    }
    let (gfw_state, gfw_progress) = match gfw {
        GfwStatus::PrivilegeLocked => (p::GfwState::PrivilegeLocked, u32::MAX),
        GfwStatus::InProgress(n) => (p::GfwState::InProgress, n as u32),
        GfwStatus::Complete => (p::GfwState::Complete, 255),
    };
    let snapshot = p::ProbeSnapshot {
        boot0: gpu.boot0,
        boot42: gpu.boot42,
        gfw_state,
        gfw_progress,
        bus_master: false,
        gsp_booted: false,
        vbios_state,
        fwsec_bytes,
        fwsec_engines,
        fwsec_ucode,
        fwsec_signature_versions,
    };
    // READY means this diagnostic reader is available, NOT GSP/IRQ/DMA ready.
    sel4::debug_println!(
        "NVIDIA_PROBE_READY boot0={:#x} boot42={:#x} gfw={:?} bus_master=false gsp_boot=false",
        gpu.boot0,
        gpu.boot42,
        gfw
    );
    horn_runtime::notify_ready(DEVICE_DRIVER_CAPS_READY_SLOT);
    let endpoint = sel4::cap::Endpoint::from_bits(DEVICE_DRIVER_CAPS_SERVICE_SLOT as _);
    let reply = sel4::cap::Reply::from_bits(DEVICE_DRIVER_CAPS_REPLY_SLOT as _);
    let receive = root.absolute_cptr(sel4::CPtr::from_bits(SLOTS.public_receive() as _));
    let mut stopped = false;
    loop {
        let request = {
            let mut server =
                Sel4Server::new(root, endpoint, receive, &mut session.transport().space);
            sel4::with_ipc_buffer_mut(|ipc| ipc.set_recv_slot(&server.recv_slot));
            let (info, badge) = server.endpoint.recv(reply);
            if badge & horn_supervisor::STOP != 0 {
                drop(server);
                receive.delete().unwrap();
                if !stopped {
                    // No DMA was started. Broker close revokes/acknowledges
                    // all CPU aliases before detaching. Never use raw BAR
                    // views after this acknowledgment.
                    time.cancel(&mut session).expect("probe alarm cancel");
                    session.control().close().expect("probe assignment close");
                    stopped = true;
                    sel4::debug_println!("NVIDIA_PROBE_STOPPED dma_started=false");
                    sel4::cap::Notification::from_bits(DEVICE_DRIVER_CAPS_STOPPED_SLOT as _)
                        .signal();
                }
                continue;
            }
            if badge != CLIENT {
                receive.delete().unwrap();
                continue;
            }
            server.decode_received_dynamic(
                info,
                badge,
                reply,
                p::nvidia_probe_object_request_layout,
                p::NvidiaProbeObjectRequest::decode,
            )
        };
        if let Ok((_, request, reply)) = request {
            let response = p::nvidia_probe_object_dispatch(
                request,
                &mut Reader {
                    snapshot: &snapshot,
                    stopped,
                },
            );
            p::nvidia_probe_object_reply(reply, &mut session.transport().space, &response).unwrap();
        }
        receive.delete().unwrap();
    }
}
