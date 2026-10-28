//! Bounded packet IPC adapter. smoltcp sees Ethernet, never device/DMA authority.
use crate::{Io, cap};
use alloc::{collections::VecDeque, vec, vec::Vec};
use horn_ipc::*;
use horn_net::protocol::*;
use smoltcp::{
    phy::{self, DeviceCapabilities, Medium},
    time::Instant,
};
pub struct Device {
    rx: VecDeque<Vec<u8>>,
    tx: VecDeque<Vec<u8>>,
    pub generation: u64,
}
impl Device {
    pub fn new() -> Self {
        Self {
            rx: VecDeque::new(),
            tx: VecDeque::new(),
            generation: 0,
        }
    }
    pub fn pending_tx(&self) -> bool {
        !self.tx.is_empty()
    }
    pub fn receive_rpc(&mut self, io: &mut Io) {
        let mut ethernet = EthernetInterfaceClient::new(&mut *io, cap(NETD_CAPS_ETHERNET_SLOT));
        self.generation = ethernet.info().unwrap().generation;
        for _ in 0..8 {
            if self.rx.len() == 16 {
                break;
            }
            let first = match ethernet.receive() {
                Ok(chunk) => chunk,
                Err(CallError::Remote(NetError::WouldBlock)) => break,
                Err(e) => panic!("Ethernet RX: {:?}", e),
            };
            let mut packet = Vec::with_capacity(1514);
            let mut chunk = first;
            loop {
                packet.extend_from_slice(&chunk.data.data[..chunk.data.len as usize]);
                assert!(packet.len() + chunk.remaining as usize <= 1514);
                if chunk.remaining == 0 {
                    break;
                }
                assert!(chunk.data.len != 0);
                chunk = ethernet.receive().unwrap();
            }
            assert!((14..=1514).contains(&packet.len()));
            self.rx.push_back(packet);
        }
    }
    pub fn transmit_rpc(&mut self, io: &mut Io) {
        let mut ethernet = EthernetInterfaceClient::new(io, cap(NETD_CAPS_ETHERNET_SLOT));
        while let Some(packet) = self.tx.front() {
            match ethernet.begin(packet.len() as u32) {
                Ok(()) => {}
                Err(CallError::Remote(NetError::WouldBlock)) => break,
                Err(e) => panic!("Ethernet TX: {:?}", e),
            }
            for chunk in packet.chunks(512) {
                let mut data = List::default();
                data.len = chunk.len() as _;
                data.data[..chunk.len()].copy_from_slice(chunk);
                ethernet.append(data).unwrap();
            }
            ethernet.commit().unwrap();
            self.tx.pop_front();
        }
    }
}
pub struct Rx(Vec<u8>);
impl phy::RxToken for Rx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}
pub struct Tx<'a>(&'a mut VecDeque<Vec<u8>>);
impl phy::TxToken for Tx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        assert!(len <= 1514 && self.0.len() < 16);
        let mut packet = vec![0; len];
        let result = f(&mut packet);
        self.0.push_back(packet);
        result
    }
}
impl phy::Device for Device {
    type RxToken<'a> = Rx;
    type TxToken<'a> = Tx<'a>;
    fn receive(&mut self, _: Instant) -> Option<(Rx, Tx<'_>)> {
        if self.tx.len() == 16 {
            return None;
        }
        Some((Rx(self.rx.pop_front()?), Tx(&mut self.tx)))
    }
    fn transmit(&mut self, _: Instant) -> Option<Tx<'_>> {
        if self.tx.len() == 16 {
            None
        } else {
            Some(Tx(&mut self.tx))
        }
    }
    fn capabilities(&self) -> DeviceCapabilities {
        let mut c = DeviceCapabilities::default();
        c.medium = Medium::Ethernet;
        c.max_transmission_unit = 1514;
        c.max_burst_size = Some(8);
        c
    }
}
