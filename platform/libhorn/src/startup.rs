//! ABI=2 startup record. All wire integers are little endian; offsets are bytes.
//! The loader retains immutable resident backing for the process lifetime.
use core::{mem, slice};

/// Little-endian bytes "HORNCTX2".
pub const HORN_STARTUP_MAGIC: u64 = 0x325854434e524f48;
pub const HORN_STARTUP_VERSION: u32 = 2;
pub const HORN_STARTUP_HEADER_SIZE: u32 = 96;
pub const HORN_CONTEXT_ENTRY_SIZE: u32 = 24;
pub const HORN_STARTUP_OK: i32 = 0;
pub const HORN_STARTUP_INVALID: i32 = -1;
pub const HORN_STARTUP_NOT_FOUND: i32 = -2;

/// Wire layout, not a requirement that the input address be Rust-aligned.
/// Ordinary applications do not construct selectors from this structure.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HornStartup {
    pub magic: u64,
    pub version: u32,
    pub header_size: u32,
    pub total_size: u64,
    pub entries_offset: u64,
    pub entry_count: u64,
    pub strings_offset: u64,
    pub strings_size: u64,
    pub ipc_buffer: u64,
    pub cspace_root: u64,
    /// Initial flat CNode radix, not a permanent limit on CSpace growth.
    pub cspace_bits: u32,
    pub reserved: u32,
    /// Empty initial slots, half-open. Receive/heap partitioning is runtime policy.
    pub free_start: u64,
    pub free_end: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HornContextEntry {
    /// Relative to the beginning of the strings region, not the whole record.
    pub name_offset: u64,
    pub name_size: u64,
    pub selector: u64,
}

const _: () = {
    assert!(mem::size_of::<HornStartup>() == HORN_STARTUP_HEADER_SIZE as usize);
    assert!(mem::size_of::<HornContextEntry>() == HORN_CONTEXT_ENTRY_SIZE as usize);
    assert!(mem::offset_of!(HornStartup, free_end) == 88);
    assert!(mem::offset_of!(HornStartup, cspace_bits) == 72);
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    UnsupportedVersion,
    BufferTooSmall,
}

#[derive(Clone, Copy, Debug)]
pub struct RuntimeConfig {
    pub ipc_buffer: u64,
    pub cspace_root: u64,
    pub cspace_bits: u32,
    pub free_start: u64,
    pub free_end: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct Entry<'a> {
    pub name: &'a str,
    pub selector: u64,
}

/// Validated, borrowed initial context. Does not own/delete kernel caps.
#[derive(Clone, Copy)]
pub struct Context<'a> {
    bytes: &'a [u8],
    header: HornStartup,
}

fn number(bytes: &[u8], offset: usize) -> Result<u64, Error> {
    let raw = bytes
        .get(offset..offset.checked_add(8).ok_or(Error::Invalid)?)
        .ok_or(Error::Invalid)?;
    Ok(u64::from_le_bytes(
        raw.try_into().map_err(|_| Error::Invalid)?,
    ))
}
fn small_number(bytes: &[u8], offset: usize) -> Result<u32, Error> {
    let raw = bytes
        .get(offset..offset.checked_add(4).ok_or(Error::Invalid)?)
        .ok_or(Error::Invalid)?;
    Ok(u32::from_le_bytes(
        raw.try_into().map_err(|_| Error::Invalid)?,
    ))
}
fn range(offset: u64, length: u64, limit: usize) -> Result<core::ops::Range<usize>, Error> {
    let start = usize::try_from(offset).map_err(|_| Error::Invalid)?;
    let length = usize::try_from(length).map_err(|_| Error::Invalid)?;
    let end = start.checked_add(length).ok_or(Error::Invalid)?;
    if end > limit {
        return Err(Error::Invalid);
    }
    Ok(start..end)
}
pub(crate) fn valid_name(name: &[u8]) -> bool {
    !name.is_empty() && name.iter().all(|b| (0x21..=0x7e).contains(b))
}
fn valid_runtime(config: RuntimeConfig) -> bool {
    config.ipc_buffer != 0
        && config.ipc_buffer & 7 == 0
        && config.cspace_bits > 0
        && config.cspace_bits < 64
        && config.cspace_root < config.free_start
        && config.free_start < config.free_end
        && config.free_end <= (1u64 << config.cspace_bits)
}
impl<'a> Context<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.len() < HORN_STARTUP_HEADER_SIZE as usize
            || number(bytes, 0)? != HORN_STARTUP_MAGIC
        {
            return Err(Error::Invalid);
        }
        if small_number(bytes, 8)? != HORN_STARTUP_VERSION {
            return Err(Error::UnsupportedVersion);
        }
        let header = HornStartup {
            magic: number(bytes, 0)?,
            version: small_number(bytes, 8)?,
            header_size: small_number(bytes, 12)?,
            total_size: number(bytes, 16)?,
            entries_offset: number(bytes, 24)?,
            entry_count: number(bytes, 32)?,
            strings_offset: number(bytes, 40)?,
            strings_size: number(bytes, 48)?,
            ipc_buffer: number(bytes, 56)?,
            cspace_root: number(bytes, 64)?,
            cspace_bits: small_number(bytes, 72)?,
            reserved: small_number(bytes, 76)?,
            free_start: number(bytes, 80)?,
            free_end: number(bytes, 88)?,
        };
        if header.header_size != HORN_STARTUP_HEADER_SIZE
            || header.total_size != bytes.len() as u64
            || header.reserved != 0
            || !valid_runtime(RuntimeConfig {
                ipc_buffer: header.ipc_buffer,
                cspace_root: header.cspace_root,
                cspace_bits: header.cspace_bits,
                free_start: header.free_start,
                free_end: header.free_end,
            })
        {
            return Err(Error::Invalid);
        }
        let entries = range(
            header.entries_offset,
            header
                .entry_count
                .checked_mul(u64::from(HORN_CONTEXT_ENTRY_SIZE))
                .ok_or(Error::Invalid)?,
            bytes.len(),
        )?;
        let strings = range(header.strings_offset, header.strings_size, bytes.len())?;
        if entries.start < HORN_STARTUP_HEADER_SIZE as usize
            || entries.start & 7 != 0
            || strings.start < entries.end
            || strings.end != bytes.len()
        {
            return Err(Error::Invalid);
        }
        let view = Self { bytes, header };
        let mut previous: Option<&[u8]> = None;
        for i in 0..view.len() {
            let (name, selector) = view.entry(i)?;
            if !valid_name(name)
                || previous.is_some_and(|p| p >= name)
                || selector == 0
                || selector >= header.free_start
            {
                return Err(Error::Invalid);
            }
            previous = Some(name);
        }
        Ok(view)
    }
    pub fn header(self) -> HornStartup {
        self.header
    }
    pub fn len(self) -> usize {
        self.header.entry_count as usize
    }
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
    fn entry(self, index: usize) -> Result<(&'a [u8], u64), Error> {
        if index >= self.len() {
            return Err(Error::Invalid);
        }
        let offset = self.header.entries_offset as usize + index * HORN_CONTEXT_ENTRY_SIZE as usize;
        let names = range(
            self.header.strings_offset,
            self.header.strings_size,
            self.bytes.len(),
        )?;
        let name = range(
            number(self.bytes, offset)?,
            number(self.bytes, offset + 8)?,
            names.len(),
        )?;
        Ok((
            &self.bytes[names.start + name.start..names.start + name.end],
            number(self.bytes, offset + 16)?,
        ))
    }
    /// Bootstrap selector only. Runtime must import it before exposing managed handles.
    pub fn lookup(self, name: &str) -> Option<u64> {
        self.lookup_bytes(name.as_bytes())
    }
    fn lookup_bytes(self, name: &[u8]) -> Option<u64> {
        let mut low = 0;
        let mut high = self.len();
        while low < high {
            let middle = low + (high - low) / 2;
            let (key, selector) = self.entry(middle).ok()?;
            match key.cmp(name) {
                core::cmp::Ordering::Less => low = middle + 1,
                core::cmp::Ordering::Greater => high = middle,
                core::cmp::Ordering::Equal => return Some(selector),
            }
        }
        None
    }
}

/// Encode into caller storage. Entries must already be sorted by ASCII name.
/// Aliases (different names for the same selector) are allowed; no cap is copied.
pub fn encode(
    config: RuntimeConfig,
    entries: &[Entry<'_>],
    out: &mut [u8],
) -> Result<usize, Error> {
    encode_iter(config, entries.iter().copied(), out)
}

pub fn encode_iter<'a>(
    config: RuntimeConfig,
    entries: impl Iterator<Item = Entry<'a>> + Clone,
    out: &mut [u8],
) -> Result<usize, Error> {
    if !valid_runtime(config) {
        return Err(Error::Invalid);
    }
    let count = entries.clone().count();
    let table_size = count
        .checked_mul(HORN_CONTEXT_ENTRY_SIZE as usize)
        .ok_or(Error::Invalid)?;
    let strings_offset = (HORN_STARTUP_HEADER_SIZE as usize)
        .checked_add(table_size)
        .ok_or(Error::Invalid)?;
    let mut size = strings_offset;
    let mut previous = None;
    for entry in entries.clone() {
        let name = entry.name.as_bytes();
        if !valid_name(name)
            || previous.is_some_and(|p: &[u8]| p >= name)
            || entry.selector == 0
            || entry.selector >= config.free_start
        {
            return Err(Error::Invalid);
        }
        previous = Some(name);
        size = size.checked_add(name.len()).ok_or(Error::Invalid)?;
    }
    if out.len() < size {
        return Err(Error::BufferTooSmall);
    }
    let out = &mut out[..size];
    out.fill(0);
    fn put(out: &mut [u8], offset: usize, value: u64) {
        out[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
    put(out, 0, HORN_STARTUP_MAGIC);
    out[8..12].copy_from_slice(&HORN_STARTUP_VERSION.to_le_bytes());
    out[12..16].copy_from_slice(&HORN_STARTUP_HEADER_SIZE.to_le_bytes());
    for (offset, value) in [
        (16, size as u64),
        (24, HORN_STARTUP_HEADER_SIZE as u64),
        (32, count as u64),
        (40, strings_offset as u64),
        (48, (size - strings_offset) as u64),
        (56, config.ipc_buffer),
        (64, config.cspace_root),
        (80, config.free_start),
        (88, config.free_end),
    ] {
        put(out, offset, value);
    }
    out[72..76].copy_from_slice(&config.cspace_bits.to_le_bytes());
    let mut cursor = strings_offset;
    for (i, entry) in entries.enumerate() {
        let offset = HORN_STARTUP_HEADER_SIZE as usize + i * HORN_CONTEXT_ENTRY_SIZE as usize;
        put(out, offset, (cursor - strings_offset) as u64);
        put(out, offset + 8, entry.name.len() as u64);
        put(out, offset + 16, entry.selector);
        out[cursor..cursor + entry.name.len()].copy_from_slice(entry.name.as_bytes());
        cursor += entry.name.len();
    }
    Ok(size)
}

/// Validate an ABI=2 record without allocations. Not a capability authenticity check.
/// # Safety
/// `bytes` must address `size` readable bytes, immutable for this call. Size must
/// not exceed isize::MAX. The function cannot validate arbitrary virtual addresses.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn horn_startup_validate(bytes: *const u8, size: usize) -> i32 {
    if bytes.is_null() || size > isize::MAX as usize {
        return HORN_STARTUP_INVALID;
    }
    match Context::parse(unsafe { slice::from_raw_parts(bytes, size) }) {
        Ok(_) => HORN_STARTUP_OK,
        Err(_) => HORN_STARTUP_INVALID,
    }
}

/// Low-level bootstrap lookup, not an owning public handle lookup.
/// # Safety
/// Record and name must be readable and immutable for the call; lengths must not
/// exceed isize::MAX. `selector` must point to writable u64 storage, aligned and
/// disjoint from both inputs. The output is untouched on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn horn_startup_lookup(
    bytes: *const u8,
    size: usize,
    name: *const u8,
    name_size: usize,
    selector: *mut u64,
) -> i32 {
    if bytes.is_null()
        || name.is_null()
        || selector.is_null()
        || size > isize::MAX as usize
        || name_size > isize::MAX as usize
    {
        return HORN_STARTUP_INVALID;
    }
    let Ok(context) = Context::parse(unsafe { slice::from_raw_parts(bytes, size) }) else {
        return HORN_STARTUP_INVALID;
    };
    let name = unsafe { slice::from_raw_parts(name, name_size) };
    if !valid_name(name) {
        return HORN_STARTUP_INVALID;
    }
    match context.lookup_bytes(name) {
        Some(value) => {
            unsafe {
                selector.write(value);
            }
            HORN_STARTUP_OK
        }
        None => HORN_STARTUP_NOT_FOUND,
    }
}

#[cfg(test)]
mod tests;
