//! Port reservations and capability-selected servers. One accepted stream per cap;
//! eight independent TCP slots permit graceful FIN/TIME_WAIT without blocking listen.
use crate::{Io, cap, root};
use alloc::{vec, vec::Vec};
use horn_ipc::*;
use horn_net::protocol::*;
use smoltcp::{
    iface::{SocketHandle, SocketSet},
    socket::tcp,
    time::Duration,
};
pub struct Slot {
    handle: SocketHandle,
    closing: bool,
}
pub struct Server {
    port: u16,
    slots: Vec<Slot>,
    current: Option<usize>,
    note: Option<Cap<Notification>>,
    interests: u32,
    _anchor: Cap<Endpoint>,
}
pub struct Service {
    pub sockets: SocketSet<'static>,
    pub servers: Vec<Server>,
    pub io: Io,
}
impl Service {
    pub fn maintain(&mut self) {
        for server in &mut self.servers {
            let mut listening = false;
            for slot in &mut server.slots {
                let socket = self.sockets.get_mut::<tcp::Socket>(slot.handle);
                if socket.state() == tcp::State::Closed {
                    slot.closing = false;
                }
                if socket.state() == tcp::State::Listen {
                    listening = true;
                }
            }
            if !listening {
                if let Some((_, slot)) = server.slots.iter().enumerate().find(|(i, s)| {
                    Some(*i) != server.current
                        && self.sockets.get::<tcp::Socket>(s.handle).state() == tcp::State::Closed
                }) {
                    self.sockets
                        .get_mut::<tcp::Socket>(slot.handle)
                        .listen(server.port)
                        .unwrap();
                }
            }
            let mut ready = 0;
            if let Some(i) = server.current {
                let socket = self.sockets.get::<tcp::Socket>(server.slots[i].handle);
                // EOF is readable; a peer FIN must not repeatedly wake a
                // backpressured writer whose send half remains open.
                if socket.can_recv() || !socket.may_recv() {
                    ready |= TcpEvents::READABLE.0;
                }
                if socket.can_send() || !socket.may_send() {
                    ready |= TcpEvents::WRITABLE.0;
                }
                if socket.state() == tcp::State::Closed {
                    ready |= TcpEvents::CLOSED.0;
                }
            } else if server.slots.iter().any(|slot| {
                !slot.closing
                    && matches!(
                        self.sockets.get::<tcp::Socket>(slot.handle).state(),
                        tcp::State::Established | tcp::State::CloseWait
                    )
            }) {
                ready |= TcpEvents::ACCEPT.0;
            }
            if ready & server.interests != 0 {
                server.interests = 0;
                if let Some(note) = server.note {
                    self.io
                        .space
                        .cap::<sel4::cap_type::Notification>(note.as_ref())
                        .unwrap()
                        .signal();
                }
            }
        }
    }
}
pub struct Caller<'a> {
    pub service: &'a mut Service,
    pub badge: u64,
}
impl Caller<'_> {
    fn index(&self) -> Result<usize, NetError> {
        let i = self.badge.checked_sub(32).ok_or(NetError::Denied)? as usize;
        if i >= self.service.servers.len() {
            return Err(NetError::Denied);
        }
        Ok(i)
    }
    fn socket(&mut self) -> Result<&mut tcp::Socket<'static>, NetError> {
        let i = self.index()?;
        let server = &self.service.servers[i];
        let j = server.current.ok_or(NetError::WouldBlock)?;
        Ok(self
            .service
            .sockets
            .get_mut::<tcp::Socket>(server.slots[j].handle))
    }
}
impl TcpFactory for Caller<'_> {
    fn listen(&mut self, port: u32) -> Result<Cap<TcpServerObjectTag>, NetError> {
        if self.badge != 16 {
            return Err(NetError::Denied);
        }
        if port == 0 || port > 65535 {
            return Err(NetError::Invalid);
        }
        if self.service.servers.iter().any(|s| s.port == port as u16) {
            return Err(NetError::Denied);
        }
        if self.service.servers.len() == 4 {
            return Err(NetError::Exhausted);
        }
        let mut slots = Vec::new();
        for _ in 0..8 {
            let mut socket = tcp::Socket::new(
                tcp::SocketBuffer::new(vec![0; 8192]),
                tcp::SocketBuffer::new(vec![0; 8192]),
            );
            socket.set_timeout(Some(Duration::from_secs(30)));
            socket.set_nagle_enabled(false);
            socket.set_ack_delay(None);
            slots.push(Slot {
                handle: self.service.sockets.add(socket),
                closing: false,
            });
        }
        let anchor = horn_supervisor::client::mint(
            &mut self.service.io,
            root(),
            cap::<Endpoint>(NETD_CAPS_SERVICE_SLOT),
            32 + self.service.servers.len() as u64,
            Rights(Rights::WRITE.0 | Rights::GRANT.0 | Rights::GRANT_REPLY.0),
        );
        let exported = horn_supervisor::client::copy(&mut self.service.io, root(), anchor);
        self.service.servers.push(Server {
            port: port as u16,
            slots,
            current: None,
            note: None,
            interests: 0,
            _anchor: anchor,
        });
        self.service.maintain();
        Ok(Cap::from_ref(exported.as_ref()))
    }
}
impl TcpServer for Caller<'_> {
    fn accept(&mut self) -> Result<(), NetError> {
        let i = self.index()?;
        let server = &mut self.service.servers[i];
        if server.current.is_some() {
            return Err(NetError::Denied);
        }
        let j = server
            .slots
            .iter()
            .position(|slot| {
                !slot.closing
                    && matches!(
                        self.service.sockets.get::<tcp::Socket>(slot.handle).state(),
                        tcp::State::Established | tcp::State::CloseWait
                    )
            })
            .ok_or(NetError::WouldBlock)?;
        server.current = Some(j);
        Ok(())
    }
    fn read(&mut self) -> Result<List<u8, 256>, NetError> {
        let socket = self.socket()?;
        if !socket.can_recv() {
            return Err(if socket.may_recv() {
                NetError::WouldBlock
            } else {
                NetError::Closed
            });
        }
        let mut data = List::default();
        data.len = socket
            .recv_slice(&mut data.data)
            .map_err(|_| NetError::Closed)? as _;
        Ok(data)
    }
    fn write(&mut self, data: List<u8, 256>) -> Result<u32, NetError> {
        let socket = self.socket()?;
        if !socket.can_send() {
            return Err(if socket.may_send() {
                NetError::WouldBlock
            } else {
                NetError::Closed
            });
        }
        Ok(socket
            .send_slice(&data.data[..data.len as usize])
            .map_err(|_| NetError::Closed)? as u32)
    }
    fn finish(&mut self) -> Result<(), NetError> {
        let i = self.index()?;
        let server = &mut self.service.servers[i];
        let j = server.current.take().ok_or(NetError::Closed)?;
        self.service
            .sockets
            .get_mut::<tcp::Socket>(server.slots[j].handle)
            .close();
        server.slots[j].closing = true;
        Ok(())
    }
    fn watch(&mut self, signal: Cap<Notification>, interests: TcpEvents) -> Result<(), NetError> {
        let i = self.index()?;
        if interests.0 == 0 || interests.0 & !15 != 0 {
            return Err(NetError::Invalid);
        }
        if signal.as_ref().unwrapped_badge().is_some() {
            return Err(NetError::Denied);
        }
        let next = horn_supervisor::client::copy(&mut self.service.io, root(), signal);
        let server = &mut self.service.servers[i];
        if let Some(old) = server.note.replace(next) {
            self.service.io.space.delete(old.as_ref()).unwrap();
        }
        server.interests = interests.0;
        Ok(())
    }
}
