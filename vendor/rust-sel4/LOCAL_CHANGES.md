# Local VM changes

The revision/tree in `upstream.json` remain the pristine import's provenance.

- x86 VM attributes expose the vendored kernel's ExecuteNever flag and an
  `EXECUTE_NEVER` constant on x86-64.
- `sel4-abstract-allocator::BasicAllocator` independently examines the next free
  hole when deallocating. Previously, freeing below the lowest hole could fail
  to merge it, preventing reuse of the complete range. Regression tests in
  `platform/core/vm/src/allocator_tests.rs` cover all three-allocation release
  permutations and repeated slot recycling.

- x86-64 TCB wrappers expose Horn's bounded ReadFPU/WriteFPU frame API. They
  add no new authority and use the generated kernel invocation bindings.

These are source edits, not build-time patches. Existing panic-environment
compatibility changes predate this VM work.
