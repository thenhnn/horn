// Copyright 2024, Colias Group, LLC
// SPDX-License-Identifier: BSD-2-Clause
//
// Translation-table setup adapted from rust-sel4's spawn-task example.
//! Resident mappings. All temporary writable aliases disappear before execution.
use crate::{
    Error,
    elf::{EXECUTE, Image, WRITE},
};
use alloc::collections::{BTreeMap, BTreeSet};
use core::ops::Range;
use horn_vm::sel4::{KernelAllocator, PAGE_SIZE};
use object::{Object, ObjectSegment};

/// Trusted boot setup, not a client-selectable mapping operation.
pub(crate) fn prepare_window(
    allocator: &mut KernelAllocator,
    vspace: sel4::cap::VSpace,
    window: Range<usize>,
) -> Result<(), Error> {
    for level in 1..sel4::vspace_levels::NUM_LEVELS {
        let span = 1usize << sel4::vspace_levels::span_bits(level);
        for address in (window.start & !(span - 1)..window.end).step_by(span) {
            let ty = sel4::TranslationTableObjectType::from_level(level).ok_or(Error::Denied)?;
            let cap = allocator
                .allocate(ty.blueprint())?
                .cast::<sel4::cap_type::UnspecifiedIntermediateTranslationTable>();
            match cap.generic_intermediate_translation_table_map(
                ty,
                vspace,
                address,
                sel4::VmAttributes::DEFAULT,
            ) {
                Ok(()) => (),
                Err(sel4::Error::DeleteFirst) => allocator.release_slot(cap.bits() as usize)?,
                Err(e) => {
                    allocator.release_slot(cap.bits() as usize)?;
                    return Err(e.into());
                }
            }
        }
    }
    Ok(())
}

pub(crate) struct Loaded {
    pub vspace: sel4::cap::VSpace,
    pub ipc: Option<sel4::cap::Granule>,
}

pub(crate) fn empty(
    allocator: &mut KernelAllocator,
    asid_pool: sel4::cap::AsidPool,
    window: Range<usize>,
) -> Result<Loaded, Error> {
    let vspace = allocator.allocate_fixed_sized::<sel4::cap_type::VSpace>()?;
    asid_pool.asid_pool_assign(vspace)?;
    prepare_window(allocator, vspace, window)?;
    Ok(Loaded { vspace, ipc: None })
}

pub(crate) fn load(
    allocator: &mut KernelAllocator,
    image: &Image<'_>,
    caller_vspace: sel4::cap::VSpace,
    scratch: usize,
    asid_pool: sel4::cap::AsidPool,
    vm_window: Range<usize>,
) -> Result<Loaded, Error> {
    let vspace = allocator.allocate_fixed_sized::<sel4::cap_type::VSpace>()?;
    asid_pool.asid_pool_assign(vspace)?;
    let mut table_addresses = BTreeSet::new();
    for level in 1..sel4::vspace_levels::NUM_LEVELS {
        let span = 1usize << sel4::vspace_levels::span_bits(level);
        let addresses = image
            .pages
            .keys()
            .copied()
            .chain(core::iter::once(image.ipc_address))
            .chain((vm_window.start..vm_window.end).step_by(PAGE_SIZE));
        for address in addresses {
            let address = address & !(span - 1);
            if !table_addresses.insert((level, address)) {
                continue;
            }
            let ty = sel4::TranslationTableObjectType::from_level(level).ok_or(Error::Denied)?;
            allocator
                .allocate(ty.blueprint())?
                .cast::<sel4::cap_type::UnspecifiedIntermediateTranslationTable>()
                .generic_intermediate_translation_table_map(
                    ty,
                    vspace,
                    address,
                    sel4::VmAttributes::DEFAULT,
                )?;
        }
    }
    let mut pages = BTreeMap::new();
    for &address in image.pages.keys() {
        pages.insert(
            address,
            allocator.allocate_fixed_sized::<sel4::cap_type::Granule>()?,
        );
    }
    // Retyping zeroes all pages, including BSS. Copy every segment before mapping RX.
    for segment in image.file.segments() {
        let mut address = segment.address() as usize;
        let mut data = segment.data().map_err(|_| Error::InvalidElf)?;
        while !data.is_empty() {
            let page = address & !(PAGE_SIZE - 1);
            let offset = address - page;
            let count = data.len().min(PAGE_SIZE - offset);
            let cap = pages.get(&page).ok_or(Error::InvalidElf)?;
            cap.frame_map(
                caller_vspace,
                scratch,
                sel4::CapRights::read_write(),
                sel4::VmAttributes::EXECUTE_NEVER,
            )?;
            unsafe {
                core::ptr::copy_nonoverlapping(data.as_ptr(), (scratch + offset) as *mut u8, count);
            }
            cap.frame_unmap()?;
            address += count;
            data = &data[count..];
        }
    }
    for (&address, &flags) in &image.pages {
        pages[&address].frame_map(
            vspace,
            address,
            sel4::CapRights::new(false, false, true, flags & WRITE != 0),
            if flags & EXECUTE != 0 {
                sel4::VmAttributes::DEFAULT
            } else {
                sel4::VmAttributes::EXECUTE_NEVER
            },
        )?;
    }
    let ipc = allocator.allocate_fixed_sized::<sel4::cap_type::Granule>()?;
    ipc.frame_map(
        vspace,
        image.ipc_address,
        sel4::CapRights::read_write(),
        sel4::VmAttributes::EXECUTE_NEVER,
    )?;
    Ok(Loaded {
        vspace,
        ipc: Some(ipc),
    })
}
