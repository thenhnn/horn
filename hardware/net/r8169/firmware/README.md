# Compiled realtek firmware

`rtl-firmware` compiles the realtek bytecode in `blobs/` into allocation-free,
`no_std` Rust.

```rust,ignore
use rtl_firmware::FIRMWARE;

let firmware = FIRMWARE.iter().find(|fw| fw.name == "rtl8125b-2").unwrap();
firmware.run(&mut io, &mut timer).await;
```

Implement `PhyIo` for synchronous register access and `Timer` for asynchronous
delays. Constructing a run does no I/O.

## Chip selection

`chips.json` is the ordered source for chip descriptors and firmware selection from Linux.

```rust,ignore
let chip = rtl_firmware::match_chip(xid, gmii, extended_xid)?;
if let Some(firmware) = chip.firmware {
    firmware.run(&mut io, &mut timer).await;
}
```
