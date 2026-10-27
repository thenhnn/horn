"""Read-only discovery. IDs select snapshot records, never device authority."""

from horn_idl import (
    BOOL,
    U8,
    U32,
    U64,
    Borrow,
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

# The collector needs imported object dependencies in this module's namespace.
from horn_vm import *


@enum(base=U32)
class DeviceError:
    Invalid = 1
    NotFound = 2
    Exhausted = 3
    Backend = 4
    Unsupported = 5
    Denied = 6
    Busy = 7
    Quarantined = 8


@enum(base=U32)
class DevicePhase:
    DiscoveryReady = 1


@enum(base=U32)
class IommuState:
    # Not a statement about hardware presence. No backend has taken ownership.
    NotInitialized = 0
    Enabled = 1


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


@record
class DeviceDma:
    mapping: Cap[DeviceMappingObject, Move]
    iova: U64


@iface("net.nhnn.horn.device.assign", version=(1, 0))
class DeviceAssignment:
    @method
    def claim(self, id: U64) -> Result[Cap[DeviceObject, Move], DeviceError]: ...


@record
class PciResources:
    address: PciAddress
    # A snapshot only: drivers never receive writable hardware ECAM authority.
    configuration: List[U32, 64]
    # One length per BAR index, zero for absent/unsupported/upper-half entries.
    bar_lengths: List[U64, 6]


@iface("net.nhnn.horn.device.control", version=(1, 0))
class DeviceControl:
    @method
    def resources(self) -> Result[PciResources, DeviceError]: ...

    @method
    def bar(
        self, index: U32, space: Cap[VmAddressSpaceObject, Borrow]
    ) -> Result[Cap[VmMemoryObject, Move], DeviceError]: ...

    @method
    def map(
        self, memory: Cap[VmMemoryObject, Borrow], offset: U64, length: U64, access: U32
    ) -> Result[DeviceDma, DeviceError]: ...

    @method
    def interrupt(self) -> Result[Cap[sel4.Notification, Move], DeviceError]: ...

    @method
    def ack(self) -> Result[None, DeviceError]: ...

    @method
    def activate(self) -> Result[None, DeviceError]: ...

    @method
    def close(self) -> Result[None, DeviceError]: ...


@iface("net.nhnn.horn.device.mapping", version=(1, 0))
class DeviceMapping:
    @method
    def close(self) -> Result[None, DeviceError]: ...


@object(implements=(DeviceControl,))
class DeviceObject:
    pass


@object(implements=(DeviceMapping,))
class DeviceMappingObject:
    pass


@object(implements=(DeviceObserver, DeviceAssignment, DeviceControl, DeviceMapping))
class DeviceHost:
    pass


@record
class PlatformInfo:
    vtd: BOOL
    ticks_per_us: U64


@record
class MmioGrant:
    token: U64
    memory: Cap[VmMemoryObject, Move]


@record
class DmaBacking:
    token: U64
    pages: List[U64, 16]


@record
class IrqRoute:
    token: U64
    vector: U32


# Private bootstrap authority, never included in the discovery capability.
@iface("net.nhnn.horn.device.platform", version=(1, 0))
class DevicePlatform:
    @method
    def rsdp(self) -> Result[List[U8, 36], DeviceError]: ...

    @method
    def read(self, address: U64, length: U32) -> Result[List[U8, 256], DeviceError]: ...

    @method
    def ready(self) -> Result[None, DeviceError]: ...

    @method
    def policy(self) -> Result[PlatformInfo, DeviceError]: ...

    @method
    def write(
        self, address: U64, width: U32, value: U64
    ) -> Result[None, DeviceError]: ...

    @method
    def mmio(
        self, space: Cap[VmAddressSpaceObject, Borrow], address: U64, length: U64
    ) -> Result[MmioGrant, DeviceError]: ...

    @method
    def revoke_mmio(self, token: U64) -> Result[None, DeviceError]: ...

    @method
    def pin(
        self, memory: Cap[VmMemoryObject, Borrow], offset: U64, length: U64, access: U32
    ) -> Result[DmaBacking, DeviceError]: ...

    @method
    def unpin(self, token: U64) -> Result[None, DeviceError]: ...

    @method
    def irq(self, sid: U32) -> Result[IrqRoute, DeviceError]: ...

    @method
    def bind_irq(
        self, token: U64, notification: Cap[sel4.Notification, Borrow]
    ) -> Result[None, DeviceError]: ...

    @method
    def ack_irq(self, token: U64) -> Result[None, DeviceError]: ...

    @method
    def close_irq(self, token: U64) -> Result[None, DeviceError]: ...


@object(implements=(DevicePlatform,))
class DevicePlatformObject:
    pass


@capset(capacity=256)
class DeviceCaps:
    reply: Cap[sel4.Reply, Move]
    service: Cap[sel4.Endpoint, Copy]
    platform: Cap[sel4.Endpoint, Copy]
    vm: Cap[VmFactory, Copy]
    address_space: Cap[VmAddressSpaceObject, Copy]
