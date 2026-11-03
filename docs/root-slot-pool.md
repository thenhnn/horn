# Shared root CSpace slots

The failed boot exhausted the kernel-object half of the root CNode, not the
whole CNode: 29,411 slots were assigned to kernel objects, while another 29,155
were separately reserved for IPC. The numbers do not by themselves establish a
capability leak.

The devmgr example root now excludes only fixed device-discovery scratch slots
and its public receive slot, then shares the remaining slots between the VM
kernel allocator and IPC. KernelRootCNodeSizeBits, RAM quotas and object-lifetime
rules are unchanged. This removes the artificial half-pool ceiling; it does not
provide an unbounded/dynamically growing CSpace.

RootSlots clones share free space. Sel4CapSpace maintains its own ownership
ledger: registering a borrowed kernel capability does not make that slot an IPC
allocation. Deletion returns a slot to the pool only after successful kernel
deletion and only if IPC allocated it. Kernel object slots remain governed by
the existing kernel allocator's leases/revocation. Fixed receive/discovery slots
never enter the pool.

An `OwnedCap` detaches an allocation from the transport ledger without freeing
capacity. Its retained local CSpace context returns that capacity only after
successful kernel deletion in Drop; duplicate adoption is rejected. See
`docs/capability-ownership.md`.

The pool uses an Arc and a short metadata lock so transport Send requirements
remain sound; no unsafe Send implementation is added to the logger. Pool metadata
allocation must not reenter the same pool (the root uses its existing fixed heap).
No kernel cap deletion/retype is performed while holding the pool lock.

Regression sources cover shared capacity, borrowed-slot ownership, slot reuse
and duplicate IPC release. They are compile-checked, not executed. Full pc-dev
console deployment builds; boot confirmation is left to the user.
