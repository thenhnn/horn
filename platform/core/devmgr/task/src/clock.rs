//! One-shot HPET backend. Only timerd receives this authority; no periodic tick.
use crate::native::{Io, Mapping};
use acpi::{
    AcpiHandler, AcpiTable, AcpiTables,
    sdt::{SdtHeader, Signature},
};
use horn_devmgr::protocol::*;
use horn_ipc::*;
#[repr(C, packed)]
struct HpetTable {
    header: SdtHeader,
}
unsafe impl AcpiTable for HpetTable {
    const SIGNATURE: Signature = Signature::HPET;
    fn header(&self) -> &SdtHeader {
        &self.header
    }
}
pub fn discover<H: AcpiHandler>(tables: &AcpiTables<H>) -> u64 {
    let table = tables.find_table::<HpetTable>().expect("ACPI HPET");
    let bytes = unsafe {
        core::slice::from_raw_parts(
            table.virtual_start().as_ptr().cast::<u8>(),
            table.region_length(),
        )
    };
    assert!(
        bytes.len() >= 56 && bytes[40] == 0 && bytes[42] == 0 && bytes[52] == 0,
        "HPET zero, memory mapped"
    );
    let base = u64::from_le_bytes(bytes[44..52].try_into().unwrap());
    assert!(base != 0 && base & 4095 == 0);
    base
}
pub struct Clock {
    map: Mapping,
    pub irq: u64,
    period_fs: u64,
    signal: Option<Cap<Notification>>,
    deadline: Option<u64>,
}
impl Clock {
    pub fn new(map: Mapping, irq: u64, address: u64, data: u32) -> Self {
        let mut this = Self {
            map,
            irq,
            period_fs: 0,
            signal: None,
            deadline: None,
        };
        let cap = this.read(0);
        this.period_fs = cap >> 32;
        assert!(
            cap & (1 << 13) != 0 && this.period_fs > 0 && this.period_fs <= 100_000_000,
            "64-bit HPET counter"
        );
        assert!(
            this.read(0x10) & 1 == 0 && this.read(0x100) & 4 == 0,
            "HPET not preowned"
        );
        assert!(
            this.read(0x100) & (1 << 15) != 0,
            "HPET FSB delivery; enable hpet.msi"
        );
        assert!(address <= u32::MAX as u64);
        this.write(0x100, 1 << 14);
        this.write(0x110, (address << 32) | data as u64);
        this.write(0xf0, 0);
        this.write(0x10, 1);
        horn_runtime::logging::debug!("hardware clock initialized");
        this
    }
    fn read(&self, offset: usize) -> u64 {
        unsafe { ((self.map.address + offset) as *const u64).read_volatile() }
    }
    fn write(&mut self, offset: usize, value: u64) {
        unsafe { ((self.map.address + offset) as *mut u64).write_volatile(value) }
    }
    pub fn now(&self) -> u64 {
        ((self.read(0xf0) as u128 * self.period_fs as u128) / 1_000_000_000) as u64
    }
    pub fn cancel(&mut self, io: &mut Io) {
        self.write(0x100, 1 << 14);
        self.deadline = None;
        if let Some(note) = self.signal.take() {
            io.space().delete(note.as_ref()).unwrap();
        }
    }
    pub fn arm(
        &mut self,
        io: &mut Io,
        deadline: u64,
        signal: Cap<Notification>,
    ) -> Result<(), ClockError> {
        if signal.as_ref().unwrapped_badge().is_some() {
            return Err(ClockError::Denied);
        }
        let ticks = (deadline as u128 * 1_000_000_000).div_ceil(self.period_fs as u128);
        if ticks > u64::MAX as u128 {
            return Err(ClockError::Invalid);
        }
        let root = io.transport.space.root;
        let note = horn_supervisor::client::copy(&mut io.transport, root, signal);
        self.cancel(io);
        self.signal = Some(note);
        self.deadline = Some(deadline);
        self.write(0x108, ticks as u64);
        self.write(0x100, (1 << 14) | 4);
        // A deadline may have passed between programming the comparator and enable.
        self.poll(io);
        Ok(())
    }
    pub fn poll(&mut self, io: &mut Io) {
        if self.deadline.is_some_and(|deadline| deadline <= self.now()) {
            self.write(0x100, 1 << 14);
            self.deadline = None;
            if let Some(note) = self.signal.take() {
                io.space()
                    .cap::<sel4::cap_type::Notification>(note.as_ref())
                    .unwrap()
                    .signal();
                io.space().delete(note.as_ref()).unwrap();
            }
        }
    }
}
