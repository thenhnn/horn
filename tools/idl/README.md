# horn-idl

Python DSL and Rust/C generators for Horn's seL4 IPC.

See [../../docs/idl.md](../../docs/idl.md) for the design.

```sh
PYTHONPATH=tools/idl python3 -m horn_idl examples/idl/schema.py \
    --rust work/idl/protocol.rs \
    --header work/idl/protocol.h \
    --c work/idl/protocol.c
```

## Build integration

`sources.json` is the shared catalog of schema names and locations. Its paths
are relative to `root`, itself relative to the catalog. Register a new root
schema once here; consumers do not enumerate its transitive imports.

A Rust package adds the native, std-only helper using the workspace dependency
(no relative tool paths per consumer):

```toml
[build-dependencies]
horn-idl-build.workspace = true
```

Its `build.rs` normally contains only:

```rust
fn main() {
    horn_idl_build::rust("horn_device", "protocol.rs");
}
```

The helper generates into `OUT_DIR`. The compiler reports the catalog,
generator directory, root schema and actually imported Python schema/helper
files as Cargo rebuild inputs. Editing a transitive import regenerates the
bindings; a fresh build does not rerun the generator. Failed generation fails
the build instead of accepting stale bindings. Package-specific ELF/catalog
embedding remains in that package's build script.

The CLI accepts both registered names and explicit schema paths:

```sh
PYTHONPATH=tools/idl python3 -B -m horn_idl horn_device --rust work/device.rs
```

Normal Python imports resolve from catalog schema directories. No caller needs
a separate `HORN_VM_IDL`, `HORN_CLOCK_IDL`, etc. `HORN_IDL` overrides the compiler
directory for the Rust helper; `HORN_IDL_SOURCES` selects an alternate catalog,
and `PYTHON` selects the helper's Python executable.

### Nix

`nix/rust/default.nix` automatically supplies IDL inputs to crates with a
`horn-idl-build` build dependency, using `tools/idl/default.nix`. No per-package
protocol override or Python import list is necessary. The Nix helper filters
Python sources from catalog directories and produces a store-only catalog;
it never relies on the checkout being accessible inside a build sandbox.
Nix deliberately tracks this catalog bundle conservatively, while local Cargo
tracks actual transitive Python imports.

For declarative service bootstrap data:

```nix
idl = import ../../../tools/idl { pkgs = pkgs.pkgsBuildBuild; };
# ...
bootstrap = idl.capsets "horn_device";
```

The catalog describes build inputs only, not service interfaces or runtime
authority. It does not replace service definitions, grants or capability checks.
Binding generation does not change ABI/layout policy.

The generated code depends on `platform/runtime/rust/horn-ipc` and
`platform/runtime/c/horn-ipc`, not on generated copies of the wire runtime.
