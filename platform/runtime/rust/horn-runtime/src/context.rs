//! Explicit single-thread initial context. Bootstrap caps are process-lifetime views.
use crate::{Error, TaskSlots, VmBootstrap, bootstrap_cap};
use horn_ipc::*;

pub struct Context {
    pub(crate) record: libhorn::startup::Context<'static>,
    root: sel4::cap::CNode,
    slots: TaskSlots,
    application: Option<core::ops::Range<u32>>,
    inbox_available: bool,
}
impl Context {
    /// # Safety
    /// Bytes must be the loader's immutable, resident, process-lifetime record.
    /// The loader must have installed the claimed caps and empty slot interval.
    pub(crate) unsafe fn from_record(bytes: &'static [u8]) -> Result<Self, Error> {
        let record = libhorn::startup::Context::parse(bytes).map_err(|_| Error::Invalid)?;
        let h = record.header();
        if h.cspace_bits < 3
            || h.cspace_bits >= 32
            || h.free_end != 1u64 << h.cspace_bits
            || h.free_start > h.free_end / 2 - 3
        {
            return Err(Error::Invalid);
        }
        let slots = TaskSlots::new(h.free_start as u32, h.cspace_bits as u8);
        Ok(Self {
            record,
            root: sel4::cap::CNode::from_bits(h.cspace_root as _),
            slots,
            application: Some(slots.application()),
            inbox_available: true,
        })
    }
    pub(crate) unsafe fn install_logging(&mut self) -> Result<(), Error> {
        let Some(_) = self.record.lookup("horn.log") else {
            return Ok(());
        };
        let sink = self.cap::<horn_log::protocol::LogSinkTag>("horn.log")?;
        let range = self.application.as_mut().ok_or(Error::Invalid)?;
        let end = range.start.checked_add(2).ok_or(Error::Invalid)?;
        if end.checked_add(2).is_none_or(|n| n > range.end) {
            return Err(Error::Invalid);
        }
        let logging = range.start..end;
        range.start = end;
        let io = crate::transport(self.root, logging)?;
        unsafe { horn_log::install(io, sink).map_err(|_| Error::Invalid) }
    }
    pub fn cap<T: CapType>(&self, name: &str) -> Result<Cap<T>, Error> {
        let slot = self.record.lookup(name).ok_or(Error::Invalid)?;
        Ok(bootstrap_cap(self.root, slot as u32))
    }
    pub fn transport(&mut self) -> Result<Sel4Transport<Sel4CapSpace>, Error> {
        crate::transport(self.root, self.application.take().ok_or(Error::Invalid)?)
    }
    pub(crate) fn heap_bootstrap(&self) -> Result<VmBootstrap, Error> {
        Ok(VmBootstrap {
            cnode: self.root.bits() as u32,
            factory: self.cap::<Endpoint>("horn.vm")?.as_ref().slot,
            address_space: self.cap::<Endpoint>("horn.address-space")?.as_ref().slot,
            slots: self.slots,
        })
    }
    pub fn notify_ready(&self) -> Result<(), Error> {
        sel4::cap::Notification::from_bits(
            self.cap::<Notification>("horn.ready")?.as_ref().slot as _,
        )
        .signal();
        Ok(())
    }
    pub fn wait(&self, name: &str) -> Result<(), Error> {
        sel4::cap::Notification::from_bits(self.cap::<Notification>(name)?.as_ref().slot as _)
            .wait();
        Ok(())
    }
    pub fn copy<T: CapType>(
        &self,
        io: &mut Sel4Transport<Sel4CapSpace>,
        source: Cap<T>,
    ) -> Result<Cap<T>, RpcError> {
        if source.as_ref().cnode != self.root.bits() as u64
            || source.as_ref().depth != sel4::WORD_SIZE as u8
        {
            return Err(RpcError::NotLocal);
        }
        let slot = io.space.reserve_slot()?;
        slot.copy(
            &self
                .root
                .absolute_cptr(sel4::CPtr::from_bits(source.as_ref().slot as _)),
            sel4::CapRights::all(),
        )
        .map_err(|_| RpcError::Transport)?;
        Ok(Cap::from_ref(io.space.register(
            sel4::cap::Unspecified::from_bits(slot.path().bits()),
        )?))
    }
    pub fn mint<T: CapType>(
        &self,
        io: &mut Sel4Transport<Sel4CapSpace>,
        source: Cap<T>,
        badge: u64,
        rights: Rights,
    ) -> Result<Cap<T>, RpcError> {
        if source.as_ref().cnode != self.root.bits() as u64
            || source.as_ref().depth != sel4::WORD_SIZE as u8
        {
            return Err(RpcError::NotLocal);
        }
        let slot = io.space.reserve_slot()?;
        slot.mint(
            &self
                .root
                .absolute_cptr(sel4::CPtr::from_bits(source.as_ref().slot as _)),
            sel4::CapRights::new(
                rights.0 & 8 != 0,
                rights.0 & 4 != 0,
                rights.0 & 1 != 0,
                rights.0 & 2 != 0,
            ),
            badge as _,
        )
        .map_err(|_| RpcError::Transport)?;
        Ok(Cap::from_ref(io.space.register(
            sel4::cap::Unspecified::from_bits(slot.path().bits()),
        )?))
    }
    pub fn inbox(&mut self, name: &str) -> Result<Inbox, Error> {
        if !self.inbox_available {
            return Err(Error::Invalid);
        }
        let endpoint = self.cap::<Endpoint>(name)?;
        let reply = self.cap::<Reply>("horn.reply")?;
        self.inbox_available = false;
        Ok(Inbox {
            root: self.root,
            endpoint: sel4::cap::Endpoint::from_bits(endpoint.as_ref().slot as _),
            reply: sel4::cap::Reply::from_bits(reply.as_ref().slot as _),
            receive: self
                .root
                .absolute_cptr(sel4::CPtr::from_bits(self.slots.public_receive() as _)),
        })
    }
}

pub struct Inbox {
    root: sel4::cap::CNode,
    endpoint: sel4::cap::Endpoint,
    reply: sel4::cap::Reply,
    receive: sel4::AbsoluteCPtr,
}
impl Inbox {
    /// Notification badges are coalesced hints; the consumer distinguishes them
    /// using its deployment badge convention, not an inferred RPC message type.
    pub fn receive(&mut self) -> Received<'_> {
        sel4::with_ipc_buffer_mut(|ipc| ipc.set_recv_slot(&self.receive));
        let (info, badge) = self.endpoint.recv(self.reply);
        Received {
            inbox: self,
            info: Some(info),
            badge,
        }
    }
}
pub struct Received<'a> {
    inbox: &'a mut Inbox,
    info: Option<sel4::MessageInfo>,
    badge: u64,
}
impl Received<'_> {
    pub fn badge(&self) -> u64 {
        self.badge
    }
    pub fn decode<A, L, F>(
        &mut self,
        space: &mut Sel4CapSpace,
        layout: L,
        decode: F,
    ) -> Result<(u64, A, Sel4Reply), RpcError>
    where
        L: FnOnce(u64) -> Result<&'static CapLayout, RpcError>,
        F: for<'a> FnOnce(u64, &mut Decoder<'a>) -> Result<A, RpcError>,
    {
        let info = self.info.take().ok_or(RpcError::InvalidValue)?;
        Sel4Server::new(
            self.inbox.root,
            self.inbox.endpoint,
            self.inbox.receive,
            space,
        )
        .decode_received_dynamic(info, self.badge, self.inbox.reply, layout, decode)
    }
}
impl Drop for Received<'_> {
    fn drop(&mut self) {
        self.inbox
            .receive
            .delete()
            .expect("runtime receive cleanup");
    }
}
