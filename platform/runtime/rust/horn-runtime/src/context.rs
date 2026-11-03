//! Explicit single-thread initial context. Bootstrap caps are process-lifetime views.
use crate::{Error, TaskSlots, bootstrap_cap};
use horn_ipc::*;

pub struct Context {
    pub(crate) record: libhorn::startup::Context<'static>,
    root: sel4::cap::CNode,
    slots: TaskSlots,
    application: Option<core::ops::Range<u32>>,
    heap: Option<core::ops::Range<u32>>,
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
            heap: Some(slots.heap()),
            inbox_available: true,
        })
    }
    pub(crate) unsafe fn install_logging(&mut self) -> Result<(), Error> {
        let Some(_) = self.record.lookup("horn.log") else {
            return Ok(());
        };
        let sink = self.cap::<horn_log::protocol::LogSinkTag>("horn.log")?;
        let io = self.logging_transport()?;
        unsafe { horn_log::install(io, sink).map_err(|_| Error::Invalid) }
    }
    pub fn cap<T: CapType>(&self, name: &str) -> Result<Cap<T>, Error> {
        let slot = self.record.lookup(name).ok_or(Error::Invalid)?;
        Ok(bootstrap_cap(self.root, slot as u32))
    }
    /// Obtain a non-owning initial object handle with all its client interfaces.
    /// The deployment contract supplies its type; no remote check is performed.
    pub fn object<T: RpcObject>(&self, name: &str) -> Result<Cap<T>, Error> {
        self.cap::<T>(name)
    }
    /// View a named initial capability as an IDL interface, without IPC or
    /// ownership transfer. The deployment contract supplies the interface type;
    /// this lookup is not a runtime server-type check.
    pub fn interface<I: RpcInterface>(&self, name: &str) -> Result<Cap<I::Tag>, Error> {
        self.cap::<I::Tag>(name)
    }
    pub fn cspace_bits(&self) -> u8 {
        self.record.header().cspace_bits as u8
    }
    /// Reserve a dedicated transport for a logger connected during bootstrap.
    /// Call before transport(); never share application receive/import slots.
    pub fn logging_transport(&mut self) -> Result<Sel4Transport<Sel4CapSpace>, Error> {
        let range = self.application.as_mut().ok_or(Error::Invalid)?;
        let end = range.start.checked_add(2).ok_or(Error::Invalid)?;
        if end.checked_add(2).is_none_or(|n| n > range.end) {
            return Err(Error::Invalid);
        }
        let logging = range.start..end;
        range.start = end;
        crate::transport(self.root, logging)
    }
    pub fn transport(&mut self) -> Result<Sel4Transport<Sel4CapSpace>, Error> {
        crate::transport(self.root, self.application.take().ok_or(Error::Invalid)?)
    }
    /// Take the exclusively reserved heap backend once. Normally installed by
    /// task!; explicit ownership supports staged allocator tests without ABI1.
    pub fn heap(&mut self) -> Result<crate::allocator::VmHeap, Error> {
        let factory = self.cap::<horn_vm::protocol::VmFactoryTag>("horn.vm")?;
        let space = self.cap::<horn_vm::protocol::VmAddressSpaceObjectTag>("horn.address-space")?;
        let range = self.heap.take().ok_or(Error::Invalid)?;
        crate::allocator::VmHeap::new(
            crate::transport(self.root, range)?,
            factory.as_ref(),
            space.as_ref(),
        )
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
    /// Revoke descendants of a locally held anchor without deleting the anchor.
    pub fn revoke<T: CapType>(&self, anchor: Cap<T>) -> Result<(), RpcError> {
        if anchor.as_ref().cnode != self.root.bits() as u64
            || anchor.as_ref().depth != sel4::WORD_SIZE as u8
        {
            return Err(RpcError::NotLocal);
        }
        self.root
            .absolute_cptr(sel4::CPtr::from_bits(anchor.as_ref().slot as _))
            .revoke()
            .map_err(|_| RpcError::Transport)
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
        self.receive_inner(false)
    }
    pub fn poll(&mut self) -> Received<'_> {
        self.receive_inner(true)
    }
    fn receive_inner(&mut self, polling: bool) -> Received<'_> {
        sel4::with_ipc_buffer_mut(|ipc| ipc.set_recv_slot(&self.receive));
        let (info, badge) = if polling {
            self.endpoint.nb_recv(self.reply)
        } else {
            self.endpoint.recv(self.reply)
        };
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
    pub fn fault(&self) -> Option<sel4::Fault> {
        self.info
            .as_ref()
            .map(|info| sel4::with_ipc_buffer(|ipc| sel4::Fault::new(ipc, info)))
    }
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
#[cfg(test)]
mod interface_tests {
    extern crate std;
    use super::*;
    use libhorn::startup::{Entry, RuntimeConfig, encode};

    type View = Cap<Endpoint>;

    #[test]
    fn named_interface_is_an_initial_view_without_reserving_transport() {
        let entries = [
            Entry {
                name: "clock",
                selector: 3,
            },
            Entry {
                name: "events",
                selector: 4,
            },
        ];
        let mut bytes = std::vec![0; 512];
        let size = encode(
            RuntimeConfig {
                ipc_buffer: 0x1000,
                cspace_root: 0,
                cspace_bits: 8,
                free_start: 8,
                free_end: 256,
            },
            &entries,
            &mut bytes,
        )
        .unwrap();
        bytes.truncate(size);
        let bytes = std::boxed::Box::leak(bytes.into_boxed_slice());
        // No kernel operations: selectors are only metadata in this fixture.
        let ctx = unsafe { Context::from_record(bytes) }.unwrap();
        let application = ctx.application.clone();
        let heap = ctx.heap.clone();
        let clock = ctx.interface::<View>("clock").unwrap();
        assert_eq!(clock.as_ref().slot, 3);
        assert_eq!(ctx.cap::<Notification>("events").unwrap().as_ref().slot, 4);
        assert!(matches!(
            ctx.interface::<View>("missing"),
            Err(Error::Invalid)
        ));
        assert_eq!(ctx.application, application);
        assert_eq!(ctx.heap, heap);
        assert!(ctx.inbox_available);
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
