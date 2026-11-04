//! Explicit clock-page mapping with RPC fallback; no transport is retained.
use crate::Context;
use horn_clock::protocol::{ClockError, ClockHostTag, VmMemoryClient as _, VmMemoryObjectTag};
use horn_clock::{ClockInterfaceClient, DefaultTimeManager, DefaultTimePage, TimeManager};
use horn_ipc::*;
use horn_vm::protocol as v;
use v::{VmAddressSpaceClient as _, VmRegionClient as _};
type Io = Sel4Transport<Sel4CapSpace>;
pub struct Clock {
    page: Option<*const DefaultTimePage>,
    clock: Cap<ClockHostTag>,
    memory: Option<OwnedCap<VmMemoryObjectTag>>,
    region: Option<OwnedCap<v::VmRegionObjectTag>>,
}
impl Clock {
    /// Fall back to RPC if page acquisition, validation or mapping fails.
    pub fn new(ctx: &Context, io: &mut Io, clock: Cap<ClockHostTag>) -> Self {
        if let Ok(memory) = clock.shared_page(&mut *io) {
            if let Some((page, region)) = Self::map(io, ctx, &memory) {
                crate::logging::debug!("shared clock page mapped (read-only)");
                return Self {
                    page: Some(page),
                    clock,
                    memory: Some(memory),
                    region: Some(region),
                };
            }
            // Shared handle has no CLOSE right: discard only this local copy.
            drop(memory);
        }
        crate::logging::warn!("shared clock page unavailable; using RPC");
        Self {
            page: None,
            clock,
            memory: None,
            region: None,
        }
    }
    fn map(
        io: &mut Io,
        ctx: &Context,
        memory: &OwnedCap<VmMemoryObjectTag>,
    ) -> Option<(*const DefaultTimePage, OwnedCap<v::VmRegionObjectTag>)> {
        let info = memory.info(&mut *io).ok()?;
        if info.size != 4096
            || info.resident_size != 4096
            || info.ceiling != horn_clock::protocol::VmProtection::READ
            || memory.rights(&mut *io).ok()? != horn_clock::protocol::VmMemoryRights::READ
        {
            return None;
        }
        let space = ctx
            .object::<v::VmAddressSpaceObjectTag>("horn.address-space")
            .ok()?;
        let region = space
            .reserve(
                &mut *io,
                v::VmRegionRequest {
                    address: 0,
                    length: 4096,
                    alignment: 4096,
                    placement: v::VmPlacement::Anywhere,
                    ceiling: v::VmProtection::READ,
                },
            )
            .ok()?;
        let mapped = (|| {
            let address = region.info(&mut *io).ok()?.address;
            let mut updates = List::default();
            updates.len = 2;
            updates.data[0] = v::VmUpdate::Prepare(v::VmRange {
                offset: 0,
                length: 4096,
            });
            updates.data[1] = v::VmUpdate::Map(v::VmMap {
                offset: 0,
                length: 4096,
                memory_offset: 0,
                protection: v::VmProtection::READ,
            });
            region
                .apply(
                    &mut *io,
                    Some(Cap::from_ref(memory.cap_ref())),
                    Parcel::inline(updates),
                )
                .ok()?;
            let page = address as *const DefaultTimePage;
            // Wire bytes, not a Rust bool/record layout. Timerd never updates
            // this ABI while serving clients; no sequence counter is required.
            if !unsafe { &*page }.is_valid() {
                return None;
            }
            Some(page)
        })();
        match mapped {
            Some(page) => Some((page, region)),
            None => {
                region.destroy(&mut *io).expect("clock region rollback");
                None
            }
        }
    }
    pub fn capability(&self) -> Cap<ClockHostTag> {
        self.clock
    }
    pub fn is_mapped(&self) -> bool {
        self.page.is_some()
    }
    /// Remove only this process's mapping. Retain resources on failure for retry.
    /// After successful unmap, now() continues through RPC.
    pub fn unmap(&mut self, io: &mut Io) -> Result<(), CallError<v::VmError>> {
        if let Some(region) = &self.region {
            region.destroy(&mut *io)?;
        }
        self.page = None;
        self.region = None;
        self.memory = None;
        Ok(())
    }
    pub fn now(&self, io: &mut Io) -> Result<u64, CallError<ClockError>> {
        if let Some(value) = self
            .page
            .and_then(|page| DefaultTimeManager::now(unsafe { &*page }))
        {
            return Ok(value);
        }
        self.clock.now(io)
    }
}
