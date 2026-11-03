//! Bootstrap client mechanics shared by service owners; no service policy.
extern crate alloc;
use crate::protocol::*;
use horn_ipc::*;
pub type Io = Sel4Transport<Sel4CapSpace>;
#[derive(Clone, Copy)]
pub struct Bootstrap {
    pub cnode: u32,
    pub bits: u8,
    pub vm: u32,
    pub space: u32,
}
impl Bootstrap {
    /// Seal and install the named startup descriptor after all initial grants.
    pub fn context(
        self,
        io: &mut Io,
        process: Cap<ProcessObjectTag>,
        entries: &mut [libhorn::startup::Entry<'_>],
    ) {
        entries.sort_unstable_by_key(|e| e.name);
        let size = 8 + entries.iter().map(|e| 16 + e.name.len()).sum::<usize>();
        let mut bytes = alloc::vec![0; size];
        libhorn::bindings::encode(entries, &mut bytes).unwrap();
        let descriptor = self.upload(io, &bytes);
        ProcessControlReceiver::from_cap(Cap::from_ref(process.as_ref()))
            .startup_context(&mut *io, &descriptor, size as u64)
            .unwrap();
        close_image(io, descriptor);
    }
    pub fn cap<T: CapType>(self, slot: u32) -> Cap<T> {
        Cap::new(
            self.cnode as _,
            slot,
            sel4::WORD_SIZE as _,
            0,
            CapMode::COPY,
            Rights::ALL,
        )
    }
    pub fn install<T: CapType>(
        self,
        io: &mut Io,
        process: Cap<ProcessObjectTag>,
        target: u32,
        source: Cap<T>,
        badge: Option<u64>,
        rights: Rights,
    ) {
        ProcessControlReceiver::from_cap(Cap::from_ref(process.as_ref()))
            .install(
                io,
                &self.cap(self.cnode),
                self.bits as _,
                source.as_ref().slot,
                target,
                badge,
                rights.0 as _,
            )
            .unwrap();
    }
    pub fn upload(self, io: &mut Io, bytes: &[u8]) -> Cap<VmMemoryObjectTag> {
        assert!(!bytes.is_empty());
        let length = bytes.len().next_multiple_of(4096);
        let memory = VmFactoryReceiver::from_cap(self.cap(self.vm))
            .allocate(&mut *io, length as _, VmBacking::Resident, VmProtection(3))
            .unwrap();
        let region = VmAddressSpaceReceiver::from_cap(self.cap(self.space))
            .reserve(
                &mut *io,
                VmRegionRequest {
                    address: 0,
                    length: length as _,
                    alignment: 4096,
                    placement: VmPlacement::Anywhere,
                    ceiling: VmProtection(3),
                },
            )
            .unwrap();
        let address = VmRegionReceiver::from_cap(Cap::from_ref(region.as_ref()))
            .info(&mut *io)
            .unwrap()
            .address;
        let mut updates = List::default();
        updates.len = 2;
        updates.data[0] = VmUpdate::Prepare(VmRange {
            offset: 0,
            length: length as _,
        });
        updates.data[1] = VmUpdate::Map(VmMap {
            offset: 0,
            length: length as _,
            memory_offset: 0,
            protection: VmProtection(3),
        });
        VmRegionReceiver::from_cap(Cap::from_ref(region.as_ref()))
            .apply(
                &mut *io,
                Some(Cap::from_ref(memory.as_ref())),
                Parcel::inline(updates),
            )
            .unwrap();
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), address as *mut u8, bytes.len());
        }
        VmRegionReceiver::from_cap(Cap::from_ref(region.as_ref()))
            .destroy(&mut *io)
            .unwrap();
        io.space.delete(region.as_ref()).unwrap();
        VmMemoryReceiver::from_cap(Cap::from_ref(memory.as_ref()))
            .add_seals(&mut *io, VmSeals::WRITE)
            .unwrap();
        memory
    }
}
pub fn close_image(io: &mut Io, memory: Cap<VmMemoryObjectTag>) {
    VmMemoryReceiver::from_cap(Cap::from_ref(memory.as_ref()))
        .close(&mut *io)
        .unwrap();
    io.space.delete(memory.as_ref()).unwrap();
}
pub fn mint<T: CapType>(
    io: &mut Io,
    root: sel4::cap::CNode,
    source: Cap<T>,
    badge: u64,
    rights: Rights,
) -> Cap<T> {
    let slot = io.space.reserve_slot().unwrap();
    slot.mint(
        &root.absolute_cptr(sel4::CPtr::from_bits(source.as_ref().slot as _)),
        sel4::CapRights::new(
            rights.0 & 8 != 0,
            rights.0 & 4 != 0,
            rights.0 & 1 != 0,
            rights.0 & 2 != 0,
        ),
        badge as _,
    )
    .unwrap();
    Cap::from_ref(
        io.space
            .register(sel4::cap::Unspecified::from_bits(slot.path().bits()))
            .unwrap(),
    )
}
pub fn copy<T: CapType>(io: &mut Io, root: sel4::cap::CNode, source: Cap<T>) -> Cap<T> {
    let slot = io.space.reserve_slot().unwrap();
    slot.copy(
        &root.absolute_cptr(sel4::CPtr::from_bits(source.as_ref().slot as _)),
        sel4::CapRights::all(),
    )
    .unwrap();
    Cap::from_ref(
        io.space
            .register(sel4::cap::Unspecified::from_bits(slot.path().bits()))
            .unwrap(),
    )
}
