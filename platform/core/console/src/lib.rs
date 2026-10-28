//! Bounded single-consumer console state. Providers own hardware and notification caps.
#![no_std]
pub mod protocol {
    include!(concat!(env!("OUT_DIR"), "/protocol.rs"));
}
use horn_ipc::List;
use protocol::*;
pub const CLIENT_BADGE: u64 = 16;
pub const CAPACITY: usize = 4096;
const EVENTS: u32 = 15;
#[cfg(test)]
mod tests;
struct Ring {
    data: [u8; CAPACITY],
    head: usize,
    len: usize,
}
impl Ring {
    const fn new() -> Self {
        Self {
            data: [0; CAPACITY],
            head: 0,
            len: 0,
        }
    }
    fn push(&mut self, bytes: &[u8]) -> usize {
        let count = bytes.len().min(CAPACITY - self.len);
        for (i, &b) in bytes[..count].iter().enumerate() {
            self.data[(self.head + self.len + i) % CAPACITY] = b;
        }
        self.len += count;
        count
    }
    fn pop(&mut self, bytes: &mut [u8]) -> usize {
        let count = bytes.len().min(self.len);
        for (i, b) in bytes[..count].iter_mut().enumerate() {
            *b = self.data[(self.head + i) % CAPACITY];
        }
        self.head = (self.head + count) % CAPACITY;
        self.len -= count;
        count
    }
}
pub struct Console {
    rx: Ring,
    tx: Ring,
    size: Option<ConsoleSize>,
    generation: u64,
    closed: bool,
    armed: ConsoleEvents,
    since: u64,
}
impl Default for Console {
    fn default() -> Self {
        Self::new()
    }
}
impl Console {
    pub const fn new() -> Self {
        Self {
            rx: Ring::new(),
            tx: Ring::new(),
            size: None,
            generation: 0,
            closed: false,
            armed: ConsoleEvents(0),
            since: 0,
        }
    }
    pub fn status(&self) -> ConsoleStatus {
        let mut events = 0;
        if self.rx.len != 0 {
            events |= ConsoleEvents::READABLE.0;
        }
        if !self.closed && self.tx.len < CAPACITY {
            events |= ConsoleEvents::WRITABLE.0;
        }
        if self.closed {
            events |= ConsoleEvents::CLOSED.0;
        }
        ConsoleStatus {
            events: ConsoleEvents(events),
            size_generation: self.generation,
        }
    }
    pub fn size(&self) -> ConsoleGeometry {
        ConsoleGeometry {
            size: self.size,
            generation: self.generation,
        }
    }
    pub fn set_size(&mut self, size: Option<ConsoleSize>) -> Result<(), ConsoleError> {
        if size.is_some_and(|s| s.rows == 0 || s.columns == 0) {
            return Err(ConsoleError::Invalid);
        }
        let same = match (self.size, size) {
            (None, None) => true,
            (Some(a), Some(b)) => a.rows == b.rows && a.columns == b.columns,
            _ => false,
        };
        if !same {
            self.size = size;
            self.generation = self
                .generation
                .checked_add(1)
                .expect("console generation exhausted");
        }
        Ok(())
    }
    pub fn close(&mut self) {
        self.closed = true;
    }
    pub fn read(&mut self, maximum: u32) -> Result<List<u8, 256>, ConsoleError> {
        if maximum == 0 || maximum > 256 {
            return Err(ConsoleError::Invalid);
        }
        if self.rx.len == 0 {
            return Err(if self.closed {
                ConsoleError::Closed
            } else {
                ConsoleError::WouldBlock
            });
        }
        let mut data = List::default();
        data.len = self.rx.pop(&mut data.data[..maximum as usize]) as _;
        Ok(data)
    }
    pub fn write(&mut self, data: &[u8]) -> Result<u32, ConsoleError> {
        if self.closed {
            return Err(ConsoleError::Closed);
        }
        if data.len() > 256 {
            return Err(ConsoleError::Invalid);
        }
        if data.is_empty() {
            return Ok(0);
        }
        let count = self.tx.push(data);
        if count == 0 {
            Err(ConsoleError::WouldBlock)
        } else {
            Ok(count as _)
        }
    }
    pub fn rx_space(&self) -> usize {
        CAPACITY - self.rx.len
    }
    pub fn receive(&mut self, data: &[u8]) -> usize {
        if self.closed { 0 } else { self.rx.push(data) }
    }
    pub fn transmit(&mut self, data: &mut [u8]) -> usize {
        self.tx.pop(data)
    }
    pub fn arm(
        &mut self,
        interests: ConsoleEvents,
        since: u64,
    ) -> Result<ConsoleStatus, ConsoleError> {
        if interests.0 & !EVENTS != 0 || since > self.generation {
            return Err(ConsoleError::Invalid);
        }
        self.armed = ConsoleEvents(if interests.0 == 0 {
            0
        } else {
            interests.0 | ConsoleEvents::CLOSED.0
        });
        self.since = since;
        Ok(self.status())
    }
    /// Consume one wake. Call after rearm and every state change. Never busy-resignal.
    pub fn take_wake(&mut self) -> bool {
        let mut events = self.status().events.0;
        if self.generation != self.since {
            events |= ConsoleEvents::SIZE_CHANGED.0;
        }
        if events & self.armed.0 == 0 {
            return false;
        }
        self.armed = ConsoleEvents(0);
        true
    }
}
