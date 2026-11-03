#![no_std]
#[cfg(test)]
extern crate std;
pub mod protocol;

#[cfg(test)]
mod tests {
    use super::protocol::*;
    use horn_ipc::*;
    use std::vec;

    #[derive(Default)]
    struct Caps(std::collections::VecDeque<CapRef>);
    impl CapSink for Caps {
        fn push(&mut self, cap: CapRef) -> Result<(), RpcError> {
            self.0.push_back(cap);
            Ok(())
        }
    }
    impl CapSource for Caps {
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
        fn call<A: Wire, R: Wire>(
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
            let result = R::decode(&mut Decoder::new(&self.bytes, &mut self.caps))?;
            assert!(self.caps.0.is_empty());
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

    #[test]
    fn receivers_use_the_existing_client_wire_and_error_paths() {
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
            copied_view.echo(&mut io, Event::Link(cap(CapMode::COPY, Rights::ALL))),
            Ok(Event::Empty)
        );
        assert_eq!(io.seen, Some((target.as_ref(), PROBE_ECHO_LABEL)));
        assert_eq!(io.request_caps.0.front().unwrap().mode, CapMode::MOVE);

        for value in [Ok(Event::Number(9)), Err(Event::Number(7))] {
            let mut receiver_io = ReplyTransport::new(ProbeTryEchoReply { value });
            let mut client =
                ProbeClient::new(ReplyTransport::new(ProbeTryEchoReply { value }), target);
            assert_eq!(
                receiver.try_echo(&mut receiver_io, Event::Empty),
                client.try_echo(Event::Empty)
            );
            assert_eq!(receiver_io.seen, client.transport.seen);
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
    fn result_clients_separate_application_and_rpc_errors() {
        for value in [
            Ok(Event::Data(payload())),
            Err(Event::Number(7)),
            Err(Event::Signal(cap(CapMode::COPY, Rights::ALL))),
        ] {
            let mut client = ProbeClient::new(
                ReplyTransport::new(ProbeTryEchoReply { value }),
                Cap::default(),
            );
            assert_eq!(
                client.try_echo(Event::Empty),
                value.map_err(CallError::Remote)
            );
        }
        let mut transport = ReplyTransport::new(ProbeHandoffReply {
            value: Err(cap(CapMode::COPY, Rights::ALL)),
        });
        transport.caps.0.front_mut().unwrap().mode = CapMode::MOVE;
        let mut client = ProbeClient::new(transport, Cap::default());
        assert_eq!(
            client.handoff(),
            Err(CallError::Remote(cap(CapMode::COPY, Rights::ALL)))
        );
        let mut client = ProbeClient::new(
            ReplyTransport::new(ProbeCheckReply { value: Ok(()) }),
            Cap::default(),
        );
        assert_eq!(client.check(), Ok(()));
        for error in [RpcError::Transport, RpcError::Server(123)] {
            client.transport.error = Some(error);
            assert_eq!(client.check(), Err(CallError::Rpc(error)));
        }
        client.transport.error = None;
        client.transport.bytes[0] = 255;
        assert_eq!(client.check(), Err(CallError::Rpc(RpcError::InvalidValue)));
        client.transport.bytes[0] = 0;
        *client.transport.bytes.last_mut().unwrap() = 1;
        assert_eq!(client.check(), Err(CallError::Rpc(RpcError::InvalidValue)));
        client.transport = ReplyTransport::new(ProbeCheckReply {
            value: Err(Event::Empty),
        });
        client.transport.bytes[8] = 255;
        assert_eq!(client.check(), Err(CallError::Rpc(RpcError::InvalidValue)));
        client.transport.bytes.pop();
        assert_eq!(client.check(), Err(CallError::Rpc(RpcError::Truncated)));
    }

    #[test]
    fn results_nest_in_records_and_containers() {
        let value = Outcomes {
            primary: Err(Event::Signal(cap(CapMode::COPY, Rights::ALL))),
            nested: Ok(Err(Event::Data(payload()))),
            optional: Some(Err(Event::Link(cap(CapMode::MOVE, Rights::WRITE)))),
            array: [Ok(11), Err(Event::Empty)],
            sequence: List {
                len: 1,
                data: [Err(Event::Number(4)), Ok(0)],
            },
        };
        let transport = ReplyTransport::new(ProbeOutcomesReply { value });
        assert_eq!(&transport.bytes[..4], &1u32.to_le_bytes());
        assert_eq!(transport.caps.0.len(), 2);
        let mut client = ProbeClient::new(transport, Cap::default());
        assert_eq!(client.outcomes(Outcomes::default()).unwrap(), value);
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
