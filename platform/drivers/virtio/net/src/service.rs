//! Bounded frame IPC for one explicitly granted network-stack consumer.
use alloc::{collections::VecDeque, vec::Vec};
use horn_driver::virtio;
use horn_ipc::*;
use horn_net::protocol::*;
pub struct Service {
    pub rx: VecDeque<Vec<u8>>,
    pub tx: VecDeque<Vec<u8>>,
    cursor: usize,
    staging: Option<(usize, Vec<u8>)>,
    mac: [u8; 6],
    pub generation: u64,
    note: Option<Cap<Notification>>,
    armed: Option<u64>,
    pub closed: bool,
}
impl Service {
    pub fn new(mac: [u8; 6]) -> Self {
        Self {
            rx: VecDeque::new(),
            tx: VecDeque::new(),
            cursor: 0,
            staging: None,
            mac,
            generation: 1,
            note: None,
            armed: None,
            closed: false,
        }
    }
    pub fn changed(&mut self) {
        self.generation = self.generation.checked_add(1).unwrap();
    }
    pub fn notify(&mut self) {
        if self
            .armed
            .is_some_and(|g| g != self.generation || self.closed)
        {
            self.armed = None;
            if let Some(note) = self.note {
                virtio::with(|s| {
                    s.transport()
                        .space
                        .cap::<sel4::cap_type::Notification>(note.as_ref())
                        .unwrap()
                        .signal()
                });
            }
        }
    }
    fn open(&self) -> Result<(), NetError> {
        if self.closed {
            Err(NetError::Closed)
        } else {
            Ok(())
        }
    }
}
impl EthernetInterface for Service {
    fn info(&mut self) -> Result<EthernetInfo, NetError> {
        self.open()?;
        let mut mac = List::default();
        mac.len = 6;
        mac.data = self.mac;
        Ok(EthernetInfo {
            mac,
            mtu: 1500,
            generation: self.generation,
        })
    }
    fn receive(&mut self) -> Result<FrameChunk, NetError> {
        self.open()?;
        let frame = self.rx.front().ok_or(NetError::WouldBlock)?;
        let n = (frame.len() - self.cursor).min(512);
        let mut data = List::default();
        data.len = n as _;
        data.data[..n].copy_from_slice(&frame[self.cursor..self.cursor + n]);
        self.cursor += n;
        let remaining = (frame.len() - self.cursor) as u32;
        if remaining == 0 {
            self.rx.pop_front();
            self.cursor = 0;
        }
        Ok(FrameChunk { data, remaining })
    }
    fn begin(&mut self, length: u32) -> Result<(), NetError> {
        self.open()?;
        if !(14..=1514).contains(&length) {
            return Err(NetError::Invalid);
        }
        if self.staging.is_some() || self.tx.len() == 8 {
            return Err(NetError::WouldBlock);
        }
        self.staging = Some((length as usize, Vec::with_capacity(length as usize)));
        Ok(())
    }
    fn append(&mut self, data: List<u8, 512>) -> Result<(), NetError> {
        self.open()?;
        let (total, buffer) = self.staging.as_mut().ok_or(NetError::Invalid)?;
        if buffer.len() + data.len as usize > *total {
            return Err(NetError::Invalid);
        }
        buffer.extend_from_slice(&data.data[..data.len as usize]);
        Ok(())
    }
    fn commit(&mut self) -> Result<(), NetError> {
        self.open()?;
        let (total, buffer) = self.staging.as_ref().ok_or(NetError::Invalid)?;
        if *total != buffer.len() {
            return Err(NetError::Invalid);
        }
        let (_, buffer) = self.staging.take().unwrap();
        self.tx.push_back(buffer);
        Ok(())
    }
    fn watch(&mut self, signal: Cap<Notification>, generation: u64) -> Result<(), NetError> {
        self.open()?;
        if generation > self.generation {
            return Err(NetError::Invalid);
        }
        if signal.as_ref().unwrapped_badge().is_some() {
            return Err(NetError::Denied);
        }
        let next = virtio::with(|s| {
            let root = s.transport().space.root;
            horn_supervisor::client::copy(s.transport(), root, signal)
        });
        if let Some(old) = self.note.replace(next) {
            virtio::with(|s| s.transport().space.delete(old.as_ref()).unwrap());
        }
        self.armed = Some(generation);
        Ok(())
    }
}
