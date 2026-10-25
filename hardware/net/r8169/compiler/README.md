# Realtek firmware compiler

Decodes r8169 host-side firmware bytecode into async Rust with structured loops
and labeled blocks. PHY/MCU I/O stays synchronous; firmware delays await a timer.

- `emit/control.rs`: validates and structures branches and loops.
- `emit/flow.rs`: propagates PHY/MCU mode through branches and backedges.
- `emit/statements.rs`: emits I/O and constant write tables.
- `emit.rs`: assembles functions, blocks and the firmware registry.
- `chips.rs`: validates chip JSON and emits ordered rules and firmware references.

Runs of eight or more constant writes become table-driven loops. Runs never
cross a branch target, and every write remains in its original order. Known
modes use direct PHY/MCU calls; ambiguous joins retain a runtime mode flag,
checked once around a write loop rather than once per write.

```sh
nix-shell --run 'cargo test -p rtl-compiler'
```

Tests compile the real `rtl-firmware` runtime with generated Rust, comparing
I/O and delay traces with a reference interpreter. They also check suspension,
resumption, and dropping pending runs and their timer futures. They cover branch targets inside write runs, mode joins,
backedges, nested labels, and the bundled firmware with zero and patterned reads.
These are translation tests, not hardware validation. Generated test artifacts
are retained under `work/r8169-compiler/`.
