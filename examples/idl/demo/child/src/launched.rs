//! A separate ELF: no built-in executable-name lookup in the launch service.
#![no_std]
#![no_main]
extern crate alloc;

use horn_ipc::*;
use horn_procd::protocol::*;
horn_runtime::task!(main, deferred);

fn main() -> ! {
    let root = sel4::cap::CNode::from_bits(PROCESS_CAPS_SELF_SLOT as _);
    let receive = root.absolute_cptr(sel4::CPtr::from_bits(PROCESS_CAPS_FIRST_FREE_SLOT as _));
    let space = Sel4CapSpace::new_local(
        root,
        (PROCESS_CAPS_FIRST_FREE_SLOT + 1) as _,
        1 << PROCESS_CAPS_SIZE_BITS,
    );
    let target = Cap::new(
        root.bits() as _,
        PROCESS_CAPS_LIFECYCLE_SLOT,
        sel4::WORD_SIZE as _,
        0,
        CapMode::COPY,
        Rights::ALL,
    );
    let mut lifecycle =
        ProcessLifecycleClient::new(Sel4Transport::new(root, receive, space), target);
    assert!(lifecycle.info().unwrap().charged_bytes > 0);
    sel4::debug_println!("LAUNCH_CHILD_PASS bootstrap=true");
    lifecycle.exit(7).unwrap();
    unreachable!()
}
