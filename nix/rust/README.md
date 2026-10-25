# Rust

The Cargo workspace lives at the repository root (`Cargo.toml`, `Cargo.lock`).
crate2nix builds its members and dependencies as separate Nix derivations.
Builds use `config.sel4.pkgs`, the matching seL4 SDK, and one pinned nightly compiler
for both native build tools and seL4 code.

`target` selects a rust-sel4 target suffix: on PC, `sel4` means
`x86_64-sel4`, while `sel4-roottask` means `x86_64-sel4-roottask`.

## Examples

Select a root-task workspace member:

```nix
let
  workspace = config.rust.workspace.override {target = "sel4-roottask";};
in workspace.members.rust-root-task
```

Select Cargo features:

```nix
workspace.members.my-library.override {features = ["default" "feature-name"];}
```

Profiles set `rust.release`: false for debug builds, true for optimized builds.

```sh
nix-shell --run 'just rust-generate'
nix-shell --run 'pytest -q tests/boot/test_rust.py'
```
