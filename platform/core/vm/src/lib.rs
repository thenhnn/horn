//! Resident backing, independent of address-space placement and RPC.
#![no_std]

extern crate alloc;

use alloc::vec::Vec;

pub mod account;

#[cfg(feature = "sel4")]
pub mod sel4;

#[cfg(feature = "rpc")]
pub mod service;

#[cfg(feature = "rpc")]
mod seals;

#[cfg(feature = "protocol")]
#[allow(dead_code, unused_imports, non_camel_case_types)]
pub mod protocol {
    include!(concat!(env!("OUT_DIR"), "/protocol.rs"));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidSize,
    QuotaExceeded,
    Exhausted,
    MetadataExhausted,
    Backend,
}

/// Failed release must quarantine backing rather than reuse it.
pub trait Frames {
    type Frame;
    fn page_size(&self) -> usize;
    fn allocate_zeroed(&mut self) -> Result<Self::Frame, Error>;
    fn release(&mut self, frame: Self::Frame) -> Result<(), Error>;
}

/// Resident backing. Must be explicitly released; drop leaks rather than reuses it.
#[must_use = "backing must be explicitly released through its provider"]
pub struct MemoryObject<F> {
    size: u64,
    resident_size: u64,
    frames: Vec<F>,
}

impl<F> MemoryObject<F> {
    pub fn allocate<B: Frames<Frame = F>>(size: u64, provider: &mut B) -> Result<Self, Error> {
        let page = provider.page_size() as u64;
        if size == 0 || !page.is_power_of_two() {
            return Err(Error::InvalidSize);
        }
        let resident_size = size.checked_add(page - 1).ok_or(Error::InvalidSize)? & !(page - 1);
        let count = usize::try_from(resident_size / page).map_err(|_| Error::InvalidSize)?;
        let mut frames = Vec::new();
        frames
            .try_reserve_exact(count)
            .map_err(|_| Error::MetadataExhausted)?;
        for _ in 0..count {
            match provider.allocate_zeroed() {
                Ok(frame) => frames.push(frame),
                Err(error) => {
                    let mut cleanup_failed = false;
                    for frame in frames {
                        cleanup_failed |= provider.release(frame).is_err();
                    }
                    return Err(if cleanup_failed {
                        Error::Backend
                    } else {
                        error
                    });
                }
            }
        }
        Ok(Self {
            size,
            resident_size,
            frames,
        })
    }

    pub fn size(&self) -> u64 {
        self.size
    }
    pub fn resident_size(&self) -> u64 {
        self.resident_size
    }
    pub fn page_count(&self) -> usize {
        self.frames.len()
    }
    /// Trusted provider tokens; these do not expose raw frame capabilities.
    pub fn frames(&self) -> &[F] {
        &self.frames
    }

    pub fn release<B: Frames<Frame = F>>(self, provider: &mut B) -> Result<(), Error> {
        let mut failed = false;
        for frame in self.frames {
            failed |= provider.release(frame).is_err();
        }
        if failed { Err(Error::Backend) } else { Ok(()) }
    }
}

/// Validate a page-aligned range before indexing backing or mapping metadata.
pub fn page_range(
    offset: u64,
    length: u64,
    limit: u64,
    page: u64,
) -> Result<core::ops::Range<usize>, Error> {
    if !page.is_power_of_two() || length == 0 || offset % page != 0 || length % page != 0 {
        return Err(Error::InvalidSize);
    }
    let end = offset
        .checked_add(length)
        .filter(|end| *end <= limit)
        .ok_or(Error::InvalidSize)?;
    Ok(
        usize::try_from(offset / page).map_err(|_| Error::InvalidSize)?
            ..usize::try_from(end / page).map_err(|_| Error::InvalidSize)?,
    )
}

#[cfg(all(test, feature = "sel4"))]
mod allocator_tests;
#[cfg(test)]
mod tests;
