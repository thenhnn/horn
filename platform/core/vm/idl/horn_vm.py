"""General VM protocol. Availability is advertised, not implied by declaration."""

from __future__ import annotations

from horn_idl import (
    BOOL,
    U32,
    U64,
    Borrow,
    Cap,
    Copy,
    List,
    Move,
    Option,
    Parcel,
    Result,
    Rights,
    enum,
    flags,
    iface,
    method,
    object,
    record,
    sel4,
)

BATCH_LIMIT = 16


@enum(base=U32)
class VmError:
    Invalid = 1
    Denied = 2
    Overlap = 3
    Exhausted = 4
    QuotaExceeded = 5
    Busy = 6
    Stale = 7
    Unsupported = 8
    Backend = 9
    Quarantined = 10


@flags(base=U32)
class VmProtection:
    READ = 1
    WRITE = 2
    EXECUTE = 4


@flags(base=U32)
class VmMemoryRights:
    READ = 1
    WRITE = 2
    EXECUTE = 4
    DELEGATE = 8
    MAKE_IMMUTABLE = 16
    REVOKE = 32
    DMA = 64
    CLOSE = 128


@flags(base=U32)
class VmFeatures:
    RESIDENT = 1
    DEMAND_ZERO = 2
    PROTECTION = 4
    USER_PAGER = 8
    CREATE_SPACE = 16
    DERIVE_ACCOUNT = 32
    DUPLICATE_MEMORY = 64
    UNTYPED = 128
    IMMUTABLE = 256


@enum(base=U32)
class VmBacking:
    Resident = 0
    DemandZero = 1


@enum(base=U32)
class VmPlacement:
    Anywhere = 0
    Fixed = 1


@record
class VmInfo:
    features: VmFeatures
    page_size: U64
    batch_limit: U32
    limit_bytes: U64
    charged_bytes: U64
    quarantined_bytes: U64


@record
class VmMemoryInfo:
    size: U64
    resident_size: U64
    ceiling: VmProtection
    immutable: BOOL


@record
class VmRegionRequest:
    address: U64
    length: U64
    alignment: U64
    placement: VmPlacement
    ceiling: VmProtection


@record
class VmRegionInfo:
    address: U64
    length: U64
    ceiling: VmProtection


@record
class VmRange:
    offset: U64
    length: U64


@record
class VmMap:
    offset: U64
    length: U64
    memory_offset: U64
    protection: VmProtection


@record
class VmProtect:
    offset: U64
    length: U64
    protection: VmProtection


@enum(base=U32)
class VmUpdate:
    @enum.value(0, type=VmRange)
    def Prepare(): ...

    @enum.value(1, type=VmMap)
    def Map(): ...

    @enum.value(2, type=VmProtect)
    def Protect(): ...

    @enum.value(3, type=VmRange)
    def Unmap(): ...


@record
class VmBatchError:
    completed: U32
    error: VmError


@record
class VmFault:
    id: U64
    cookie: U64
    offset: U64
    access: VmProtection


@enum(base=U32)
class VmFaultReason:
    Invalid = 1
    Denied = 2
    Exhausted = 3


@record
class VmFaultMap:
    id: U64
    memory_offset: U64
    protection: VmProtection


@record
class VmFaultReject:
    id: U64
    reason: VmFaultReason


@enum(base=U32)
class VmFaultReply:
    @enum.value(0, type=VmFaultMap)
    def Map(): ...

    @enum.value(1, type=VmFaultReject)
    def Reject(): ...


@iface("net.nhnn.horn.vm.memory", version=(3, 0))
class VmMemory:
    @method
    def info(self) -> Result[VmMemoryInfo, VmError]: ...

    @method
    def duplicate(
        self, ceiling: VmProtection
    ) -> Result[Cap[VmMemoryObject, Move], VmError]: ...

    @method
    def close(self) -> Result[None, VmError]: ...

    @method
    def revoke(self) -> Result[None, VmError]: ...

    @method
    def make_immutable(self) -> Result[None, VmError]: ...

    @method
    def delegate(
        self, rights: VmMemoryRights
    ) -> Result[Cap[VmMemoryObject, Move], VmError]: ...

    @method
    def rights(self) -> Result[VmMemoryRights, VmError]: ...


@object(implements=(VmMemory,))
class VmMemoryObject:
    pass


@iface("net.nhnn.horn.vm.space", version=(1, 0))
class VmAddressSpace:
    @method
    def reserve(
        self, request: VmRegionRequest
    ) -> Result[Cap[VmRegionObject, Move], VmError]: ...

    @method
    def destroy(self) -> Result[None, VmError]: ...


@object(implements=(VmAddressSpace,))
class VmAddressSpaceObject:
    pass


@iface("net.nhnn.horn.vm.region", version=(1, 0))
class VmRegion:
    @method
    def info(self) -> Result[VmRegionInfo, VmError]: ...

    @method
    def apply(
        self,
        memory: Option[Cap[VmMemoryObject, Borrow]],
        updates: Parcel[List[VmUpdate, BATCH_LIMIT]],
    ) -> Result[None, VmBatchError]: ...

    @method
    def bind_pager(
        self, pager: Cap[VmPagerObject, Borrow], cookie: U64
    ) -> Result[None, VmError]: ...

    @method
    def destroy(self) -> Result[None, VmError]: ...


@object(implements=(VmRegion,))
class VmRegionObject:
    pass


@iface("net.nhnn.horn.vm.pager", version=(1, 0))
class VmPager:
    @method
    def drain(self) -> Result[Parcel[List[VmFault, BATCH_LIMIT]], VmError]: ...

    @method
    def resolve(
        self,
        memory: Option[Cap[VmMemoryObject, Borrow]],
        replies: Parcel[List[VmFaultReply, BATCH_LIMIT]],
    ) -> Result[None, VmBatchError]: ...

    @method
    def destroy(self) -> Result[None, VmError]: ...


@object(implements=(VmPager,))
class VmPagerObject:
    pass


@record
class VmUntypedGrant:
    capability: Cap[sel4.Untyped, Copy]
    size_bits: U32


@iface("net.nhnn.horn.vm.untyped", version=(1, 0))
class VmUntyped:
    @method
    def capability(self) -> Result[VmUntypedGrant, VmError]: ...

    @method
    def close(self) -> Result[None, VmError]: ...


@object(implements=(VmUntyped,))
class VmUntypedObject:
    pass


@iface("net.nhnn.horn.vm.factory", version=(1, 0))
class VmFactory:
    @method
    def info(self) -> Result[VmInfo, VmError]: ...

    @method
    def allocate(
        self, size: U64, backing: VmBacking, ceiling: VmProtection
    ) -> Result[Cap[VmMemoryObject, Move], VmError]: ...

    @method
    def create_address_space(
        self,
    ) -> Result[Cap[VmAddressSpaceObject, Move], VmError]: ...

    @method
    def create_pager(
        self, wake: Cap[sel4.Notification, Copy, Rights.WRITE]
    ) -> Result[Cap[VmPagerObject, Move], VmError]: ...

    @method
    def delegate(
        self, size_bits: U32
    ) -> Result[Cap[VmUntypedObject, Move], VmError]: ...

    @method
    def derive(self, limit_bytes: U64) -> Result[Cap[VmService, Move], VmError]: ...


@object(implements=(VmFactory,))
class VmService:
    pass


@object(implements=(VmFactory, VmMemory, VmAddressSpace, VmRegion, VmPager, VmUntyped))
class VmHost:
    pass
