//! Stable queue buffers and nonblocking upstream net operations.
use alloc::{boxed::Box, vec::Vec};
use core::{mem::ManuallyDrop, ptr::NonNull};
use horn_driver::virtio::{self, CapabilityHal};
use horn_virtio::{
    IommuPciTransport,
    virtio_drivers::{device::net::VirtIONetRaw, transport::pci::PciTransport},
};
type Raw = VirtIONetRaw<CapabilityHal, IommuPciTransport<PciTransport>, 8>;
pub struct Hardware {
    raw: ManuallyDrop<Raw>,
    common: NonNull<u8>,
    rx: ManuallyDrop<[Box<[u8; 2048]>; 4]>,
    rx_tokens: [u16; 4],
    tx: ManuallyDrop<Box<[u8; 2048]>>,
    tx_token: Option<u16>,
    tx_len: usize,
}
impl Hardware {
    pub fn new(transport: IommuPciTransport<PciTransport>, common: NonNull<u8>) -> Self {
        let raw = ManuallyDrop::new(Raw::new(transport).expect("virtio net queues"));
        let rx = ManuallyDrop::new(core::array::from_fn(|_| Box::new([0; 2048])));
        let mut this = Self {
            raw,
            common,
            rx,
            rx_tokens: [0; 4],
            tx: ManuallyDrop::new(Box::new([0; 2048])),
            tx_token: None,
            tx_len: 0,
        };
        for i in 0..4 {
            this.rx_tokens[i] =
                unsafe { this.raw.receive_begin(this.rx[i].as_mut_slice()) }.unwrap();
        }
        this
    }
    pub fn mac(&self) -> [u8; 6] {
        self.raw.mac_address()
    }
    pub fn interrupt(&mut self) {
        self.raw.ack_interrupt();
    }
    pub fn receive(&mut self) -> Option<Vec<u8>> {
        let token = self.raw.poll_receive()?;
        let i = self
            .rx_tokens
            .iter()
            .position(|t| *t == token)
            .expect("RX token");
        let (header, length) =
            unsafe { self.raw.receive_complete(token, self.rx[i].as_mut_slice()) }.unwrap();
        assert!(header + length <= 2048, "device RX length");
        let packet = if (14..=1514).contains(&length) {
            Some(self.rx[i][header..header + length].to_vec())
        } else {
            Some(Vec::new())
        };
        self.rx_tokens[i] = unsafe { self.raw.receive_begin(self.rx[i].as_mut_slice()) }.unwrap();
        packet
    }
    pub fn complete(&mut self) -> bool {
        if let Some(token) = self.tx_token {
            if self.raw.poll_transmit() == Some(token) {
                unsafe { self.raw.transmit_complete(token, &self.tx[..self.tx_len]) }.unwrap();
                self.tx_token = None;
                return true;
            }
        }
        false
    }
    pub fn writable(&self) -> bool {
        self.tx_token.is_none() && self.raw.can_send()
    }
    pub fn send(&mut self, packet: &[u8]) {
        assert!(self.writable() && (14..=1514).contains(&packet.len()));
        let header = self.raw.fill_buffer_header(self.tx.as_mut_slice()).unwrap();
        self.tx[header..header + packet.len()].copy_from_slice(packet);
        self.tx_len = header + packet.len();
        self.tx_token = Some(unsafe { self.raw.transmit_begin(&self.tx[..self.tx_len]) }.unwrap());
    }
    pub fn shutdown(mut self) {
        virtio::reset(self.common);
        unsafe {
            ManuallyDrop::drop(&mut self.raw);
            virtio::with(|s| s.discard_shared());
            ManuallyDrop::drop(&mut self.rx);
            ManuallyDrop::drop(&mut self.tx);
        }
    }
}
