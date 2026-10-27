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

    fn memory(slot: u32) -> Cap<VmMemoryObjectTag> {
        Cap::new(0, slot, 64, 0, CapMode::BORROW, Rights::ALL)
    }

    fn round<T: Wire + Copy + PartialEq + core::fmt::Debug>(value: T, cap_count: usize) {
        let mut bytes = vec![0xff; T::SIZE];
        let mut out = Encoder::new(&mut bytes);
        value.encode(&mut out);
        let mut caps = Caps::default();
        value.caps(&mut caps).unwrap();
        assert_eq!(caps.0.len(), cap_count);
        let decoded = T::decode(&mut Decoder::new(&bytes, &mut caps)).unwrap();
        assert_eq!(decoded, value);
        assert!(caps.pop().is_err());
    }

    #[test]
    fn mapping_batches_cover_empty_mixed_and_full_capability_payloads() {
        for len in 0..=16 {
            let mut updates = List::default();
            updates.len = len;
            for index in 0..len as usize {
                let range = VmRange {
                    offset: index as u64 * 4096,
                    length: 4096,
                };
                updates.data[index] = match index % 4 {
                    0 => VmUpdate::Prepare(range),
                    1 => VmUpdate::Map(VmMap {
                        offset: range.offset,
                        length: range.length,
                        memory: memory(index as u32 + 1),
                        memory_offset: 0,
                        protection: VmProtection::READ,
                    }),
                    2 => VmUpdate::Protect(VmProtect {
                        offset: range.offset,
                        length: range.length,
                        protection: VmProtection::READ,
                    }),
                    _ => VmUpdate::Unmap(range),
                };
            }
            round(
                VmregionApplyArgs {
                    updates: Parcel::inline(updates),
                },
                (len + 2) as usize / 4,
            );
        }
        let mut updates = List::default();
        updates.len = 16;
        for index in 0..16 {
            updates.data[index] = VmUpdate::Map(VmMap {
                offset: index as u64 * 4096,
                length: 4096,
                memory: memory(index as u32 + 1),
                memory_offset: 0,
                protection: VmProtection::READ,
            });
        }
        round(
            VmregionApplyArgs {
                updates: Parcel::inline(updates),
            },
            16,
        );
        assert!(VmregionApplyArgs::SIZE <= MAX_MESSAGE_BYTES);
    }

    #[test]
    fn faults_are_values_and_resolution_caps_are_active_only() {
        let mut faults = List::default();
        faults.len = 16;
        for index in 0..16 {
            faults.data[index] = VmFault {
                id: index as u64 + 1,
                cookie: 42,
                offset: index as u64 * 4096,
                access: VmProtection::WRITE,
            };
        }
        round(
            VmpagerDrainReply {
                value: Ok(Parcel::inline(faults)),
            },
            0,
        );
        let mut replies = List::default();
        replies.len = 16;
        for index in 0..16 {
            replies.data[index] = if index % 2 == 0 {
                VmFaultReply::Map(VmFaultMap {
                    id: index as u64 + 1,
                    memory: memory(index as u32 + 1),
                    memory_offset: 0,
                    protection: VmProtection::READ,
                })
            } else {
                VmFaultReply::Reject(VmFaultReject {
                    id: index as u64 + 1,
                    reason: VmFaultReason::Exhausted,
                })
            };
        }
        round(
            VmpagerResolveArgs {
                replies: Parcel::inline(replies),
            },
            8,
        );
        assert!(VmpagerResolveArgs::SIZE <= MAX_MESSAGE_BYTES);
        assert!(VmpagerDrainReply::SIZE + 4 <= MAX_MESSAGE_BYTES);
    }

    struct Unsupported;
    impl Transport for Unsupported {
        fn call<A: Wire, R: Wire>(
            &mut self,
            _: CapRef,
            _: u64,
            _: &A,
            _: &'static CapLayout,
            _: &'static CapLayout,
        ) -> Result<R, RpcError> {
            let reply = VmfactoryCreateAddressSpaceReply {
                value: Err(VmError::Unsupported),
            };
            let mut bytes = [0; VmfactoryCreateAddressSpaceReply::SIZE];
            reply.encode(&mut Encoder::new(&mut bytes));
            R::decode(&mut Decoder::new(&bytes, &mut WireCaps::default()))
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
    fn unsupported_is_an_application_error_and_batch_errors_keep_progress() {
        let mut client = VmFactoryClient::new(Unsupported, Cap::default());
        assert_eq!(
            client.create_address_space(),
            Err(CallError::Remote(VmError::Unsupported))
        );
        round(
            VmregionApplyReply {
                value: Err(VmBatchError {
                    completed: 3,
                    error: VmError::Unsupported,
                }),
            },
            0,
        );
        round(
            VmpagerResolveReply {
                value: Err(VmBatchError {
                    completed: 0,
                    error: VmError::Quarantined,
                }),
            },
            0,
        );
    }
}
