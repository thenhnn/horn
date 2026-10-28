//! Single-port nonblocking console using upstream transport and VirtQueue.
//! Upstream VirtIOConsole::send_bytes spins; it cannot serve a single-threaded RPC loop.
use alloc::boxed::Box;
use core::mem::ManuallyDrop;
use virtio_drivers::{
    Hal, Result,
    queue::VirtQueue,
    transport::{DeviceStatus, Transport},
};
bitflags::bitflags! {
    #[derive(Debug,Clone,Copy)]
    struct Features:u64 {const SIZE=1;const VERSION_1=1<<32;}
}
const PAGE: usize = 4096;
pub struct Console<H: Hal, T: Transport> {
    // No implicit DMA teardown. Abandoned instances retain backing until owner cleanup.
    transport: ManuallyDrop<T>,
    rxq: ManuallyDrop<VirtQueue<H, 2>>,
    txq: ManuallyDrop<VirtQueue<H, 2>>,
    rx: ManuallyDrop<Box<[u8; PAGE]>>,
    tx: ManuallyDrop<Box<[u8; PAGE]>>,
    rx_token: Option<u16>,
    tx_token: Option<u16>,
    cursor: usize,
    length: usize,
    tx_length: usize,
    features: Features,
}
impl<H: Hal, T: Transport> Console<H, T> {
    pub fn new(mut transport: T) -> Result<Self> {
        let features = transport.begin_init(Features::SIZE | Features::VERSION_1);
        let rxq = VirtQueue::new(&mut transport, 0, false, false)?;
        let txq = VirtQueue::new(&mut transport, 1, false, false)?;
        transport.finish_init();
        let mut this = Self {
            transport: ManuallyDrop::new(transport),
            rxq: ManuallyDrop::new(rxq),
            txq: ManuallyDrop::new(txq),
            rx: ManuallyDrop::new(Box::new([0; PAGE])),
            tx: ManuallyDrop::new(Box::new([0; PAGE])),
            rx_token: None,
            tx_token: None,
            cursor: 0,
            length: 0,
            tx_length: 0,
            features,
        };
        this.arm_receive()?;
        Ok(this)
    }
    fn arm_receive(&mut self) -> Result<()> {
        if self.rx_token.is_none() && self.cursor == self.length {
            // SAFETY: stable owned box, no other descriptor or CPU access until completion.
            self.rx_token = Some(unsafe { self.rxq.add(&[], &mut [self.rx.as_mut_slice()]) }?);
            if self.rxq.should_notify() {
                self.transport.notify(0);
            }
        }
        Ok(())
    }
    pub fn poll(&mut self) -> Result<()> {
        if let Some(token) = self.rx_token {
            if self.rxq.peek_used() == Some(token) {
                let len = unsafe { self.rxq.pop_used(token, &[], &mut [self.rx.as_mut_slice()]) }?;
                assert!(len as usize <= PAGE, "invalid console RX length");
                self.rx_token = None;
                self.cursor = 0;
                self.length = len as usize;
                self.arm_receive()?;
            }
        }
        if let Some(token) = self.tx_token {
            if self.txq.peek_used() == Some(token) {
                // SAFETY: exactly the same stable slice submitted by send().
                unsafe {
                    self.txq
                        .pop_used(token, &[&self.tx[..self.tx_length]], &mut [])
                }?;
                self.tx_token = None;
            }
        }
        Ok(())
    }
    pub fn interrupt(&mut self) -> Result<()> {
        self.transport.ack_interrupt();
        self.poll()
    }
    pub fn size(&self) -> Result<Option<(u32, u32)>> {
        if !self.features.contains(Features::SIZE) {
            return Ok(None);
        }
        self.transport.read_consistent(|| {
            let columns = self.transport.read_config_space::<u16>(0)? as u32;
            let rows = self.transport.read_config_space::<u16>(2)? as u32;
            Ok(if rows == 0 || columns == 0 {
                None
            } else {
                Some((rows, columns))
            })
        })
    }
    pub fn receive(&mut self, out: &mut [u8]) -> Result<usize> {
        let count = out.len().min(self.length - self.cursor);
        if count == 0 {
            return Ok(0);
        }
        out[..count].copy_from_slice(&self.rx[self.cursor..self.cursor + count]);
        self.cursor += count;
        self.arm_receive()?;
        Ok(count)
    }
    pub fn writable(&self) -> bool {
        self.tx_token.is_none()
    }
    pub fn send(&mut self, data: &[u8]) -> Result<()> {
        assert!(self.writable() && data.len() <= PAGE && !data.is_empty());
        self.tx[..data.len()].copy_from_slice(data);
        self.tx_length = data.len();
        self.tx_token = Some(unsafe { self.txq.add(&[&self.tx[..self.tx_length]], &mut []) }?);
        if self.txq.should_notify() {
            self.transport.notify(1);
        }
        Ok(())
    }
    /// Reset acknowledgment precedes ANY queue/backing release.
    /// # Safety
    /// The callback must close/drain all outstanding HAL shares without copy-back.
    /// It runs after reset and queue release, while the submitted boxes remain live.
    pub unsafe fn shutdown(mut self, discard: impl FnOnce()) {
        self.transport.set_status(DeviceStatus::empty());
        let mut reset = false;
        for _ in 0..1_000_000 {
            if self.transport.get_status() == DeviceStatus::empty() {
                reset = true;
                break;
            }
            core::hint::spin_loop();
        }
        assert!(reset, "virtio reset timeout: retain DMA backing");
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        unsafe {
            ManuallyDrop::drop(&mut self.rxq);
            ManuallyDrop::drop(&mut self.txq);
            discard();
            ManuallyDrop::drop(&mut self.rx);
            ManuallyDrop::drop(&mut self.tx);
            ManuallyDrop::drop(&mut self.transport);
        }
    }
}
