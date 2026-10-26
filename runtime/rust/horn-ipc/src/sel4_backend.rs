//! seL4 transport backend.
//!
//! This module deliberately owns the seL4-specific parts: the IPC buffer,
//! capability slots, `Call`/`Send`/`Reply`, and source-slot deletion for
//! `Move`.  Generated protocol glue only sees the `Transport` trait.

use sel4::cap_type::Endpoint;
use sel4::{sel4_cfg, sel4_cfg_if, CPtr, Cap, MessageInfo, Word};

use crate::layout::{CapPacker, CapSpace, CapUnpacker};
use crate::{
    CapLayout, CapList, CapMode, CapRef, CapSink, Decoder, Encoder, Rights, RpcError, Transport,
    Wire, MAX_MESSAGE_BYTES,
};

/// A client-side seL4 transport.
///
/// `recv_slot` must address a contiguous range of at least `MAX_CAPS` empty
/// slots.  Received capabilities are placed there by the kernel.
pub struct Sel4Transport<S: CapSpace> {
    pub cnode: sel4::cap::CNode,
    pub recv_slot: sel4::AbsoluteCPtr,
    pub space: S,
}

impl<S: CapSpace> Sel4Transport<S> {
    pub const fn new(cnode: sel4::cap::CNode, recv_slot: sel4::AbsoluteCPtr, space: S) -> Self {
        Self { cnode, recv_slot, space }
    }

    fn delete(&self, slot: u64) {
        let _ = self.cnode.absolute_cptr(CPtr::from_bits(slot as Word)).delete();
    }

    fn encode_args<A: Wire>(&self, args: &A) -> ([u8; MAX_MESSAGE_BYTES], usize) {
        let mut buf = [0u8; MAX_MESSAGE_BYTES];
        let mut enc = Encoder::new(&mut buf);
        args.encode(&mut enc);
        let len = enc.position();
        (buf, len)
    }

    fn install(&self, caps: &CapList, buf: &[u8], len: usize) {
        sel4::with_ipc_buffer_mut(|ipc| {
            ipc.msg_bytes_mut()[..len].copy_from_slice(buf);
            for index in 0..caps.len() {
                let cap = caps.as_slice()[index].unwrap();
                ipc.caps_or_badges_mut()[index] = cap.slot as Word;
            }
        });
    }

    fn commit_moves(&self, caps: &CapList) {
        for index in 0..caps.len() {
            let cap = caps.as_slice()[index].unwrap();
            if cap.mode == CapMode::MOVE {
                self.delete(cap.slot);
            }
        }
    }
}

impl<S: CapSpace> Transport for Sel4Transport<S> {
    fn call<A: Wire, R: Wire>(
        &mut self,
        target: CapRef,
        label: u64,
        args: &A,
        request_layout: &'static CapLayout,
        result_layout: &'static CapLayout,
    ) -> Result<R, RpcError> {
        let (buf, len) = self.encode_args(args);
        let mut packer = CapPacker::new(&mut self.space, request_layout);
        args.caps(&mut packer)?;
        let root_caps = packer.finish()?;
        self.install(&root_caps, &buf[..len], len);
        let words = (len + 7) / 8;
        let target_cptr = self.space.cptr(target)?;
        let reply = Cap::<Endpoint>::from_bits(target_cptr as Word)
            .call(MessageInfo::new(label, 0, root_caps.len(), words));
        let result = sel4::with_ipc_buffer_mut(|ipc| {
            let length = reply.length();
            let extra = reply.extra_caps();
            if length == 0 {
                return Err(RpcError::Truncated);
            }
            let bytes = &ipc.msg_bytes()[..length * 8];
            if bytes.len() < 4 {
                return Err(RpcError::Truncated);
            }
            let status = u32::from_le_bytes(bytes[..4].try_into().unwrap());
            if status != 0 {
                return Err(RpcError::Server(status));
            }
            let mut received = CapList::default();
            for index in 0..extra {
                received.push(CapRef {
                    slot: ipc.caps_or_badges()[index] as u64,
                    mode: CapMode::MOVE,
                    rights: Rights::ALL,
                })?;
            }
            let mut unpacker = CapUnpacker::new_localizing(&mut self.space, result_layout, received);
            let mut dec = Decoder::new(&bytes[4..], &mut unpacker);
            R::decode(&mut dec)
        });
        if result.is_ok() {
            self.commit_moves(&root_caps);
        }
        result
    }

    fn send<A: Wire>(
        &mut self,
        target: CapRef,
        label: u64,
        args: &A,
        request_layout: &'static CapLayout,
    ) -> Result<(), RpcError> {
        let (buf, len) = self.encode_args(args);
        let mut packer = CapPacker::new(&mut self.space, request_layout);
        args.caps(&mut packer)?;
        let root_caps = packer.finish()?;
        self.install(&root_caps, &buf[..len], len);
        let words = (len + 7) / 8;
        let target_cptr = self.space.cptr(target)?;
        let endpoint = Cap::<Endpoint>::from_bits(target_cptr as Word);
        endpoint.send(MessageInfo::new(label, 0, root_caps.len(), words));
        self.commit_moves(&root_caps);
        Ok(())
    }
}

trait ReplySend {
    fn send_reply(self, info: MessageInfo);
}

sel4_cfg_if! {
    if #[sel4_cfg(KERNEL_MCS)] {
        impl ReplySend for sel4::cap::Reply {
            fn send_reply(self, info: MessageInfo) {
                self.send(info);
            }
        }
    } else {
        impl ReplySend for sel4::ImplicitReplyAuthority {
            fn send_reply(self, info: MessageInfo) {
                self.reply(info);
            }
        }
    }
}

/// A server-side seL4 transport.
///
/// `recv` returns a [`Sel4Reply`].  Calling `reply` immediately is the normal
/// synchronous path.  Holding the handle and replying later is the MCS
/// saved-reply async path; the client remains blocked in `Call` unless it
/// uses an explicit reply endpoint/notification protocol.
pub struct Sel4Server<S: CapSpace> {
    pub cnode: sel4::cap::CNode,
    pub endpoint: sel4::cap::Endpoint,
    pub recv_slot: sel4::AbsoluteCPtr,
    pub space: S,
}

impl<S: CapSpace> Sel4Server<S> {
    pub const fn new(
        cnode: sel4::cap::CNode,
        endpoint: sel4::cap::Endpoint,
        recv_slot: sel4::AbsoluteCPtr,
        space: S,
    ) -> Self {
        Self {
            cnode,
            endpoint,
            recv_slot,
            space,
        }
    }

    pub fn recv<A: Wire>(
        &mut self,
        request_layout: &'static CapLayout,
        reply: sel4::ReplyAuthority,
    ) -> Result<(u64, A, Sel4Reply), RpcError> {
        self.recv_request(request_layout, reply, |_label, dec| A::decode(dec))
    }

    /// Receive and decode a request with a schema-generated decoder.
    ///
    /// This is the path used by generated `Request::decode(label, input)`
    /// functions, which are not `Wire` values themselves.
    pub fn recv_request<A, F>(
        &mut self,
        request_layout: &'static CapLayout,
        reply: sel4::ReplyAuthority,
        decode: F,
    ) -> Result<(u64, A, Sel4Reply), RpcError>
    where
        F: for<'a> FnOnce(u64, &mut Decoder<'a>) -> Result<A, RpcError>,
    {
        let endpoint = self.endpoint;
        let recv_slot = self.recv_slot;
        sel4::with_ipc_buffer_mut(|ipc| ipc.set_recv_slot(&recv_slot));
        let (info, _badge) = endpoint.recv(reply);
        let space = &mut self.space;
        let args = sel4::with_ipc_buffer_mut(|ipc| {
            let mut received = CapList::default();
            for index in 0..info.extra_caps() {
                received.push(CapRef {
                    slot: ipc.caps_or_badges()[index] as u64,
                    mode: CapMode::MOVE,
                    rights: Rights::ALL,
                })?;
            }
            let bytes = &ipc.msg_bytes()[..info.length() * 8];
            let mut unpacker = CapUnpacker::new(space, request_layout, received);
            let mut dec = Decoder::new(bytes, &mut unpacker);
            decode(info.label(), &mut dec)
        })?;
        Ok((
            info.label(),
            args,
            Sel4Reply {
                reply,
                label: info.label(),
            },
        ))
    }

    /// Raw receive: request caps remain in the received CNode tree.
    pub fn recv_raw<A: Wire>(
        &mut self,
        request_layout: &'static CapLayout,
        reply: sel4::ReplyAuthority,
    ) -> Result<(u64, A, Sel4Reply), RpcError> {
        self.recv_request(request_layout, reply, |_label, dec| A::decode(dec))
    }
}

pub struct Sel4Reply {
    reply: sel4::ReplyAuthority,
    pub label: u64,
}

impl Sel4Reply {
    pub fn reply<S: CapSpace, R: Wire>(
        self,
        space: &mut S,
        result: &R,
        result_layout: &'static CapLayout,
    ) -> Result<(), RpcError> {
        self.reply_with(
            space,
            result_layout,
            |out| result.encode(out),
            |caps| result.caps(caps),
        )
    }

    pub fn reply_with<S: CapSpace, F, G>(
        self,
        space: &mut S,
        result_layout: &'static CapLayout,
        encode: F,
        caps: G,
    ) -> Result<(), RpcError>
    where
        F: for<'a> FnOnce(&mut Encoder<'a>),
        G: FnOnce(&mut dyn CapSink) -> Result<(), RpcError>,
    {
        let mut packer = CapPacker::new(space, result_layout);
        caps(&mut packer)?;
        let root_caps = packer.finish()?;
        let mut result_buf = [0u8; MAX_MESSAGE_BYTES];
        let mut result_enc = Encoder::new(&mut result_buf);
        encode(&mut result_enc);
        let result_len = result_enc.position();
        let mut buf = [0u8; MAX_MESSAGE_BYTES];
        buf[..4].copy_from_slice(&0u32.to_le_bytes());
        buf[4..4 + result_len].copy_from_slice(&result_buf[..result_len]);
        let len = 4 + result_len;
        let words = (len + 7) / 8;
        sel4::with_ipc_buffer_mut(|ipc| {
            ipc.msg_bytes_mut()[..len].copy_from_slice(&buf[..len]);
            for index in 0..root_caps.len() {
                let cap = root_caps.as_slice()[index].unwrap();
                ipc.caps_or_badges_mut()[index] = cap.slot as Word;
            }
        });
        self.reply
            .send_reply(MessageInfo::new(self.label, 0, root_caps.len(), words));
        for index in 0..root_caps.len() {
            let cap = root_caps.as_slice()[index].unwrap();
            if cap.mode == CapMode::MOVE {
                space.delete(cap)?;
            }
        }
        Ok(())
    }

    pub fn reply_status(self, status: u32) -> Result<(), RpcError> {
        let mut buf = [0u8; 8];
        buf[..4].copy_from_slice(&status.to_le_bytes());
        sel4::with_ipc_buffer_mut(|ipc| {
            ipc.msg_bytes_mut()[..4].copy_from_slice(&buf[..4]);
        });
        self.reply
            .send_reply(MessageInfo::new(self.label, 0, 0, 1));
        Ok(())
    }
}
