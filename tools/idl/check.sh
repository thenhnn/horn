#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SEL4_PREFIX="${SEL4_PREFIX:-$ROOT/work/sel4}"
LIBCLANG_PATH="${LIBCLANG_PATH:-$ROOT/work/libclang-lib/lib}"
export SEL4_PREFIX
export SEL4_INCLUDE_DIRS="$SEL4_PREFIX/libsel4/include"
export LIBCLANG_PATH

echo "== rust runtime =="
cargo test --quiet --manifest-path "$ROOT/platform/runtime/rust/horn-ipc/Cargo.toml"
cargo check --quiet --features sel4 --manifest-path "$ROOT/platform/runtime/rust/horn-ipc/Cargo.toml"

echo "== c runtime =="
make -C "$ROOT/platform/runtime/c/horn-ipc" clean test >/dev/null
cc -std=gnu11 -Wall -Wextra -Werror -Wno-unused-parameter \
  -I "$ROOT/platform/runtime/c/horn-ipc/include" \
  -I "$SEL4_INCLUDE_DIRS" \
  -I "$SEL4_PREFIX/libsel4/sel4_arch_include/x86_64" \
  -I "$SEL4_PREFIX/libsel4/arch_include/x86" \
  -c "$ROOT/platform/runtime/c/horn-ipc/src/horn_ipc_sel4.c" \
  -o "$ROOT/work/horn_ipc_sel4.o"

echo "== idl tests =="
PYTHONPATH="$ROOT/tools/idl" pytest -q "$ROOT/tests/idl"

echo "== generated rust =="
TMP="$(mktemp -d "$ROOT/work/idl-check.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT
PYTHONPATH="$ROOT/tools/idl" python3 -m horn_idl "$ROOT/examples/idl/schema.py" \
  --rust "$TMP/protocol.rs" --header "$TMP/protocol.h" --c "$TMP/protocol.c"
mkdir -p "$TMP/src"
cp "$TMP/protocol.rs" "$TMP/src/protocol.rs"
printf '#![no_std]\npub mod protocol;\n' > "$TMP/src/lib.rs"
cat > "$TMP/Cargo.toml" <<MANIFEST
[package]
name = "idl-check"
version = "0.1.0"
edition = "2024"

[dependencies]
horn-ipc = { path = "$ROOT/platform/runtime/rust/horn-ipc" }

[workspace]
MANIFEST
cargo check --quiet --offline --manifest-path "$TMP/Cargo.toml"

echo "== generated c =="
cc -std=c11 -Wall -Wextra -Werror \
  -I "$ROOT/platform/runtime/c/horn-ipc/include" -I "$TMP" \
  -fsyntax-only "$TMP/protocol.c"

echo "== tagged enum round trips =="
PYTHONPATH="$ROOT/tools/idl" python3 -m horn_idl "$ROOT/examples/idl/tagged.py" \
  --rust "$TMP/src/protocol.rs" --header "$TMP/protocol.h" --c "$TMP/protocol.c"
cp "$ROOT/tests/idl/tagged.rs" "$TMP/src/lib.rs"
cargo test --quiet --offline --manifest-path "$TMP/Cargo.toml"
cc -std=c11 -Wall -Wextra -Werror \
  -I "$ROOT/platform/runtime/c/horn-ipc/include" -I "$TMP" \
  "$TMP/protocol.c" "$ROOT/tests/idl/tagged.c" \
  "$ROOT/platform/runtime/c/horn-ipc/src/horn_ipc.c" -o "$TMP/tagged-c"
"$TMP/tagged-c"

echo "== vm schema and batch round trips =="
PYTHONPATH="$ROOT/tools/idl" python3 -m horn_idl "$ROOT/platform/core/vm/idl/horn_vm.py" \
  --rust "$TMP/src/protocol.rs" --header "$TMP/protocol.h" --c "$TMP/protocol.c"
cp "$ROOT/tests/idl/vm.rs" "$TMP/src/lib.rs"
cargo test --quiet --offline --manifest-path "$TMP/Cargo.toml"
cc -std=c11 -Wall -Wextra -Werror \
  -I "$ROOT/platform/runtime/c/horn-ipc/include" -I "$TMP" \
  "$TMP/protocol.c" "$ROOT/tests/idl/vm.c" \
  "$ROOT/platform/runtime/c/horn-ipc/src/horn_ipc.c" -o "$TMP/vm-c"
"$TMP/vm-c"

echo "== process schema C bindings =="
PYTHONPATH="$ROOT/tools/idl:$ROOT/platform/core/vm/idl" python3 -m horn_idl \
  "$ROOT/platform/core/procd/idl/horn_process.py" \
  --header "$TMP/protocol.h" --c "$TMP/protocol.c"
cc -std=c11 -Wall -Wextra -Werror \
  -I "$ROOT/platform/runtime/c/horn-ipc/include" -I "$TMP" \
  -c "$TMP/protocol.c" -o "$TMP/process.o"

echo "== device discovery schema and C round trips =="
PYTHONPATH="$ROOT/tools/idl:$ROOT/platform/core/vm/idl:$ROOT/platform/core/clock/idl" python3 -m horn_idl \
  "$ROOT/platform/core/devmgr/idl/horn_device.py" \
  --header "$TMP/protocol.h" --c "$TMP/protocol.c"
cc -std=c11 -Wall -Wextra -Werror \
  -I "$ROOT/platform/runtime/c/horn-ipc/include" -I "$TMP" \
  "$TMP/protocol.c" "$ROOT/tests/idl/device.c" \
  "$ROOT/platform/runtime/c/horn-ipc/src/horn_ipc.c" -o "$TMP/device-c"
"$TMP/device-c"
cargo test --quiet --offline -p horn-devmgr

echo "ok"
