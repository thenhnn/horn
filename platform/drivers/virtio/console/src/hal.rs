//! Upstream virtio Hal adapter. Resource ownership lives in horn-driver.
use core::{
    cell::UnsafeCell,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};
use horn_devmgr::protocol::*;
use horn_driver::{DeviceAccess, DeviceSession};
use horn_ipc::*;
use horn_virtio::{BufferDirection, Hal, PhysAddr};

pub struct State {
    pub session: DeviceSession,
    #[cfg(feature = "lifecycle-test")]
    supervisor: Cap<Endpoint>,
    #[cfg(feature = "lifecycle-test")]
    lifecycle: Cap<Endpoint>,
    #[cfg(feature = "dma-probe")]
    stale: Option<u64>,
    #[cfg(feature = "dma-probe")]
    injected: bool,
    #[cfg(feature = "dma-probe")]
    pub fault_wait: Option<sel4::cap::Notification>,
}
impl State {
    pub fn new(
        session: DeviceSession,
        _supervisor: Cap<Endpoint>,
        _lifecycle: Cap<Endpoint>,
    ) -> Self {
        Self {
            session,
            #[cfg(feature = "lifecycle-test")]
            supervisor: _supervisor,
            #[cfg(feature = "lifecycle-test")]
            lifecycle: _lifecycle,
            #[cfg(feature = "dma-probe")]
            stale: None,
            #[cfg(feature = "dma-probe")]
            injected: false,
            #[cfg(feature = "dma-probe")]
            fault_wait: None,
        }
    }
}
fn access(direction: BufferDirection) -> DeviceAccess {
    match direction {
        BufferDirection::DriverToDevice => DeviceAccess::Read,
        BufferDirection::DeviceToDriver => DeviceAccess::Write,
        BufferDirection::Both => DeviceAccess::ReadWrite,
    }
}
struct Storage(UnsafeCell<Option<State>>);
// Upstream Hal has static methods; only this adapter needs singleton dispatch.
// The runtime is single-threaded; BUSY detects reentry, not SMP routing.
unsafe impl Sync for Storage {}
static STATE: Storage = Storage(UnsafeCell::new(None));
static BUSY: AtomicBool = AtomicBool::new(false);
struct Guard;
impl Drop for Guard {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}
pub fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
    assert!(
        BUSY.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok(),
        "HAL reentry"
    );
    let _guard = Guard;
    f(unsafe { (&mut *STATE.0.get()).as_mut().expect("HAL installed") })
}
pub fn install(state: State) {
    assert!(
        BUSY.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    );
    let _guard = Guard;
    let slot = unsafe { &mut *STATE.0.get() };
    assert!(slot.is_none());
    *slot = Some(state);
}
#[cfg(feature = "lifecycle-test")]
pub fn probe_for_test() {
    use horn_supervisor::protocol::*;
    with(|state| {
        assert!(matches!(
            SupervisorJob::from_ref(state.supervisor.as_ref())
                .cleanup_done(state.session.transport(),),
            Err(CallError::Remote(SupervisorError::Denied))
        ));
        assert!(matches!(
            SupervisorFactory::from_ref(state.supervisor.as_ref())
                .core_log_sink(state.session.transport(),),
            Err(CallError::Remote(SupervisorError::Denied))
        ));
    });
}
#[cfg(feature = "lifecycle-test")]
pub fn stop_for_test(force: bool) {
    use horn_supervisor::protocol::*;
    with(|state| {
        let client = SupervisorRuntime::from_ref(state.supervisor.as_ref());
        // READY is asynchronous; request_stop is legal during startup as well.
        assert!(matches!(
            client.status(state.session.transport()).unwrap(),
            ServiceStatus::Starting | ServiceStatus::Active
        ));
        client
            .request_stop(
                state.session.transport(),
                if force {
                    StopMode::Force
                } else {
                    StopMode::Graceful
                },
            )
            .unwrap();
    });
}
#[cfg(feature = "lifecycle-test")]
pub fn exit_for_test() -> ! {
    with(|state| {
        let _ = horn_procd::protocol::ProcessLifecycle::from_ref(state.lifecycle.as_ref())
            .exit(state.session.transport(), 0);
        panic!("exit unexpectedly returned");
    })
}
pub struct CapabilityHal;
// SAFETY: horn-driver owns fresh resident backing, directional independent
// bounces and IOVA mappings. Unshare closes/drains before copy-back or release;
// MMIO resolution is confined to owned BAR grants. No RAM paddr is exposed.
unsafe impl Hal for CapabilityHal {
    fn dma_alloc(pages: usize, direction: BufferDirection) -> (PhysAddr, NonNull<u8>) {
        with(|state| state.session.dma_alloc(pages, access(direction)))
    }
    unsafe fn dma_dealloc(iova: PhysAddr, pointer: NonNull<u8>, pages: usize) -> i32 {
        with(|state| {
            unsafe {
                state.session.dma_dealloc(iova, pointer, pages);
            }
            0
        })
    }
    unsafe fn mmio_phys_to_virt(physical: PhysAddr, size: usize) -> NonNull<u8> {
        with(|state| state.session.mmio(physical, size))
    }
    unsafe fn share(buffer: NonNull<[u8]>, direction: BufferDirection) -> PhysAddr {
        with(|state| {
            let iova = unsafe { state.session.share(buffer, access(direction)) };
            #[cfg(feature = "dma-probe")]
            if direction == BufferDirection::DriverToDevice {
                match option_env!("HORN_DMA_PROBE") {
                    Some("outside") => {
                        sel4::debug_println!("DMA_PROBE outside=true address=dead0000");
                        state.injected = true;
                        return 0xdead_0000;
                    }
                    Some("stale") => {
                        if let Some(stale) = state.stale {
                            state.injected = true;
                            sel4::debug_println!(
                                "DMA_PROBE stale=true drained=true warmed=true address={:x}",
                                stale
                            );
                            return stale;
                        }
                    }
                    _ => {}
                }
            }
            iova
        })
    }
    unsafe fn unshare(iova: PhysAddr, buffer: NonNull<[u8]>, direction: BufferDirection) {
        with(|state| {
            #[cfg(feature = "dma-probe")]
            if state.injected {
                let notification = state.fault_wait.expect("test fault park notification");
                loop {
                    notification.wait();
                }
            }
            unsafe {
                state.session.unshare(iova, buffer, access(direction));
            }
            #[cfg(feature = "dma-probe")]
            if direction == BufferDirection::DriverToDevice
                && option_env!("HORN_DMA_PROBE") == Some("stale")
            {
                state.stale = Some(iova);
            }
        })
    }
}
