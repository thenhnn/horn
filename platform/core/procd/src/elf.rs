//! Validate ELF before spending process resources. No relocations or dynamic linker.
use crate::Error;
use alloc::collections::BTreeMap;
use core::ops::Range;
use object::{Object, ObjectSection, ObjectSegment, SegmentFlags, read::elf::ProgramHeader};

pub(crate) const READ: u32 = 4;
pub(crate) const WRITE: u32 = 2;
pub(crate) const EXECUTE: u32 = 1;

pub struct Image<'a> {
    pub(crate) file: object::File<'a>,
    pub(crate) pages: BTreeMap<usize, u32>,
    pub entry: usize,
    pub ipc_address: usize,
}
impl<'a> Image<'a> {
    pub fn parse(bytes: &'a [u8], allowed: Range<usize>, max_pages: usize) -> Result<Self, Error> {
        let file = object::File::parse(bytes).map_err(|_| Error::InvalidElf)?;
        if file.format() != object::BinaryFormat::Elf
            || file.architecture() != object::Architecture::X86_64
            || !file.is_little_endian()
            || file.kind() != object::ObjectKind::Executable
        {
            return Err(Error::InvalidElf);
        }
        if let object::File::Elf64(elf) = &file {
            for header in elf.elf_program_headers() {
                let kind = header.p_type(elf.endian());
                if kind == object::elf::PT_INTERP
                    || kind == object::elf::PT_DYNAMIC
                    || (kind == object::elf::PT_GNU_STACK
                        && header.p_flags(elf.endian()).0 & EXECUTE != 0)
                {
                    return Err(Error::Denied);
                }
            }
        }
        let tag = file.section_by_name(".horn").ok_or(Error::InvalidAbi)?;
        if tag.data().map_err(|_| Error::InvalidAbi)? != b"HORN\x02\x00\x00\x00" {
            return Err(Error::InvalidAbi);
        }
        let mut pages = BTreeMap::new();
        let page = horn_vm::sel4::PAGE_SIZE;
        let entry = usize::try_from(file.entry()).map_err(|_| Error::InvalidElf)?;
        let mut entry_valid = false;
        for segment in file.segments() {
            let start = usize::try_from(segment.address()).map_err(|_| Error::InvalidElf)?;
            let size = usize::try_from(segment.size()).map_err(|_| Error::InvalidElf)?;
            if size == 0 {
                continue;
            }
            let end = start.checked_add(size).ok_or(Error::InvalidElf)?;
            let data = segment.data().map_err(|_| Error::InvalidElf)?;
            if start < allowed.start || end > allowed.end || data.len() > size {
                return Err(Error::InvalidElf);
            }
            let SegmentFlags::Elf { p_flags, .. } = segment.flags() else {
                return Err(Error::InvalidElf);
            };
            let flags = p_flags.0;
            if flags & READ == 0 || flags & !(READ | WRITE | EXECUTE) != 0 {
                return Err(Error::Denied);
            }
            if flags & EXECUTE != 0 && (start..end).contains(&entry) {
                entry_valid = true;
            }
            let end_page = end.checked_add(page - 1).ok_or(Error::InvalidElf)? & !(page - 1);
            for address in (start & !(page - 1)..end_page).step_by(page) {
                if !pages.contains_key(&address) && pages.len() >= max_pages {
                    return Err(Error::Resource(horn_vm::Error::QuotaExceeded));
                }
                let combined = *pages.entry(address).or_insert(0) | flags;
                if combined & (WRITE | EXECUTE) == WRITE | EXECUTE {
                    return Err(Error::Denied);
                }
                pages.insert(address, combined);
            }
        }
        if !entry_valid {
            return Err(Error::InvalidElf);
        }
        let ipc_address = pages
            .last_key_value()
            .ok_or(Error::InvalidElf)?
            .0
            .checked_add(page)
            .ok_or(Error::InvalidElf)?;
        if ipc_address
            .checked_add(page)
            .is_none_or(|end| end > allowed.end)
        {
            return Err(Error::InvalidElf);
        }
        Ok(Self {
            file,
            pages,
            entry,
            ipc_address,
        })
    }
}
