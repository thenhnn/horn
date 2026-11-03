//! A separate ELF: no built-in executable-name lookup in the launch service.
#![no_std]
#![no_main]
extern crate alloc;

use horn_ipc::*;
use horn_procd::protocol::*;
horn_runtime::task!(main);

fn main(mut ctx: horn_runtime::Context) -> ! {
    let io = ctx.transport().unwrap();
    let target = ctx.cap("horn.lifecycle").unwrap();
    let mut lifecycle = ProcessLifecycleClient::new(io, target);
    assert!(lifecycle.info().unwrap().charged_bytes > 0);
    sel4::debug_println!("LAUNCH_CHILD_PASS bootstrap=true");
    lifecycle.exit(7).unwrap();
    unreachable!()
}
