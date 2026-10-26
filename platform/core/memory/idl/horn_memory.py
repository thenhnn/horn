"""Initial resident backing protocol. No raw frame or mapping delegation."""

from horn_idl import U64, Cap, Move, iface, method, object, record


@record
class MemoryInfo:
    size: U64
    resident_size: U64
    page_size: U64


@iface("net.nhnn.horn.memory.object", version=(1, 0))
class Memory:
    @method
    def info(self) -> MemoryInfo: ...

    @method
    def release(self) -> None: ...


@object(implements=(Memory,))
class MemoryObject:
    pass


@iface("net.nhnn.horn.memory.allocator", version=(1, 0))
class MemoryAllocator:
    @method
    def allocate(self, size: U64) -> Cap[MemoryObject, Move]: ...


@object(implements=(MemoryAllocator,))
class MemoryAllocatorService:
    pass
