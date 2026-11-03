//! ABI2 context provisioning. No caller-selected runtime addresses or mappings.
use crate::{
    Error, Manager, ProcessId, State,
    protocol::*,
    service::{error, vm_error},
};
use alloc::vec::Vec;
use horn_ipc::CapRef;
use horn_vm::{protocol::VmError, sel4::PAGE_SIZE};
use libhorn::{
    bindings::Bindings,
    startup::{self, RuntimeConfig},
};

impl Manager {
    /// Provision ABI2 from trusted native-loader bytes. The native caller owns
    /// installation authority; runtime addresses and CSpace bounds are still
    /// derived here, never accepted from this descriptor.
    pub fn startup_context_bytes(&mut self, id: ProcessId, bytes: &[u8]) -> Result<(), Error> {
        let p = self.process(id)?;
        if p.state != State::Created || p.startup_attempted {
            return Err(Error::Denied);
        }
        if bytes.is_empty() || bytes.len() > p.allocator.account().usage().limit {
            return Err(Error::Denied);
        }
        self.install_context(id, bytes)
    }

    /// Native counterpart of ProcessControl.install. Installation freezes once
    /// startup allocation begins, including after a partial failed allocation.
    pub fn install_initial_capability(
        &mut self,
        id: ProcessId,
        slot: u32,
        source: sel4::AbsoluteCPtr,
        rights: sel4::CapRights,
    ) -> Result<(), Error> {
        let p = self.process(id)?;
        if p.state != State::Created
            || p.startup_attempted
            || slot as u64 >= (1u64 << p.cnode_bits) / 2 - 3
        {
            return Err(Error::Denied);
        }
        let bits = p.cnode_bits;
        self.cnode(id)?
            .absolute_cptr_from_bits_with_depth(slot as _, bits)
            .copy(&source, rights)?;
        let p = self.process_mut(id)?;
        p.bootstrap_end = p.bootstrap_end.max(slot + 1);
        Ok(())
    }

    pub(crate) fn provision_context(
        &mut self,
        id: ProcessId,
        descriptor: CapRef,
        length: u64,
    ) -> Result<(), ProcessError> {
        let p = self.process(id).map_err(error)?;
        if p.state != State::Created || p.startup_attempted {
            return Err(ProcessError::State);
        }
        let length = usize::try_from(length).map_err(|_| ProcessError::Invalid)?;
        if length == 0 || length > p.allocator.account().usage().limit {
            return Err(ProcessError::Invalid);
        }
        let window = self
            .launch_window
            .clone()
            .ok_or(ProcessError::Quarantined)?;
        let source = self
            .processes
            .iter()
            .find(|p| p.vm.as_ref().is_some_and(|vm| vm.owns_memory(descriptor)))
            .map(|p| p.id)
            .ok_or(ProcessError::Denied)?;
        let mut vm = self.process_mut(source).map_err(error)?.vm.take().unwrap();
        let mut result = Err(Error::Denied);
        let read = unsafe {
            vm.with_sealed_bytes(descriptor, self.caller_vspace, window, |bytes| {
                result = bytes
                    .get(..length)
                    .ok_or(Error::Denied)
                    .and_then(|bytes| self.install_context(id, bytes));
            })
        };
        self.process_mut(source).map_err(error)?.vm = Some(vm);
        if let Err(e) = read {
            self.process_mut(id).map_err(error)?.startup = None;
            if e == VmError::Quarantined {
                self.launch_window = None;
                self.launch_poisoned = true;
            }
            return Err(vm_error(e));
        }
        result.map_err(error)
    }

    fn install_context(&mut self, id: ProcessId, bytes: &[u8]) -> Result<(), Error> {
        if self.scratch_poisoned {
            return Err(Error::Cleanup);
        }
        let bindings = Bindings::parse(bytes).map_err(|_| Error::Denied)?;
        let p = self.process(id)?;
        let config = RuntimeConfig {
            ipc_buffer: p.ipc_address as u64,
            cspace_root: PROCESS_CAPS_SELF_SLOT as u64,
            cspace_bits: p.cnode_bits as u32,
            free_start: p.bootstrap_end as u64,
            free_end: 1u64 << p.cnode_bits,
        };
        if config.free_end < 8 || config.free_start > config.free_end / 2 - 3 {
            return Err(Error::Denied);
        }
        let mut size = startup::HORN_STARTUP_HEADER_SIZE as usize;
        for entry in bindings.iter() {
            if entry.selector >= config.free_start {
                return Err(Error::Denied);
            }
            size = size
                .checked_add(startup::HORN_CONTEXT_ENTRY_SIZE as usize)
                .and_then(|s| s.checked_add(entry.name.len()))
                .ok_or(Error::Denied)?;
        }
        let address = p.resident_end;
        let rounded = size.checked_add(PAGE_SIZE - 1).ok_or(Error::Denied)? & !(PAGE_SIZE - 1);
        let end = address.checked_add(rounded).ok_or(Error::Denied)?;
        if end > p.resident_limit || rounded > p.allocator.account().usage().limit {
            return Err(Error::Denied);
        }
        let vspace = p.loaded.vspace;
        let mut allocator = p.allocator.clone();
        let mut record = Vec::new();
        record
            .try_reserve_exact(size)
            .map_err(|_| Error::Resource(horn_vm::Error::MetadataExhausted))?;
        record.resize(size, 0);
        startup::encode_iter(config, bindings.iter(), &mut record).map_err(|_| Error::Denied)?;
        // After this point failures retain accounted backing until process teardown;
        // no retry can consume another batch of frames in this process.
        self.process_mut(id)?.startup_attempted = true;
        crate::loader::prepare_window(&mut allocator, vspace, address..end)?;
        for (i, data) in record.chunks(PAGE_SIZE).enumerate() {
            let frame = allocator.allocate_fixed_sized::<sel4::cap_type::Granule>()?;
            if frame
                .frame_map(
                    self.caller_vspace,
                    self.scratch,
                    sel4::CapRights::read_write(),
                    sel4::VmAttributes::EXECUTE_NEVER,
                )
                .is_err()
            {
                self.scratch_poisoned = true;
                return Err(Error::Cleanup);
            }
            unsafe {
                core::ptr::copy_nonoverlapping(data.as_ptr(), self.scratch as *mut u8, data.len());
            }
            if frame.frame_unmap().is_err() {
                self.scratch_poisoned = true;
                return Err(Error::Cleanup);
            }
            frame.frame_map(
                vspace,
                address + i * PAGE_SIZE,
                sel4::CapRights::new(false, false, true, false),
                sel4::VmAttributes::EXECUTE_NEVER,
            )?;
        }
        let p = self.process_mut(id)?;
        p.resident_end = end;
        p.startup = Some((address, size));
        Ok(())
    }
}
