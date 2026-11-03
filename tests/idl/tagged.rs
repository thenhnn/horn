#![no_std]
#[cfg(test)]
extern crate std;
pub mod protocol;

#[cfg(test)]
mod tests {
    use super::protocol::*;
    use horn_ipc::*;
    use std::vec;

    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Default)]
    struct Local(AtomicUsize);
    impl LocalCapSpace for Local {
        fn return_raw(&self, _: CapRef) {}
        fn delete_owned(&self, _: CapRef) -> Result<(), RpcError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }
    #[derive(Default)]
    struct Caps(std::collections::VecDeque<CapRef>, Arc<Local>);
    impl CapSink for Caps {
        fn push(&mut self, cap: CapRef) -> Result<(), RpcError> {
            self.0.push_back(cap);
            Ok(())
        }
    }
    impl CapSource for Caps {
        fn pop_owned(
            &mut self,
            mode: CapMode,
            rights: Rights,
        ) -> Result<OwnedCap<Endpoint>, RpcError> {
            if mode == CapMode::BORROW {
                return Err(RpcError::NotLocal);
            }
            let cap = self.pop_typed(mode, rights)?;
            // This mock owns the received descriptors; Local counts retirement
            // instead of issuing kernel operations. No raw aliases escape.
            Ok(unsafe { OwnedCap::from_raw_in(Cap::from_ref(cap), self.1.clone()) })
        }
        fn pop(&mut self) -> Result<CapRef, RpcError> {
            self.0.pop_front().ok_or(RpcError::MissingCap)
        }
    }

    fn payload() -> Payload {
        Payload {
            small: 3,
            count: 0x1122334455667788,
            tail: 9,
        }
    }
    fn cap<T: CapType>(mode: CapMode, rights: Rights) -> Cap<T> {
        Cap::new(0, 42, 64, 0, mode, rights)
    }
    fn round(value: Event, count: usize) {
        let mut bytes = vec![0xff; Event::SIZE];
        value.encode(&mut Encoder::new(&mut bytes));
        let mut caps = Caps::default();
        value.caps(&mut caps).unwrap();
        assert_eq!(caps.0.len(), count);
        assert_eq!(
            Event::decode(&mut Decoder::new(&bytes, &mut caps)).unwrap(),
            value
        );
        assert!(caps.pop().is_err());
    }

    #[test]
    fn every_variant_and_nested_record() {
        round(Event::Empty, 0);
        round(Event::Number(19), 0);
        round(Event::Data(payload()), 0);
        round(Event::Link(cap(CapMode::MOVE, Rights::WRITE)), 1);
        round(Event::Signal(cap(CapMode::COPY, Rights::ALL)), 1);
        round(
            Event::Sequence(List {
                len: 1,
                data: [payload(), Payload::default()],
            }),
            0,
        );
        round(Event::Many([cap(CapMode::MOVE, Rights::ALL); 4]), 4);
        round(
            Event::Tuple((7, cap(CapMode::COPY, Rights::ALL), payload())),
            1,
        );
        round(Event::Maybe(None), 0);
        round(Event::Maybe(Some(cap(CapMode::MOVE, Rights::ALL))), 1);
        let value = Nested {
            prefix: 5,
            event: Event::Data(payload()),
            suffix: 99,
        };
        let mut bytes = vec![0xff; Nested::SIZE];
        value.encode(&mut Encoder::new(&mut bytes));
        assert_eq!(bytes[0], 5);
        assert_eq!(&bytes[8..12], &2u32.to_le_bytes());
        assert_eq!(&bytes[24..32], &payload().count.to_le_bytes());
        assert_eq!(&bytes[Nested::SIZE - 8..], &99u64.to_le_bytes());
        assert_eq!(
            Nested::decode(&mut Decoder::new(&bytes, &mut WireCaps::default())).unwrap(),
            value
        );
    }

    struct ReplyTransport {
        bytes: std::vec::Vec<u8>,
        caps: Caps,
        error: Option<RpcError>,
        seen: Option<(CapRef, u64)>,
        request_caps: Caps,
    }

    impl ReplyTransport {
        fn new<W: Wire>(value: W) -> Self {
            let mut bytes = vec![0xff; W::SIZE];
            value.encode(&mut Encoder::new(&mut bytes));
            let mut caps = Caps::default();
            value.caps(&mut caps).unwrap();
            Self {
                bytes,
                caps,
                error: None,
                seen: None,
                request_caps: Caps::default(),
            }
        }
    }

    impl Transport for ReplyTransport {
        fn call<A: Wire, R: ReplyDecode>(
            &mut self,
            target: CapRef,
            label: u64,
            args: &A,
            _: &'static CapLayout,
            _: &'static CapLayout,
        ) -> Result<R, RpcError> {
            self.seen = Some((target, label));
            self.request_caps.0.clear();
            args.caps(&mut self.request_caps)?;
            if let Some(error) = self.error {
                return Err(error);
            }
            let result = R::decode_reply(&mut Decoder::new(&self.bytes, &mut self.caps))?;
            if !self.caps.0.is_empty() {
                return Err(RpcError::TooManyCaps);
            }
            Ok(result)
        }
        fn send<A: Wire>(
            &mut self,
            _: CapRef,
            _: u64,
            _: &A,
            _: &'static CapLayout,
        ) -> Result<(), RpcError> {
            unreachable!()
        }
    }

    // Only wire-value comparison tests deliberately hand owners back to raw
    // descriptors. Cleanup behavior is checked separately without this bridge.
    fn raw_event(
        result: Result<EventOwned, CallError<EventOwned>>,
    ) -> Result<Event, CallError<Event>> {
        result
            .map(EventOwned::into_raw)
            .map_err(|error| match error {
                CallError::Remote(value) => CallError::Remote(value.into_raw()),
                CallError::Rpc(error) => CallError::Rpc(error),
            })
    }

    #[test]
    fn receivers_use_the_wire_and_error_paths() {
        let target = cap::<ProbeTag>(CapMode::COPY, Rights::WRITE);
        let receiver = ProbeReceiver::from_cap(target);
        assert_eq!(receiver.as_cap(), target);
        let copied_view = receiver;
        let mut io = ReplyTransport::new(ProbeCheckReply { value: Ok(()) });
        assert_eq!(receiver.check(&mut io), Ok(()));
        assert_eq!(io.seen, Some((target.as_ref(), PROBE_CHECK_LABEL)));

        io = ReplyTransport::new(ProbeEchoReply {
            value: Event::Empty,
        });
        assert_eq!(
            copied_view
                .echo(&mut io, Event::Link(cap(CapMode::COPY, Rights::ALL)))
                .map(EventOwned::into_raw),
            Ok(Event::Empty)
        );
        assert_eq!(io.seen, Some((target.as_ref(), PROBE_ECHO_LABEL)));
        assert_eq!(io.request_caps.0.front().unwrap().mode, CapMode::MOVE);

        for value in [Ok(Event::Number(9)), Err(Event::Number(7))] {
            let mut io = ReplyTransport::new(ProbeTryEchoReply { value });
            assert_eq!(
                raw_event(receiver.try_echo(&mut io, Event::Empty)),
                value.map_err(CallError::Remote)
            );
            assert_eq!(io.seen, Some((target.as_ref(), PROBE_TRY_ECHO_LABEL)));
        }
        let mut io = ReplyTransport::new(ProbeCheckReply { value: Ok(()) });
        io.error = Some(RpcError::Transport);
        assert_eq!(
            receiver.check(&mut io),
            Err(CallError::Rpc(RpcError::Transport))
        );
        io.error = None;
        io.bytes[0] = 255;
        assert_eq!(
            receiver.check(&mut io),
            Err(CallError::Rpc(RpcError::InvalidValue))
        );
    }

    #[test]
    fn result_receivers_separate_application_and_rpc_errors() {
        let receiver = ProbeReceiver::from_cap(Cap::default());
        for value in [
            Ok(Event::Data(payload())),
            Err(Event::Number(7)),
            Err(Event::Signal(cap(CapMode::COPY, Rights::ALL))),
        ] {
            let mut io = ReplyTransport::new(ProbeTryEchoReply { value });
            assert_eq!(
                raw_event(receiver.try_echo(&mut io, Event::Empty)),
                value.map_err(CallError::Remote)
            );
        }
        let mut io = ReplyTransport::new(ProbeHandoffReply {
            value: Err(cap(CapMode::COPY, Rights::ALL)),
        });
        io.caps.0.front_mut().unwrap().mode = CapMode::MOVE;
        assert_eq!(
            receiver
                .handoff(&mut io)
                .map(OwnedCap::into_raw)
                .map_err(|error| match error {
                    CallError::Remote(value) => CallError::Remote(value.into_raw()),
                    CallError::Rpc(error) => CallError::Rpc(error),
                }),
            Err(CallError::Remote(cap(CapMode::COPY, Rights::ALL)))
        );
        io = ReplyTransport::new(ProbeCheckReply { value: Ok(()) });
        assert_eq!(receiver.check(&mut io), Ok(()));
        for error in [RpcError::Transport, RpcError::Server(123)] {
            io.error = Some(error);
            assert_eq!(receiver.check(&mut io), Err(CallError::Rpc(error)));
        }
        io.error = None;
        io.bytes[0] = 255;
        assert_eq!(
            receiver.check(&mut io),
            Err(CallError::Rpc(RpcError::InvalidValue))
        );
        io.bytes[0] = 0;
        *io.bytes.last_mut().unwrap() = 1;
        assert_eq!(
            receiver.check(&mut io),
            Err(CallError::Rpc(RpcError::InvalidValue))
        );
        io = ReplyTransport::new(ProbeCheckReply {
            value: Err(Event::Empty),
        });
        io.bytes[8] = 255;
        assert_eq!(
            receiver.check(&mut io),
            Err(CallError::Rpc(RpcError::InvalidValue))
        );
        io.bytes.pop();
        assert_eq!(
            receiver.check(&mut io),
            Err(CallError::Rpc(RpcError::Truncated))
        );
    }

    #[test]
    fn results_nest_in_records_and_containers() {
        let value = Outcomes {
            primary: Err(Event::Signal(cap(CapMode::COPY, Rights::ALL))),
            nested: Ok(Err(Event::Data(payload()))),
            optional: Some(Err(Event::Link(Cap::new(
                0,
                43,
                64,
                0,
                CapMode::MOVE,
                Rights::WRITE,
            )))),
            array: [Ok(11), Err(Event::Empty)],
            sequence: List {
                len: 1,
                data: [Err(Event::Number(4)), Ok(0)],
            },
        };
        let mut transport = ReplyTransport::new(ProbeOutcomesReply { value });
        assert_eq!(&transport.bytes[..4], &1u32.to_le_bytes());
        assert_eq!(transport.caps.0.len(), 2);
        let receiver = ProbeReceiver::from_cap(Cap::default());
        assert_eq!(
            receiver
                .outcomes(&mut transport, Outcomes::default())
                .unwrap()
                .into_raw(),
            value
        );
    }

    #[test]
    fn decoded_results_own_caps_on_success_and_remote_error() {
        let receiver = ProbeReceiver::from_cap(Cap::default());
        let mut io = ReplyTransport::new(ProbeEchoReply {
            value: Event::Link(cap(CapMode::MOVE, Rights::WRITE)),
        });
        let local = io.caps.1.clone();
        let value = receiver.echo(&mut io, Event::Empty).unwrap();
        assert!(matches!(&value, EventOwned::Link(_)));
        assert_eq!(local.0.load(Ordering::Relaxed), 0);
        drop(value);
        assert_eq!(local.0.load(Ordering::Relaxed), 1);

        let mut io = ReplyTransport::new(ProbeTryEchoReply {
            value: Err(Event::Signal(cap(CapMode::COPY, Rights::ALL))),
        });
        let local = io.caps.1.clone();
        let error = receiver.try_echo(&mut io, Event::Empty).unwrap_err();
        assert!(matches!(&error, CallError::Remote(EventOwned::Signal(_))));
        assert_eq!(local.0.load(Ordering::Relaxed), 0);
        drop(error);
        assert_eq!(local.0.load(Ordering::Relaxed), 1);
    }

    fn nested_owners() -> Nested {
        Nested {
            prefix: 5,
            event: Event::Many(core::array::from_fn(|i| {
                Cap::new(0, 42 + i as u32, 64, 0, CapMode::MOVE, Rights::ALL)
            })),
            suffix: 99,
        }
    }

    #[test]
    fn failed_nested_and_array_decode_retires_partial_owners() {
        let value = nested_owners();
        let mut bytes = vec![0; Nested::SIZE];
        value.encode(&mut Encoder::new(&mut bytes));
        let mut caps = Caps::default();
        value.caps(&mut caps).unwrap();
        let local = caps.1.clone();
        // Decode all four array owners, then fail while reading the suffix.
        assert!(
            NestedOwned::decode_reply(&mut Decoder::new(&bytes[..bytes.len() - 1], &mut caps,))
                .is_err()
        );
        assert_eq!(local.0.load(Ordering::Relaxed), 4);

        let mut caps = Caps::default();
        value.caps(&mut caps).unwrap();
        caps.0.truncate(2);
        let local = caps.1.clone();
        // Fail inside the array after accepting just two capabilities.
        assert_eq!(
            NestedOwned::decode_reply(&mut Decoder::new(&bytes, &mut caps)),
            Err(RpcError::MissingCap)
        );
        assert_eq!(local.0.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn finish_failure_retires_an_otherwise_decoded_reply() {
        let receiver = ProbeReceiver::from_cap(Cap::default());
        let mut io = ReplyTransport::new(ProbeEchoReply {
            value: Event::Signal(cap(CapMode::COPY, Rights::ALL)),
        });
        io.caps
            .0
            .push_back(CapRef::new(0, 43, 64, 0, CapMode::MOVE, Rights::ALL));
        let local = io.caps.1.clone();
        assert_eq!(
            receiver.echo(&mut io, Event::Empty),
            Err(RpcError::TooManyCaps)
        );
        assert_eq!(local.0.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn rejects_unknown_truncated_and_nonzero_inactive_storage() {
        let mut bytes = vec![0; Event::SIZE];
        bytes[..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Event::decode(&mut Decoder::new(&bytes, &mut WireCaps::default())).is_err());
        bytes[..4].fill(0);
        bytes[Event::SIZE - 1] = 1;
        assert!(Event::decode(&mut Decoder::new(&bytes, &mut WireCaps::default())).is_err());
        assert!(
            Event::decode(&mut Decoder::new(
                &bytes[..Event::SIZE - 1],
                &mut WireCaps::default()
            ))
            .is_err()
        );
    }
}
