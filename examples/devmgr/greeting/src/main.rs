//! A console consumer, not a terminal host or driver.
#![no_std]
#![no_main]
use horn_console::protocol::*;
use horn_ipc::*;
const SLOTS: TaskSlots = TaskSlots::new(
    CONSOLE_CLIENT_CAPS_FIRST_FREE_SLOT,
    CONSOLE_CLIENT_CAPS_SIZE_BITS,
);
horn_runtime::task!(main,vm:horn_runtime::VmBootstrap{
    cnode:CONSOLE_CLIENT_CAPS_SELF_SLOT,factory:CONSOLE_CLIENT_CAPS_VM_SLOT,
    address_space:CONSOLE_CLIENT_CAPS_ADDRESS_SPACE_SLOT,slots:SLOTS,
});
fn cap<T: CapType>(slot: u32) -> Cap<T> {
    horn_runtime::bootstrap_cap(
        sel4::cap::CNode::from_bits(CONSOLE_CLIENT_CAPS_SELF_SLOT as _),
        slot,
    )
}
type Io = Sel4Transport<Sel4CapSpace>;
fn wait(io: &mut Io, events: ConsoleEvents) {
    let mut client = ConsoleInterfaceClient::new(io, cap(CONSOLE_CLIENT_CAPS_SERVICE_SLOT));
    let status = client.status().expect("console status");
    assert_eq!(
        status.events.0 & ConsoleEvents::CLOSED.0,
        0,
        "console closed"
    );
    client
        .watch(
            Some(cap(CONSOLE_CLIENT_CAPS_SIGNAL_SLOT)),
            events,
            status.size_generation,
        )
        .expect("console watch");
    sel4::cap::Notification::from_bits(CONSOLE_CLIENT_CAPS_EVENTS_SLOT as _).wait();
}
fn write_all(io: &mut Io, mut data: &[u8]) {
    while !data.is_empty() {
        let count = data.len().min(256);
        let mut bytes = List::default();
        bytes.len = count as _;
        bytes.data[..count].copy_from_slice(&data[..count]);
        match ConsoleInterfaceClient::new(&mut *io, cap(CONSOLE_CLIENT_CAPS_SERVICE_SLOT))
            .write(bytes)
        {
            Ok(n) => {
                assert!(n > 0 && n as usize <= count);
                data = &data[n as usize..];
            }
            Err(CallError::Remote(ConsoleError::WouldBlock)) => wait(io, ConsoleEvents::WRITABLE),
            error => panic!("console write: {:?}", error),
        }
    }
}
fn main() -> ! {
    let root = sel4::cap::CNode::from_bits(CONSOLE_CLIENT_CAPS_SELF_SLOT as _);
    let mut io = horn_runtime::transport(root, SLOTS.application()).unwrap();
    #[cfg(not(feature = "echo-test"))]
    write_all(&mut io, b"\x1b[1;36mHorn v0.1 started\x1b[0m\r\n");
    #[cfg(feature = "echo-test")]
    write_all(&mut io, b"HORN_VIRTIO_CONSOLE_READY\n");
    horn_runtime::notify_ready(CONSOLE_CLIENT_CAPS_READY_SLOT);
    #[cfg(feature = "echo-test")]
    loop {
        match ConsoleInterfaceClient::new(&mut io, cap(CONSOLE_CLIENT_CAPS_SERVICE_SLOT)).read(256)
        {
            Ok(bytes) => write_all(&mut io, &bytes.data[..bytes.len as usize]),
            Err(CallError::Remote(ConsoleError::WouldBlock)) => {
                wait(&mut io, ConsoleEvents::READABLE)
            }
            error => panic!("console read: {:?}", error),
        }
    }
    #[cfg(not(feature = "echo-test"))]
    {
        let _ = horn_procd::protocol::ProcessLifecycleClient::new(
            &mut io,
            cap(CONSOLE_CLIENT_CAPS_LIFECYCLE_SLOT),
        )
        .exit(0);
        panic!("exit returned");
    }
}
