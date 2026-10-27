use super::*;
struct Fake {
    common: NonNull<u8>,
    features: u64,
    selected: u64,
    status: DeviceStatus,
    legacy: bool,
    reject: bool,
    queue: Option<u16>,
}
impl Fake {
    fn new(common: NonNull<u8>) -> Self {
        Self {
            common,
            features: VERSION_1 | ACCESS_PLATFORM,
            selected: 0,
            status: DeviceStatus::empty(),
            legacy: false,
            reject: false,
            queue: None,
        }
    }
}
impl Transport for Fake {
    fn device_type(&self) -> DeviceType {
        DeviceType::Console
    }
    fn read_device_features(&mut self) -> u64 {
        self.features
    }
    fn write_driver_features(&mut self, features: u64) {
        self.selected = features;
    }
    fn max_queue_size(&mut self, _: u16) -> u32 {
        2
    }
    fn notify(&mut self, _: u16) {}
    fn get_status(&self) -> DeviceStatus {
        self.status
    }
    fn set_status(&mut self, status: DeviceStatus) {
        self.status = if self.reject {
            status & !DeviceStatus::FEATURES_OK
        } else {
            status
        };
    }
    fn set_guest_page_size(&mut self, _: u32) {}
    fn requires_legacy_layout(&self) -> bool {
        self.legacy
    }
    fn queue_set(&mut self, queue: u16, _: u32, _: PhysAddr, _: PhysAddr, _: PhysAddr) {
        assert_eq!(
            unsafe { self.common.as_ptr().add(22).cast::<u16>().read_volatile() },
            queue
        );
        assert_eq!(
            unsafe { self.common.as_ptr().add(26).cast::<u16>().read_volatile() },
            0
        );
        self.queue = Some(queue);
    }
    fn queue_unset(&mut self, _: u16) {
        self.queue = None;
    }
    fn queue_used(&mut self, queue: u16) -> bool {
        self.queue == Some(queue)
    }
    fn ack_interrupt(&mut self) -> InterruptStatus {
        InterruptStatus::QUEUE_INTERRUPT
    }
    fn read_config_generation(&self) -> u32 {
        0
    }
    fn read_config_space<V: FromBytes + IntoBytes>(&self, _: usize) -> virtio_drivers::Result<V> {
        Err(virtio_drivers::Error::Unsupported)
    }
    fn write_config_space<V: IntoBytes + Immutable>(
        &mut self,
        _: usize,
        _: V,
    ) -> virtio_drivers::Result<()> {
        Err(virtio_drivers::Error::Unsupported)
    }
}
#[test]
fn iommu_feature_and_msix_selectors_are_driver_side_adaptations() {
    let mut common = [0u64; 8];
    let pointer = NonNull::new(common.as_mut_ptr().cast()).unwrap();
    let mut transport =
        unsafe { IommuPciTransport::new(Fake::new(pointer), pointer, 64, 0) }.unwrap();
    transport.set_status(DeviceStatus::ACKNOWLEDGE | DeviceStatus::DRIVER);
    transport.write_driver_features(VERSION_1);
    assert_eq!(transport.inner.selected, VERSION_1 | ACCESS_PLATFORM);
    transport
        .set_status(DeviceStatus::ACKNOWLEDGE | DeviceStatus::DRIVER | DeviceStatus::FEATURES_OK);
    transport.queue_set(1, 2, 0x10000, 0x11000, 0x12000);
    assert!(transport.queue_used(1));
    transport.queue_unset(1);
    assert!(!transport.queue_used(1));
    assert!(transport.ack_interrupt() == InterruptStatus::QUEUE_INTERRUPT);
}
#[test]
fn reject_legacy_missing_platform_access_and_invalid_register_span() {
    let mut common = [0u64; 8];
    let pointer = NonNull::new(common.as_mut_ptr().cast()).unwrap();
    let mut fake = Fake::new(pointer);
    fake.legacy = true;
    assert!(matches!(
        unsafe { IommuPciTransport::new(fake, pointer, 64, 0) },
        Err(Error::LegacyTransport)
    ));
    let mut fake = Fake::new(pointer);
    fake.features = VERSION_1;
    assert!(matches!(
        unsafe { IommuPciTransport::new(fake, pointer, 64, 0) },
        Err(Error::MissingPlatformAccess)
    ));
    assert!(matches!(
        unsafe { IommuPciTransport::new(Fake::new(pointer), pointer, 32, 0) },
        Err(Error::InvalidCommonRegion)
    ));
}
#[test]
#[should_panic(expected = "virtio features rejected")]
fn check_feature_acceptance_before_continuing() {
    let mut common = [0u64; 8];
    let pointer = NonNull::new(common.as_mut_ptr().cast()).unwrap();
    let mut fake = Fake::new(pointer);
    fake.reject = true;
    let mut transport = unsafe { IommuPciTransport::new(fake, pointer, 64, 0) }.unwrap();
    transport
        .set_status(DeviceStatus::ACKNOWLEDGE | DeviceStatus::DRIVER | DeviceStatus::FEATURES_OK);
}
