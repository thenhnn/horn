//! On-demand TCP storage, bounded accept queues and capability-selected streams.
//! Closing sockets retain buffers, not application handles, until TCP teardown.
use crate::Io;
use alloc::vec::Vec;
use horn_ipc::*;
use horn_net::protocol::*;
use horn_runtime::Context;
use smoltcp::{
    iface::{SocketHandle, SocketSet},
    socket::tcp,
    time::Duration,
};
const BUFFER_SIZE: usize = 8192;
const SOCKET_BYTES: usize = 2 * BUFFER_SIZE;
const MAX_PORTS: usize = 4;
// Two CSpace entries per accepted stream (authority + optional notification).
// Leaves room for listeners, VM/clock imports and transient RPC exports.
const CSPACE_CONNECTION_LIMIT: usize = 32;
struct Limits {
    buffer_bytes: usize,
    connections: usize,
    backlog: usize,
}
impl Limits {
    fn configured() -> Self {
        let this = Self {
            buffer_bytes: option_env!("HORN_TCP_BUFFER_BYTES")
                .unwrap_or("4194304")
                .parse()
                .unwrap(),
            connections: option_env!("HORN_TCP_CONNECTIONS")
                .unwrap_or("32")
                .parse()
                .unwrap(),
            backlog: option_env!("HORN_TCP_BACKLOG")
                .unwrap_or("32")
                .parse()
                .unwrap(),
        };
        assert!(this.buffer_bytes >= SOCKET_BYTES && this.buffer_bytes <= 16 * 1024 * 1024);
        assert!((1..=CSPACE_CONNECTION_LIMIT).contains(&this.connections));
        assert!((1..=128).contains(&this.backlog));
        this
    }
}
struct Watch {
    note: Option<Cap<Notification>>,
    interests: u32,
}
impl Watch {
    fn new() -> Self {
        Self {
            note: None,
            interests: 0,
        }
    }
    fn arm(
        &mut self,
        ctx: &Context,
        io: &mut Io,
        signal: Cap<Notification>,
        interests: u32,
    ) -> Result<(), NetError> {
        if signal.as_ref().unwrapped_badge().is_some() {
            return Err(NetError::Denied);
        }
        let next = ctx.copy(io, signal).map_err(|_| NetError::Exhausted)?;
        if let Some(old) = self.note.replace(next) {
            io.space.delete(old.as_ref()).unwrap();
        }
        self.interests = interests;
        Ok(())
    }
    fn notify(&mut self, io: &mut Io, ready: u32) {
        if ready & self.interests != 0 {
            self.interests = 0;
            if let Some(note) = self.note {
                io.space
                    .cap::<sel4::cap_type::Notification>(note.as_ref())
                    .unwrap()
                    .signal();
            }
        }
    }
    fn release(self, io: &mut Io) {
        if let Some(note) = self.note {
            io.space.delete(note.as_ref()).unwrap();
        }
    }
}
struct Connection {
    badge: u64,
    anchor: Cap<Endpoint>,
    watch: Watch,
}
enum State {
    Pending,
    Open(Connection),
    Closing,
}
struct Slot {
    handle: SocketHandle,
    server: usize,
    state: State,
}
struct Server {
    port: u16,
    watch: Watch,
    _anchor: Cap<Endpoint>,
}
pub struct Service {
    pub ctx: Context,
    pub sockets: SocketSet<'static>,
    pub io: Io,
    servers: Vec<Server>,
    slots: Vec<Option<Slot>>,
    limits: Limits,
    buffer_bytes: usize,
    connections: usize,
    next_badge: u64,
}
impl Service {
    pub fn new(ctx: Context, io: Io, sockets: SocketSet<'static>) -> Self {
        Self {
            ctx,
            sockets,
            io,
            servers: Vec::new(),
            slots: Vec::new(),
            limits: Limits::configured(),
            buffer_bytes: 0,
            connections: 0,
            next_badge: 256,
        }
    }
    fn allocate(&mut self, server: usize, port: u16) -> Result<(), NetError> {
        if self.buffer_bytes + SOCKET_BYTES > self.limits.buffer_bytes {
            return Err(NetError::Exhausted);
        }
        let free = self.slots.iter().position(Option::is_none);
        if free.is_none() {
            self.slots.try_reserve(1).map_err(|_| NetError::Exhausted)?;
        }
        let buffer = || -> Result<Vec<u8>, NetError> {
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(BUFFER_SIZE)
                .map_err(|_| NetError::Exhausted)?;
            bytes.resize(BUFFER_SIZE, 0);
            Ok(bytes)
        };
        let mut socket = tcp::Socket::new(
            tcp::SocketBuffer::new(buffer()?),
            tcp::SocketBuffer::new(buffer()?),
        );
        socket.set_timeout(Some(Duration::from_secs(30)));
        socket.set_nagle_enabled(false);
        socket.set_ack_delay(None);
        socket.listen(port).map_err(|_| NetError::Invalid)?;
        let slot = Some(Slot {
            handle: self.sockets.add(socket),
            server,
            state: State::Pending,
        });
        if let Some(i) = free {
            self.slots[i] = slot;
        } else {
            self.slots.push(slot);
        }
        self.buffer_bytes += SOCKET_BYTES;
        Ok(())
    }
    pub fn maintain(&mut self) {
        for entry in &mut self.slots {
            let Some(slot) = entry else {
                continue;
            };
            if matches!(slot.state, State::Closing)
                && self.sockets.get::<tcp::Socket>(slot.handle).state() == tcp::State::Closed
            {
                self.sockets.remove(slot.handle);
                *entry = None;
                self.buffer_bytes -= SOCKET_BYTES;
            }
        }
        for i in 0..self.servers.len() {
            let mut pending = 0;
            let mut listening = false;
            let mut acceptable = false;
            for slot in self.slots.iter_mut().flatten().filter(|s| s.server == i) {
                if matches!(slot.state, State::Pending) {
                    pending += 1;
                    let socket = self.sockets.get_mut::<tcp::Socket>(slot.handle);
                    if socket.state() == tcp::State::Closed {
                        socket.listen(self.servers[i].port).unwrap();
                    }
                    listening |= socket.state() == tcp::State::Listen;
                    acceptable |= matches!(
                        socket.state(),
                        tcp::State::Established | tcp::State::CloseWait
                    );
                }
                if let State::Open(connection) = &mut slot.state {
                    let socket = self.sockets.get::<tcp::Socket>(slot.handle);
                    let mut ready = 0;
                    if socket.can_recv() || !socket.may_recv() {
                        ready |= TcpEvents::READABLE.0;
                    }
                    if socket.can_send() || !socket.may_send() {
                        ready |= TcpEvents::WRITABLE.0;
                    }
                    if socket.state() == tcp::State::Closed {
                        ready |= TcpEvents::CLOSED.0;
                    }
                    connection.watch.notify(&mut self.io, ready);
                }
            }
            // Keep a listener behind each handshake/accepted connection, not a
            // fixed batch of eight sockets. Budget exhaustion applies backpressure.
            if !listening && pending < self.limits.backlog {
                let _ = self.allocate(i, self.servers[i].port);
            }
            let ready = if acceptable && self.connections < self.limits.connections {
                TcpEvents::ACCEPT.0
            } else {
                0
            };
            self.servers[i].watch.notify(&mut self.io, ready);
        }
    }
    fn connection(&self, badge: u64) -> Result<usize, NetError> {
        self.slots
            .iter()
            .position(|s| {
                s.as_ref()
                    .is_some_and(|s| matches!(&s.state,State::Open(c) if c.badge==badge))
            })
            .ok_or(NetError::Denied)
    }
    fn endpoint(&mut self, badge: u64) -> (Cap<Endpoint>, Cap<Endpoint>) {
        let anchor = self
            .ctx
            .mint(
                &mut self.io,
                self.ctx.cap::<Endpoint>("service").unwrap(),
                badge,
                Rights(Rights::WRITE.0 | Rights::GRANT.0 | Rights::GRANT_REPLY.0),
            )
            .unwrap();
        let exported = self.ctx.copy(&mut self.io, anchor).unwrap();
        (anchor, exported)
    }
}
pub struct Caller<'a> {
    pub service: &'a mut Service,
    pub badge: u64,
}
impl Caller<'_> {
    fn server(&self) -> Result<usize, NetError> {
        let i = self.badge.checked_sub(32).ok_or(NetError::Denied)? as usize;
        if i >= self.service.servers.len() {
            return Err(NetError::Denied);
        }
        Ok(i)
    }
    fn socket(&mut self) -> Result<&mut tcp::Socket<'static>, NetError> {
        let i = self.service.connection(self.badge)?;
        Ok(self
            .service
            .sockets
            .get_mut::<tcp::Socket>(self.service.slots[i].as_ref().unwrap().handle))
    }
}
impl TcpFactoryServer for Caller<'_> {
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
        if self.service.servers.len() == MAX_PORTS {
            return Err(NetError::Exhausted);
        }
        self.service
            .servers
            .try_reserve(1)
            .map_err(|_| NetError::Exhausted)?;
        let index = self.service.servers.len();
        self.service.allocate(index, port as u16)?;
        let (anchor, exported) = self.service.endpoint(32 + index as u64);
        self.service.servers.push(Server {
            port: port as u16,
            watch: Watch::new(),
            _anchor: anchor,
        });
        Ok(Cap::from_ref(exported.as_ref()))
    }
}
impl TcpServerServer for Caller<'_> {
    fn accept(&mut self) -> Result<Cap<TcpConnectionObjectTag>, NetError> {
        let server = self.server()?;
        if self.service.connections == self.service.limits.connections {
            return Err(NetError::Exhausted);
        }
        let i = self
            .service
            .slots
            .iter()
            .position(|s| {
                s.as_ref().is_some_and(|s| {
                    s.server == server
                        && matches!(s.state, State::Pending)
                        && matches!(
                            self.service.sockets.get::<tcp::Socket>(s.handle).state(),
                            tcp::State::Established | tcp::State::CloseWait
                        )
                })
            })
            .ok_or(NetError::WouldBlock)?;
        let badge = self.service.next_badge;
        // Endpoint badges are never reused, even when socket/storage slots are.
        self.service.next_badge = badge.checked_add(1).ok_or(NetError::Exhausted)?;
        let (anchor, exported) = self.service.endpoint(badge);
        self.service.slots[i].as_mut().unwrap().state = State::Open(Connection {
            badge,
            anchor,
            watch: Watch::new(),
        });
        self.service.connections += 1;
        Ok(Cap::from_ref(exported.as_ref()))
    }
    fn watch(&mut self, signal: Cap<Notification>, interests: TcpEvents) -> Result<(), NetError> {
        let i = self.server()?;
        if interests.0 != TcpEvents::ACCEPT.0 {
            return Err(NetError::Invalid);
        }
        self.service.servers[i].watch.arm(
            &self.service.ctx,
            &mut self.service.io,
            signal,
            interests.0,
        )
    }
}
impl TcpConnectionServer for Caller<'_> {
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
        let i = self.service.connection(self.badge)?;
        let slot = self.service.slots[i].as_mut().unwrap();
        let State::Open(connection) = core::mem::replace(&mut slot.state, State::Closing) else {
            unreachable!()
        };
        self.service
            .sockets
            .get_mut::<tcp::Socket>(slot.handle)
            .close();
        self.service.connections -= 1;
        connection.watch.release(&mut self.service.io);
        self.service.ctx.revoke(connection.anchor).unwrap();
        self.service
            .io
            .space
            .delete(connection.anchor.as_ref())
            .unwrap();
        Ok(())
    }
    fn watch(&mut self, signal: Cap<Notification>, interests: TcpEvents) -> Result<(), NetError> {
        let i = self.service.connection(self.badge)?;
        if interests.0 == 0
            || interests.0 & !(TcpEvents::READABLE.0 | TcpEvents::WRITABLE.0 | TcpEvents::CLOSED.0)
                != 0
        {
            return Err(NetError::Invalid);
        }
        let State::Open(connection) = &mut self.service.slots[i].as_mut().unwrap().state else {
            unreachable!()
        };
        connection
            .watch
            .arm(&self.service.ctx, &mut self.service.io, signal, interests.0)
    }
}
