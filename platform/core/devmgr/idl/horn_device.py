"""Read-only discovery. IDs select snapshot records, never device authority."""

from horn_idl import (
    BOOL,
    U8,
    U32,
    U64,
    Cap,
    Copy,
    List,
    Move,
    Result,
    capset,
    enum,
    iface,
    method,
    object,
    record,
    sel4,
)


@enum(base=U32)
class DeviceError:
    Invalid = 1
    NotFound = 2
    Exhausted = 3
    Backend = 4
    Unsupported = 5


@enum(base=U32)
class DevicePhase:
    DiscoveryReady = 1


@enum(base=U32)
class IommuState:
    # Not a statement about hardware presence. No backend has taken ownership.
    NotInitialized = 0


@record
class DeviceStatus:
    phase: DevicePhase
    pci_devices: U32
    iommu: IommuState


@record
class PciAddress:
    segment: U32
    bus: U32
    device: U32
    function: U32


@record
class PciDevice:
    id: U64
    address: PciAddress
    vendor_id: U32
    device_id: U32
    class_code: U32
    subclass: U32
    programming_interface: U32
    revision: U32
    header_type: U32


@enum(base=U32)
class PciBarKind:
    Io = 1
    Memory32 = 2
    Memory64 = 3
    MemoryBelow1M = 4


@record
class PciBar:
    index: U32
    kind: PciBarKind
    address: U64
    prefetchable: BOOL
    # No length: sizing requires config writes, forbidden during discovery.


@record
class PciResizableBar:
    index: U32
    supported_sizes: U64
    current_size: U64


@record
class PciDeviceInfo:
    device: PciDevice
    command: U32
    interrupt_line: U32
    interrupt_pin: U32
    bars: List[PciBar, 6]
    msi: BOOL
    msix: BOOL
    resizable_bars: List[PciResizableBar, 6]


@record
class DevicePage:
    entries: List[PciDevice, 4]
    next: U64


@iface("net.nhnn.horn.device.observe", version=(1, 0))
class DeviceObserver:
    @method
    def status(self) -> Result[DeviceStatus, DeviceError]: ...

    @method
    def list(self, after: U64) -> Result[DevicePage, DeviceError]: ...

    @method
    def info(self, id: U64) -> Result[PciDeviceInfo, DeviceError]: ...


@object(implements=(DeviceObserver,))
class DeviceDiscovery:
    pass


# Private bootstrap authority, never included in the discovery capability.
@iface("net.nhnn.horn.device.platform", version=(1, 0))
class DevicePlatform:
    @method
    def rsdp(self) -> Result[List[U8, 36], DeviceError]: ...

    @method
    def read(self, address: U64, length: U32) -> Result[List[U8, 256], DeviceError]: ...

    @method
    def ready(self) -> Result[None, DeviceError]: ...


@object(implements=(DevicePlatform,))
class DevicePlatformObject:
    pass


@capset(capacity=256)
class DeviceCaps:
    reply: Cap[sel4.Reply, Move]
    service: Cap[sel4.Endpoint, Copy]
    platform: Cap[sel4.Endpoint, Copy]
