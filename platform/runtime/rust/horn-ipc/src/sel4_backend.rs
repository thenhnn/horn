//! seL4 transport backend.
//!
//! This module deliberately owns the seL4-specific parts: the IPC buffer,
//! capability slots, `Call`/`Send`/`Reply`, and source-slot deletion for
//! `Move`.  Generated protocol glue only sees the `Transport` trait.

use sel4::cap_type::Endpoint;
use sel4::{CPtr, Cap, MessageInfo, Word, sel4_cfg, sel4_cfg_if};

use crate::layout::{CapPacker, CapSpace, CapUnpacker};
use crate::{
    CapLayout, CapMode, CapRef, CapSink, Decoder, Encoder, MAX_MESSAGE_BYTES, Rights, RpcError,
    Transport, Wire, WireCaps,
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
        Self {
            cnode,
            recv_slot,
            space,
        }
    }

    fn delete(&self, slot: u64) {
        let _ = self
            .cnode
            .absolute_cptr(CPtr::from_bits(slot as Word))
            .delete();
    }

    fn encode_args<A: Wire>(&self, args: &A) -> ([u8; MAX_MESSAGE_BYTES], usize) {
        let mut buf = [0u8; MAX_MESSAGE_BYTES];
        let mut enc = Encoder::new(&mut buf);
        args.encode(&mut enc);
        let len = enc.position();
        (buf, len)
    }

    fn install(&self, caps: &WireCaps, buf: &[u8], len: usize) {
        sel4::with_ipc_buffer_mut(|ipc| {
            ipc.msg_bytes_mut()[..len].copy_from_slice(buf);
            for index in 0..caps.len() {
                let cap = caps.as_slice()[index].unwrap();
                ipc.caps_or_badges_mut()[index] = cap.slot as Word;
            }
        });
    }

    fn commit_moves(&self, caps: &WireCaps) {
        for index in 0..caps.len() {
            let cap = caps.as_slice()[index].unwrap();
            if cap.mode == CapMode::MOVE {
                self.delete(cap.slot.into());
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
        let wire_caps = packer.finish()?;
        if wire_caps.len() > 1 {
            return Err(RpcError::TooManyCaps);
        }
        self.install(&wire_caps, &buf[..len], len);
        sel4::with_ipc_buffer_mut(|ipc| ipc.set_recv_slot(&self.recv_slot));
        let words = (len + 7) / 8;
        let target_cptr = self.space.cptr(target)?;
        let reply = Cap::<Endpoint>::from_bits(target_cptr as Word).call(MessageInfo::new(
            label,
            0,
            wire_caps.len(),
            words,
        ));
        let mut bytes = [0u8; MAX_MESSAGE_BYTES];
        let length = reply.length() * 8;
        if length < 4 || length > bytes.len() {
            return Err(RpcError::Truncated);
        }
        sel4::with_ipc_buffer_mut(|ipc| {
            bytes[..length].copy_from_slice(&ipc.msg_bytes()[..length])
        });
        if reply.extra_caps() > 1 || reply.caps_unwrapped() != 0 {
            return Err(RpcError::InvalidValue);
        }
        let status = u32::from_le_bytes(bytes[..4].try_into().unwrap());
        if status != 0 {
            return Err(RpcError::Server(status));
        }
        let mut received = WireCaps::default();
        let received_cap = CapRef::new(
            self.recv_slot.root().bits() as u64,
            self.recv_slot.path().bits() as u32,
            self.recv_slot.path().depth() as u8,
            0,
            CapMode::MOVE,
            Rights::ALL,
        );
        if reply.extra_caps() == 1 {
            received.push(received_cap)?;
        }
        // Capture bytes before localization: CNode calls overwrite the IPC buffer.
        let result = {
            let mut unpacker =
                CapUnpacker::new_localizing(&mut self.space, result_layout, received);
            let mut dec = Decoder::new(&bytes[4..length], &mut unpacker);
            R::decode(&mut dec).and_then(|value| {
                unpacker.finish()?;
                Ok(value)
            })
        };
        if reply.extra_caps() == 1 {
            self.recv_slot.delete().map_err(|_| RpcError::Transport)?;
        }
        if result.is_ok() {
            self.commit_moves(&wire_caps);
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
        let wire_caps = packer.finish()?;
        self.install(&wire_caps, &buf[..len], len);
        let words = (len + 7) / 8;
        let target_cptr = self.space.cptr(target)?;
        let endpoint = Cap::<Endpoint>::from_bits(target_cptr as Word);
        endpoint.send(MessageInfo::new(label, 0, wire_caps.len(), words));
        self.commit_moves(&wire_caps);
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
        self.recv_request_dynamic(|_| Ok(request_layout), reply, decode)
    }

    /// Receive a request and choose its capability layout from the label.
    ///
    /// The label is only known after `recv`, so this is the multi-method
    /// counterpart to [`recv_request`](Self::recv_request).
    pub fn recv_request_dynamic<A, L, F>(
        &mut self,
        layout_for: L,
        reply: sel4::ReplyAuthority,
        decode: F,
    ) -> Result<(u64, A, Sel4Reply), RpcError>
    where
        L: FnOnce(u64) -> Result<&'static CapLayout, RpcError>,
        F: for<'a> FnOnce(u64, &mut Decoder<'a>) -> Result<A, RpcError>,
    {
        let endpoint = self.endpoint;
        let recv_slot = self.recv_slot;
        sel4::with_ipc_buffer_mut(|ipc| ipc.set_recv_slot(&recv_slot));
        let (info, badge) = endpoint.recv(reply);
        self.decode_received_dynamic(info, badge, reply, layout_for, decode)
    }

    /// Decode an envelope obtained directly from the kernel (for example by a
    /// deadline loop using NBRecv). Call before any invocation overwrites IPC
    /// state. The caller must distinguish no-sender from a real receive; NBRecv
    /// provides no usable MessageInfo in the no-sender case.
    pub fn decode_received_dynamic<A, L, F>(
        &mut self,
        info: sel4::MessageInfo,
        badge: Word,
        reply: sel4::ReplyAuthority,
        layout_for: L,
        decode: F,
    ) -> Result<(u64, A, Sel4Reply), RpcError>
    where
        L: FnOnce(u64) -> Result<&'static CapLayout, RpcError>,
        F: for<'a> FnOnce(u64, &mut Decoder<'a>) -> Result<A, RpcError>,
    {
        let recv_slot = self.recv_slot;
        let label = info.label();
        let request_layout = layout_for(label)?;
        if info.extra_caps() > 1 || info.length() * 8 > MAX_MESSAGE_BYTES {
            return Err(RpcError::InvalidValue);
        }
        // Snapshot IPC state before any CNode invocation reuses the IPC buffer.
        let mut bytes = [0; MAX_MESSAGE_BYTES];
        let mut received = WireCaps::default();
        sel4::with_ipc_buffer(|ipc| {
            bytes[..info.length() * 8].copy_from_slice(&ipc.msg_bytes()[..info.length() * 8]);
            if info.extra_caps() == 1 {
                let cap = if info.caps_unwrapped() == 1 {
                    CapRef::from_unwrapped_badge(ipc.caps_or_badges()[0] as u64)
                } else {
                    CapRef::new(
                        recv_slot.root().bits() as u64,
                        recv_slot.path().bits() as u32,
                        recv_slot.path().depth() as u8,
                        0,
                        CapMode::MOVE,
                        Rights::ALL,
                    )
                };
                received.push(cap)?;
            }
            Ok::<_, RpcError>(())
        })?;
        let mut unpacker = CapUnpacker::new(&mut self.space, request_layout, received);
        let mut dec = Decoder::new(&bytes[..info.length() * 8], &mut unpacker);
        let args = decode(label, &mut dec)?;
        unpacker.finish()?;
        Ok((
            label,
            args,
            Sel4Reply {
                reply,
                label,
                badge,
            },
        ))
    }

    /// Raw receive: request caps remain in the received CNode.
    pub fn recv_raw<A: Wire>(
        &mut self,
        request_layout: &'static CapLayout,
        reply: sel4::ReplyAuthority,
    ) -> Result<(u64, A, Sel4Reply), RpcError> {
        self.recv_request(request_layout, reply, |_label, dec| A::decode(dec))
    }
}

impl<S: CapSpace> crate::AsyncServer for Sel4Server<S> {
    type Authority = sel4::ReplyAuthority;
    type Reply = Sel4Reply;

    fn recv_request_dynamic<A, L, F>(
        &mut self,
        layout_for: L,
        reply: Self::Authority,
        decode: F,
    ) -> Result<(u64, A, Self::Reply), RpcError>
    where
        L: FnOnce(u64) -> Result<&'static CapLayout, RpcError>,
        F: for<'a> FnOnce(u64, &mut Decoder<'a>) -> Result<A, RpcError>,
    {
        Sel4Server::recv_request_dynamic(self, layout_for, reply, decode)
    }
}

pub struct Sel4Reply {
    reply: sel4::ReplyAuthority,
    pub label: u64,
    /// Kernel-supplied identity of the endpoint capability used by the caller.
    pub badge: Word,
}

impl crate::AsyncReply for Sel4Reply {
    fn reply_error(self, error: RpcError) -> Result<(), RpcError> {
        match error {
            RpcError::Server(status) if status != 0 => self.reply_status(status),
            _ => Err(error),
        }
    }

    fn reply_wire<S: CapSpace, W: Wire>(
        self,
        space: &mut S,
        result: &W,
        result_layout: &'static CapLayout,
    ) -> Result<(), RpcError> {
        self.reply(space, result, result_layout)
    }
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
        let wire_caps = packer.finish()?;
        if wire_caps.len() > 1 {
            return Err(RpcError::TooManyCaps);
        }
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
            for index in 0..wire_caps.len() {
                let cap = wire_caps.as_slice()[index].unwrap();
                ipc.caps_or_badges_mut()[index] = cap.slot as Word;
            }
        });
        self.reply
            .send_reply(MessageInfo::new(self.label, 0, wire_caps.len(), words));
        for index in 0..wire_caps.len() {
            let cap = wire_caps.as_slice()[index].unwrap();
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
        self.reply.send_reply(MessageInfo::new(self.label, 0, 0, 1));
        Ok(())
    }
}
