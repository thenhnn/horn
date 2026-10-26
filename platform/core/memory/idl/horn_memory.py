"""Initial resident backing protocol. No raw frame or mapping delegation."""

from horn_idl import U32, U64, Cap, Move, Result, enum, iface, method, object, record


@enum(base=U32)
class MemoryError:
    InvalidSize = 1
    Exhausted = 2
    MetadataExhausted = 3
    Backend = 4
    Denied = 5
    BadHandle = 6


@record
class MemoryInfo:
    size: U64
    resident_size: U64
    page_size: U64


@iface("net.nhnn.horn.memory.object", version=(2, 0))
class Memory:
    @method
    def info(self) -> Result[MemoryInfo, MemoryError]: ...

    @method
    def release(self) -> Result[None, MemoryError]: ...


@object(implements=(Memory,))
class MemoryObject:
    pass


@iface("net.nhnn.horn.memory.allocator", version=(2, 0))
class MemoryAllocator:
    @method
    def allocate(self, size: U64) -> Result[Cap[MemoryObject, Move], MemoryError]: ...


@object(implements=(MemoryAllocator,))
class MemoryAllocatorService:
    pass


@object(implements=(MemoryAllocator, Memory))
class MemoryHost:
    pass
