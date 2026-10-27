use super::*;
use alloc::{collections::BTreeMap, vec, vec::Vec};
use core::cell::Cell;
const BASE: u64 = 0xfed9_0000;
struct Mock {
    registers: BTreeMap<u64, u64>,
    pages: Vec<[u64; 512]>,
    ticks: Cell<u64>,
    stall: bool,
    drain: Vec<u64>,
}
impl Mock {
    fn new() -> Self {
        Self {
            registers: [
                (BASE, 0x10),
                (
                    BASE + 8,
                    (3 << 54) | (0x22 << 24) | (47 << 16) | (1 << 10) | 1,
                ),
                (BASE + 16, 0xb),
            ]
            .into_iter()
            .collect(),
            pages: Vec::new(),
            ticks: Cell::new(0),
            stall: false,
            drain: Vec::new(),
        }
    }
    fn page(&self, physical: u64) -> usize {
        ((physical - 0x100000) / 4096) as usize
    }
    fn read(&self, physical: u64, word: usize) -> u64 {
        self.pages[self.page(physical)][word]
    }
    fn translate(&self, sid: u16, iova: u64, write: bool) -> Option<u64> {
        if self.registers.get(&(BASE + 0x1c)).copied().unwrap_or(0) & TE as u64 == 0 {
            return Some(iova);
        }
        let root = self.registers[&(BASE + 0x20)];
        let context = self.read(root, (sid >> 8) as usize * 2);
        if context & 1 == 0 {
            return None;
        }
        let low = self.read(context & ADDR, (sid & 255) as usize * 2);
        if low & 1 == 0 {
            return None;
        }
        let aw = self.read(context & ADDR, (sid & 255) as usize * 2 + 1) & 7;
        let mut page = low & ADDR;
        for level in (0..aw + 2).rev() {
            let pte = self.read(page, ((iova >> (12 + level * 9)) & 511) as usize);
            if pte & if write { 2 } else { 1 } == 0 {
                return None;
            }
            page = pte & ADDR;
        }
        Some(page | (iova & 4095))
    }
    fn deliver(&self, sid: u16, address: u64, data: u32) -> Option<u8> {
        if address & 0xfff0_001f != 0xfee0_0010 || data != 0 {
            return None;
        }
        let index = ((address >> 5) & 0x7fff) as usize;
        if index >= 256 {
            return None;
        }
        let table = self.registers[&(BASE + 0xb8)] & ADDR;
        let low = self.read(table, index * 2);
        let high = self.read(table, index * 2 + 1);
        if low & 1 == 0 || high >> 18 & 3 != 1 || high as u16 != sid {
            return None;
        }
        Some((low >> 16) as u8)
    }
}
unsafe impl Hardware for Mock {
    fn read32(&mut self, address: u64) -> Result<u32> {
        Ok(self.registers.get(&address).copied().unwrap_or(0) as u32)
    }
    fn read64(&mut self, address: u64) -> Result<u64> {
        Ok(self.registers.get(&address).copied().unwrap_or(0))
    }
    fn write32(&mut self, address: u64, value: u32) -> Result<()> {
        if address == BASE + 0x34 {
            let old = self.registers.get(&address).copied().unwrap_or(0);
            self.registers.insert(address, old & !(value as u64));
            return Ok(());
        }
        self.registers.insert(address, value as u64);
        if address == BASE + 0x18 && !self.stall {
            self.registers.insert(BASE + 0x1c, value as u64);
        }
        Ok(())
    }
    fn write64(&mut self, address: u64, value: u64) -> Result<()> {
        if address == BASE + 0x228 {
            let old = self.registers.get(&address).copied().unwrap_or(0);
            self.registers.insert(address, old & !value);
            return Ok(());
        }
        self.registers.insert(address, value);
        if address == BASE + 0x88 && !self.stall {
            if let Some(&queue) = self.registers.get(&(BASE + 0x90)) {
                let mut head = self.registers.get(&(BASE + 0x80)).copied().unwrap_or(0);
                while head != value {
                    let low = self.read(queue & ADDR, head as usize / 8);
                    let high = self.read(queue & ADDR, head as usize / 8 + 1);
                    match low & 15 {
                        1 | 4 => {}
                        2 => self.drain.push(low),
                        5 => {
                            let page = self.page(high & ADDR);
                            self.pages[page][0] = low >> 32;
                        }
                        _ => return Err(Error::Backend),
                    }
                    head = (head + 16) & 4095;
                }
                self.registers.insert(BASE + 0x80, value);
            }
        }
        Ok(())
    }
    fn allocate_page(&mut self) -> Result<Page> {
        let cookie = self.pages.len() as u32;
        self.pages.push([0; 512]);
        Ok(Page {
            cookie,
            physical: 0x100000 + cookie as u64 * 4096,
        })
    }
    fn read_page(&mut self, page: Page, word: usize) -> Result<u64> {
        Ok(self.pages[page.cookie as usize][word])
    }
    fn write_page(&mut self, page: Page, word: usize, value: u64) -> Result<()> {
        self.pages[page.cookie as usize][word] = value;
        Ok(())
    }
    fn now(&self) -> u64 {
        let n = self.ticks.get();
        self.ticks.set(n + 1);
        n
    }
}
fn start(mock: Mock) -> Result<Vtd<Mock>> {
    unsafe {
        Vtd::enable(
            mock,
            dmar::Dmar {
                registers: BASE,
                host_width: 48,
            },
            100,
        )
    }
}
#[test]
fn deny_by_default_map_permissions_and_acknowledged_drain() {
    let mut unit = start(Mock::new()).unwrap();
    assert_eq!(unit.io.translate(0x18, 0x10000, true), None);
    let first = unit.attach(0x18).unwrap();
    let second = unit.attach(0x20).unwrap();
    unit.map(first, 0x10000, &[0x900000, 0x901000], 1).unwrap();
    assert_eq!(unit.io.translate(0x18, 0x10037, false), Some(0x900037));
    assert_eq!(unit.io.translate(0x18, 0x10000, true), None);
    assert_eq!(unit.io.translate(0x20, 0x10000, false), None);
    assert_eq!(unit.io.translate(0x18, 0x12000, false), None);
    unit.map(second, 0x10000, &[0xa00000], 3).unwrap();
    assert_eq!(unit.io.translate(0x20, 0x10000, true), Some(0xa00000));
    let invalidations = unit.io.drain.len();
    unit.unmap(first, 0x10000, 2).unwrap();
    assert!(unit.io.drain.len() > invalidations);
    assert_eq!(unit.io.drain.last().unwrap() & 0xc0, 0xc0);
    assert_eq!(unit.io.translate(0x18, 0x10000, false), None);
    assert_eq!(unit.io.translate(0x20, 0x10000, true), Some(0xa00000));
    assert_eq!(
        unit.map(first, 0x10000, &[0xb00000], 3),
        Err(Error::Invalid)
    );
    unit.detach(first).unwrap();
    assert!(matches!(unit.attach(0x18), Err(Error::Invalid)));
    assert_eq!(unit.unmap(first, 0x10000, 1), Err(Error::Invalid));
}
#[test]
fn interrupt_routes_validate_requester_and_block_compatibility_format() {
    let mut unit = start(Mock::new()).unwrap();
    let domain = unit.attach(0x18).unwrap();
    let route = unit.interrupt(domain, 0x80, 0).unwrap();
    assert_eq!(
        unit.io.deliver(0x18, route.address(), route.data()),
        Some(0x80)
    );
    assert_eq!(unit.io.deliver(0x20, route.address(), route.data()), None);
    assert_eq!(unit.io.deliver(0x18, 0xfee0_0000, 0x80), None);
    assert_eq!(unit.io.registers[&(BASE + 0x1c)] & CFI as u64, 0);
    unit.remove_interrupt(domain, route).unwrap();
    assert_eq!(unit.io.deliver(0x18, route.address(), 0), None);
    let second = unit.interrupt(domain, 0x81, 0).unwrap();
    unit.detach(domain).unwrap();
    assert_eq!(unit.io.deliver(0x18, second.address(), 0), None);
}
#[test]
fn invalid_requests_do_not_poison_but_failed_invalidation_does() {
    let mut unit = start(Mock::new()).unwrap();
    let domain = unit.attach(0x18).unwrap();
    assert_eq!(
        unit.map(domain, 0x10001, &[0x900000], 3),
        Err(Error::Invalid)
    );
    unit.map(domain, 0x11000, &[0x900000], 3).unwrap();
    assert_eq!(
        unit.map(domain, 0x10000, &[0xa00000, 0xa01000], 3),
        Err(Error::Invalid)
    );
    assert_eq!(unit.io.translate(0x18, 0x10000, false), None);
    assert!(!unit.poisoned());
    unit.io.stall = true;
    assert_eq!(unit.unmap(domain, 0x11000, 1), Err(Error::Timeout));
    assert!(unit.poisoned());
    assert_eq!(
        unit.map(domain, 0x12000, &[0xa00000], 3),
        Err(Error::Poisoned)
    );
}
#[test]
fn unsupported_or_preowned_units_never_enable_translation() {
    for (reg, value) in [
        (BASE + 16, 3),
        (BASE + 16, 0xa),
        (BASE + 8, 0x700),
        (BASE + 0x1c, TE as u64),
    ] {
        let mut mock = Mock::new();
        mock.registers.insert(reg, value);
        assert!(matches!(start(mock), Err(Error::Unsupported)));
    }
    let mut mock = Mock::new();
    mock.stall = true;
    assert!(matches!(start(mock), Err(Error::Timeout)));
}
#[test]
fn queue_wrap_and_domain_id_capability_bounds() {
    let mut unit = start(Mock::new()).unwrap();
    let domain = unit.attach(0x18).unwrap();
    for n in 0..180 {
        let address = 0x10000 + n * 4096;
        unit.map(domain, address, &[0x900000], 3).unwrap();
        unit.unmap(domain, address, 1).unwrap();
    }
    // CAP.ND=1 supports 64 domains, ID0 remains reserved.
    for sid in 0..62 {
        unit.attach(sid + 0x20).unwrap();
    }
    assert!(matches!(unit.attach(0x10), Err(Error::Exhausted)));
}
#[test]
fn device_faults_are_reported_without_poisoning_the_unit() {
    let mut unit = start(Mock::new()).unwrap();
    unit.io.registers.insert(BASE + 0x34, 3);
    unit.io.registers.insert(BASE + 0x220, 0xdead_b023);
    unit.io
        .registers
        .insert(BASE + 0x228, (1 << 63) | (1 << 62) | (5 << 32) | 0x18);
    assert_eq!(
        unit.poll_fault().unwrap(),
        FaultReport {
            overflow: true,
            fault: Some(Fault {
                source: 0x18,
                address: 0xdead_b000,
                reason: 5,
                read: true
            })
        }
    );
    assert_eq!(unit.poll_fault().unwrap().fault, None);
    assert!(!unit.poisoned());
    unit.io.registers.insert(BASE + 0x34, 1 << 4);
    assert_eq!(unit.poll_fault(), Err(Error::Backend));
    assert!(unit.poisoned());
}
fn dmar(entries: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0; 48];
    bytes[..4].copy_from_slice(b"DMAR");
    bytes[36] = 47;
    bytes[37] = 1;
    bytes.extend_from_slice(entries);
    let length = bytes.len() as u32;
    bytes[4..8].copy_from_slice(&length.to_le_bytes());
    bytes[9] = 0u8.wrapping_sub(bytes.iter().fold(0u8, |s, b| s.wrapping_add(*b)));
    bytes
}
#[test]
fn dmar_is_bounded_and_reserved_memory_is_not_silently_ignored() {
    let mut drhd = vec![0, 0, 16, 0, 1, 0, 0, 0];
    drhd.extend_from_slice(&BASE.to_le_bytes());
    assert_eq!(dmar::Dmar::parse(&dmar(&drhd)).unwrap().registers, BASE);
    let mut invalid = dmar(&drhd);
    invalid[20] ^= 1;
    assert!(dmar::Dmar::parse(&invalid).is_err());
    for length in 0..16 {
        let mut short = drhd.clone();
        short[2] = length;
        assert!(dmar::Dmar::parse(&dmar(&short)).is_err());
    }
    let mut multiple = drhd.clone();
    multiple.extend_from_slice(&drhd);
    assert!(matches!(
        dmar::Dmar::parse(&dmar(&multiple)),
        Err(crate::protocol::DeviceError::Unsupported)
    ));
    let mut reserved = drhd.clone();
    reserved.extend_from_slice(&[1, 0, 4, 0]);
    assert!(matches!(
        dmar::Dmar::parse(&dmar(&reserved)),
        Err(crate::protocol::DeviceError::Unsupported)
    ));
}
