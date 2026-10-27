# horn-idl

Python DSL and Rust/C generators for Horn's seL4 IPC.

See [../../docs/idl.md](../../docs/idl.md) for the design.

```sh
PYTHONPATH=tools/idl python3 -m horn_idl examples/idl/schema.py \
    --rust work/idl/protocol.rs \
    --header work/idl/protocol.h \
    --c work/idl/protocol.c
```

The generated code depends on `platform/runtime/rust/horn-ipc` and
`platform/runtime/c/horn-ipc`, not on generated copies of the wire runtime.
