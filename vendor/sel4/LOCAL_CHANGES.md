# Local VM changes

The upstream revision/tree in `upstream.json` identify the pristine import,
not this locally modified source tree.

The x86-64 ExecuteNever extension derives from l4h patch
`kernel/patches/0009-x86_64-add-seL4_X86_ExecuteNever-for-user-mappings.patch`,
https://github.com/losfair/l4h at
`9e50b64755ec2bed0d897b4d82ccdf7a7db02f09`.

It adds the public VM attribute, PTE execute-disable plumbing and NXE setup.
Horn reads EFER directly instead of retaining the patch's static NXE cache.
Unsupported NX requests return InvalidArgument (argument 2), rather than
silently granting executable access. No snapshot-fix patches are imported here.

The kernel builds and resident-map debug/release boots pass. An explicit
instruction-fetch fault test is still outstanding; this is not a verification
claim for the modified kernel.
