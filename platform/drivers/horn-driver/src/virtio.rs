//! Thin upstream HAL and modern PCI setup for one device session per process.
use crate::{DeviceAccess, DeviceSession};
use core::{
    cell::UnsafeCell,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};
use horn_devmgr::protocol::DeviceControlClient;
use horn_virtio::{
    BufferDirection, Hal, IommuPciTransport, PhysAddr,
    config::ConfigSnapshot,
    virtio_drivers::transport::{
        DeviceType, Transport,
        pci::{
            PciTransport,
            bus::{ConfigurationAccess, DeviceFunction, PciRoot},
        },
    },
};
struct State(UnsafeCell<Option<DeviceSession>>);
unsafe impl Sync for State {}
static STATE: State = State(UnsafeCell::new(None));
static BUSY: AtomicBool = AtomicBool::new(false);
struct Guard;
impl Drop for Guard {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}
pub fn with<R>(f: impl FnOnce(&mut DeviceSession) -> R) -> R {
    assert!(
        BUSY.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok(),
        "HAL reentry"
    );
    let _guard = Guard;
    f(unsafe { (&mut *STATE.0.get()).as_mut().expect("HAL installed") })
}
pub fn install(session: DeviceSession) {
    assert!(
        BUSY.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    );
    let _guard = Guard;
    let state = unsafe { &mut *STATE.0.get() };
    assert!(state.is_none());
    *state = Some(session);
}
fn access(d: BufferDirection) -> DeviceAccess {
    match d {
        BufferDirection::DriverToDevice => DeviceAccess::Read,
        BufferDirection::DeviceToDriver => DeviceAccess::Write,
        BufferDirection::Both => DeviceAccess::ReadWrite,
    }
}
pub struct CapabilityHal;
unsafe impl Hal for CapabilityHal {
    fn dma_alloc(p: usize, d: BufferDirection) -> (PhysAddr, NonNull<u8>) {
        with(|s| s.dma_alloc(p, access(d)))
    }
    unsafe fn dma_dealloc(i: PhysAddr, p: NonNull<u8>, n: usize) -> i32 {
        with(|s| unsafe { s.dma_dealloc(i, p, n) });
        0
    }
    unsafe fn mmio_phys_to_virt(p: PhysAddr, n: usize) -> NonNull<u8> {
        with(|s| s.mmio(p, n))
    }
    unsafe fn share(p: NonNull<[u8]>, d: BufferDirection) -> PhysAddr {
        with(|s| unsafe { s.share(p, access(d)) })
    }
    unsafe fn unshare(i: PhysAddr, p: NonNull<[u8]>, d: BufferDirection) {
        with(|s| unsafe { s.unshare(i, p, access(d)) })
    }
}
pub fn transport(expected: DeviceType) -> (IommuPciTransport<PciTransport>, NonNull<u8>) {
    let resources = with(|s| {
        let r = s.control().resources(s.transport()).unwrap();
        s.map_bars(&r);
        r
    });
    assert_eq!(resources.address.segment, 0);
    let function = DeviceFunction {
        bus: resources.address.bus.try_into().unwrap(),
        device: resources.address.device.try_into().unwrap(),
        function: resources.address.function.try_into().unwrap(),
    };
    let snapshot = ConfigSnapshot::new(
        function,
        resources.configuration.data,
        resources.bar_lengths.data,
    );
    let mut pci = PciRoot::new(snapshot.clone());
    let capability = pci
        .capabilities(function)
        .find(|c| c.id == 9 && c.private_header as u8 >= 16 && c.private_header >> 8 == 1)
        .expect("modern virtio common configuration");
    assert!(capability.offset <= 240);
    let bar = snapshot.read_word(function, capability.offset + 4) as u8;
    let offset = snapshot.read_word(function, capability.offset + 8) as u64;
    let length = snapshot.read_word(function, capability.offset + 12) as usize;
    let (base, size) = pci
        .bar_info(function, bar)
        .unwrap()
        .unwrap()
        .memory_address_size()
        .unwrap();
    assert!(
        offset
            .checked_add(length as u64)
            .is_some_and(|end| end <= size)
    );
    let common = with(|s| s.mmio(base.checked_add(offset).unwrap(), length));
    let pci = PciTransport::new::<CapabilityHal, _>(&mut pci, function).unwrap();
    assert_eq!(pci.device_type(), expected);
    (
        unsafe { IommuPciTransport::new(pci, common, length, 0) }.unwrap(),
        common,
    )
}
/// Modern common configuration status byte. Failure retains every DMA backing.
pub fn reset(common: NonNull<u8>) {
    unsafe {
        common.as_ptr().add(20).write_volatile(0);
    }
    for _ in 0..1_000_000 {
        if unsafe { common.as_ptr().add(20).read_volatile() } == 0 {
            core::sync::atomic::fence(Ordering::SeqCst);
            return;
        }
        core::hint::spin_loop();
    }
    panic!("virtio reset timeout: retain DMA backing");
}
