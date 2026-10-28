//! Fixed-response HTTP demo. Only a TCP capability, no Ethernet/config/hardware.
#![no_std]
#![no_main]
use horn_ipc::*;
use horn_net::protocol::*;
const SLOTS: TaskSlots = TaskSlots::new(HTTP_CAPS_FIRST_FREE_SLOT, HTTP_CAPS_SIZE_BITS);
horn_runtime::task!(main,vm:horn_runtime::VmBootstrap{cnode:HTTP_CAPS_SELF_SLOT,factory:HTTP_CAPS_VM_SLOT,address_space:HTTP_CAPS_ADDRESS_SPACE_SLOT,slots:SLOTS});
type Io = Sel4Transport<Sel4CapSpace>;
fn root() -> sel4::cap::CNode {
    sel4::cap::CNode::from_bits(HTTP_CAPS_SELF_SLOT as _)
}
fn cap<T: CapType>(slot: u32) -> Cap<T> {
    horn_runtime::bootstrap_cap(root(), slot)
}
fn wait(io: &mut Io, server: &Cap<TcpServerObjectTag>, events: u32) {
    TcpServerClient::new(io, Cap::from_ref(server.as_ref()))
        .watch(&cap(HTTP_CAPS_SIGNAL_SLOT), TcpEvents(events))
        .unwrap();
    sel4::cap::Notification::from_bits(HTTP_CAPS_EVENTS_SLOT as _).wait();
}
fn handle(io: &mut Io, server: &Cap<TcpServerObjectTag>) {
    let mut header = [0u8; 1024];
    let mut used = 0;
    loop {
        match TcpServerClient::new(&mut *io, Cap::from_ref(server.as_ref())).read() {
            Ok(data) => {
                let len = data.len as usize;
                if len == 0 || len > header.len() - used {
                    return;
                }
                header[used..used + len].copy_from_slice(&data.data[..len]);
                used += len;
                if header[..used].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Err(CallError::Remote(NetError::WouldBlock)) => {
                wait(io, server, TcpEvents::READABLE.0 | TcpEvents::CLOSED.0)
            }
            Err(CallError::Remote(NetError::Closed)) => return,
            Err(e) => panic!("HTTP read: {:?}", e),
        }
    }
    let response=b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 17\r\nConnection: close\r\n\r\nHello from Horn!\n";
    let mut offset = 0;
    while offset < response.len() {
        let n = (response.len() - offset).min(256);
        let mut data = List::default();
        data.len = n as _;
        data.data[..n].copy_from_slice(&response[offset..offset + n]);
        match TcpServerClient::new(&mut *io, Cap::from_ref(server.as_ref())).write(data) {
            Ok(n) => {
                assert!(n > 0 && n as usize <= response.len() - offset);
                offset += n as usize;
            }
            Err(CallError::Remote(NetError::WouldBlock)) => {
                wait(io, server, TcpEvents::WRITABLE.0 | TcpEvents::CLOSED.0)
            }
            Err(CallError::Remote(NetError::Closed)) => return,
            Err(e) => panic!("HTTP write: {:?}", e),
        }
    }
}
fn main() -> ! {
    let mut io = horn_runtime::transport(root(), SLOTS.application()).unwrap();
    let server = TcpFactoryClient::new(&mut io, cap(HTTP_CAPS_TCP_SLOT))
        .listen(80)
        .unwrap();
    sel4::debug_println!("HTTP_DEMO_READY tcp_port=80");
    horn_runtime::notify_ready(HTTP_CAPS_READY_SLOT);
    loop {
        match TcpServerClient::new(&mut io, Cap::from_ref(server.as_ref())).accept() {
            Ok(()) => {
                handle(&mut io, &server);
                TcpServerClient::new(&mut io, Cap::from_ref(server.as_ref()))
                    .finish()
                    .unwrap();
            }
            Err(CallError::Remote(NetError::WouldBlock)) => {
                wait(&mut io, &server, TcpEvents::ACCEPT.0)
            }
            Err(e) => panic!("HTTP accept: {:?}", e),
        }
    }
}
