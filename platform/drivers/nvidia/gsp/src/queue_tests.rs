// SPDX-License-Identifier: GPL-2.0-only
//! Ring arithmetic and fault guards only, not a firmware/RPC peer model.
use crate::{Error, Result, boot::*, queue::*, wire};
use alloc::{vec, vec::Vec};
struct Words {
    bytes: Vec<u8>,
    fail: Option<usize>,
    quarantines: usize,
}
impl Words {
    fn new() -> Self {
        let mut bytes = vec![0; QUEUE_BYTES];
        queues(Span::new(0x100000, QUEUE_BYTES).unwrap(), &mut bytes).unwrap();
        Self {
            bytes,
            fail: None,
            quarantines: 0,
        }
    }
}
impl Memory for Words {
    fn bytes(&self) -> usize {
        self.bytes.len()
    }
    fn read32(&mut self, offset: usize) -> Result<u32> {
        Ok(u32::from_le_bytes(
            self.bytes[offset..offset + 4].try_into().unwrap(),
        ))
    }
    fn write32(&mut self, offset: usize, value: u32) -> Result<()> {
        if self.fail == Some(offset) {
            return Err(Error::Backend);
        }
        self.bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        Ok(())
    }
    fn acquire(&mut self) {}
    fn release(&mut self) {}
    fn quarantine(&mut self) {
        self.quarantines += 1;
    }
}
#[test]
fn invalid_peer_pointer_poison_precedes_ring_access() {
    let mut words = Words::new();
    words.write32(GSP_HEADER + 16, 63).unwrap();
    // SAFETY: exclusively owned inert test bytes, no device or concurrent access.
    let mut queue = unsafe { Queue::attach(words) }.unwrap();
    assert!(matches!(queue.try_receive(), Err(Error::Malformed)));
    assert!(matches!(queue.try_receive(), Err(Error::Quarantined)));
}
#[test]
fn publication_failure_never_returns_a_doorbell_ticket() {
    let mut words = Words::new();
    words.fail = Some(CPU_HEADER + 16);
    let mut queue = unsafe { Queue::attach(words) }.unwrap();
    assert!(matches!(
        queue.try_send(65, 0, [&[], &[]]),
        Err(Error::Backend)
    ));
    assert!(matches!(
        queue.try_send(65, 0, [&[], &[]]),
        Err(Error::Quarantined)
    ));
}
#[test]
fn full_ring_is_backpressure_not_an_implicit_retry_or_fault() {
    let mut queue = unsafe { Queue::attach(Words::new()) }.unwrap();
    for i in 0..62 {
        let publication = queue
            .try_send(wire::STATIC_INFO, 0, [&[], &[]])
            .unwrap()
            .unwrap();
        assert_eq!(publication.transport_sequence, i);
        assert_eq!(publication.pages, 1);
    }
    assert!(
        queue
            .try_send(wire::STATIC_INFO, 0, [&[], &[]])
            .unwrap()
            .is_none()
    );
    assert!(queue.try_receive().unwrap().is_none());
}
