# Resident memory

- `idl/horn_memory.py`: allocation, info and release with typed `Result[..., MemoryError]` replies (protocol version 2).
- `src/lib.rs`: resident backing and rollback.
- `src/sel4.rs`: retyping/revocation with rust-sel4 `BasicAllocator` bookkeeping.
- `src/service.rs`: root-hosted service using generated fallible dispatch.

Provision a page budget and live-object limit explicitly. Each page has its own
Untyped ancestor; release revokes its frames before returning it to the pool.
seL4 clears frame contents on retype. Object badges separate allocator and object
authority; interface types alone do not enforce that boundary.

The demo owns the zeroing/reuse probes. The service runs until shutdown, with no
test counters or scratch mapping. No mapping RPC, pager, process-death cleanup or
last-handle reference tracking is implemented yet.
