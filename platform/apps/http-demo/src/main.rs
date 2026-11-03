//! Single-threaded, nonblocking HTTP event loop with independently scoped streams.
#![no_std]
#![no_main]
extern crate alloc;
use alloc::vec::Vec;
use horn_ipc::*;
use horn_net::protocol::*;
horn_runtime::task!(main);
type Io = Sel4Transport<Sel4CapSpace>;
const MAX_CONNECTIONS: usize = 32;
const RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 17\r\nConnection: close\r\n\r\nHello from Horn!\n";
enum Phase {
    Headers(usize),
    Response(usize),
    Done,
}
struct Connection {
    tcp: Cap<TcpConnectionObjectTag>,
    header: [u8; 1024],
    phase: Phase,
}
impl Connection {
    fn poll(&mut self, io: &mut Io, signal: Cap<Notification>) {
        // Bound RPC work per connection per turn. A partial request or
        // backpressured writer arms its own hint, never waits here.
        let mut budget = 8;
        loop {
            if budget == 0 {
                let events = match self.phase {
                    Phase::Headers(_) => TcpEvents::READABLE.0,
                    Phase::Response(_) => TcpEvents::WRITABLE.0,
                    Phase::Done => return,
                };
                TcpConnectionReceiver::from_cap(Cap::from_ref(self.tcp.as_ref()))
                    .watch(io, &signal, TcpEvents(events | TcpEvents::CLOSED.0))
                    .unwrap();
                return;
            }
            budget -= 1;
            let interest = match self.phase {
                Phase::Headers(used) => {
                    match TcpConnectionReceiver::from_cap(Cap::from_ref(self.tcp.as_ref()))
                        .read(&mut *io)
                    {
                        Ok(data) => {
                            let n = data.len as usize;
                            if n == 0 || n > self.header.len() - used {
                                self.phase = Phase::Done;
                                continue;
                            }
                            self.header[used..used + n].copy_from_slice(&data.data[..n]);
                            if self.header[..used + n].windows(4).any(|w| w == b"\r\n\r\n") {
                                self.phase = Phase::Response(0);
                            } else if used + n == self.header.len() {
                                self.phase = Phase::Done;
                            } else {
                                self.phase = Phase::Headers(used + n);
                            }
                            continue;
                        }
                        Err(CallError::Remote(NetError::WouldBlock)) => TcpEvents::READABLE.0,
                        Err(CallError::Remote(NetError::Closed)) => {
                            self.phase = Phase::Done;
                            continue;
                        }
                        Err(e) => panic!("HTTP read: {:?}", e),
                    }
                }
                Phase::Response(offset) => {
                    let n = (RESPONSE.len() - offset).min(256);
                    let mut data = List::default();
                    data.len = n as _;
                    data.data[..n].copy_from_slice(&RESPONSE[offset..offset + n]);
                    match TcpConnectionReceiver::from_cap(Cap::from_ref(self.tcp.as_ref()))
                        .write(&mut *io, data)
                    {
                        Ok(written) => {
                            assert!(written > 0 && written as usize <= n);
                            let next = offset + written as usize;
                            self.phase = if next == RESPONSE.len() {
                                Phase::Done
                            } else {
                                Phase::Response(next)
                            };
                            continue;
                        }
                        Err(CallError::Remote(NetError::WouldBlock)) => TcpEvents::WRITABLE.0,
                        Err(CallError::Remote(NetError::Closed)) => {
                            self.phase = Phase::Done;
                            continue;
                        }
                        Err(e) => panic!("HTTP write: {:?}", e),
                    }
                }
                Phase::Done => return,
            };
            TcpConnectionReceiver::from_cap(Cap::from_ref(self.tcp.as_ref()))
                .watch(io, &signal, TcpEvents(interest | TcpEvents::CLOSED.0))
                .unwrap();
            return;
        }
    }
    fn close(self, io: &mut Io) {
        TcpConnectionReceiver::from_cap(Cap::from_ref(self.tcp.as_ref()))
            .finish(&mut *io)
            .unwrap();
        io.space.delete(self.tcp.as_ref()).unwrap();
    }
}
fn main(mut ctx: horn_runtime::Context) -> ! {
    let mut io = ctx.transport().unwrap();
    let signal = ctx.cap::<Notification>("signal").unwrap();
    let listener = ctx
        .interface::<TcpFactoryReceiver>("tcp")
        .unwrap()
        .listen(&mut io, 80)
        .unwrap();
    let mut connections = Vec::<Connection>::new();
    horn_runtime::logging::info!("listening on TCP port 80");
    ctx.notify_ready().unwrap();
    loop {
        while connections.len() < MAX_CONNECTIONS {
            connections
                .try_reserve(1)
                .expect("HTTP connection metadata quota");
            match TcpServerReceiver::from_cap(Cap::from_ref(listener.as_ref())).accept(&mut io) {
                Ok(tcp) => connections.push(Connection {
                    tcp,
                    header: [0; 1024],
                    phase: Phase::Headers(0),
                }),
                Err(CallError::Remote(NetError::WouldBlock | NetError::Exhausted)) => break,
                Err(e) => panic!("HTTP accept: {:?}", e),
            }
        }
        let mut i = 0;
        while i < connections.len() {
            connections[i].poll(&mut io, signal);
            if matches!(connections[i].phase, Phase::Done) {
                connections.swap_remove(i).close(&mut io);
            } else {
                i += 1;
            }
        }
        if connections.len() < MAX_CONNECTIONS {
            TcpServerReceiver::from_cap(Cap::from_ref(listener.as_ref()))
                .watch(&mut io, &signal, TcpEvents::ACCEPT)
                .unwrap();
        }
        // All active streams were driven to WouldBlock and armed. Notifications
        // coalesce; re-scan conditions after waking instead of trusting a token.
        ctx.wait("events").unwrap();
    }
}
