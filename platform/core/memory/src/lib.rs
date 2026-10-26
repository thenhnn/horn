//! Resident backing, independent of address-space placement and RPC.
#![no_std]

extern crate alloc;

use alloc::vec::Vec;

#[cfg(feature = "sel4")]
pub mod sel4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidSize,
    Exhausted,
    MetadataExhausted,
    Backend,
}

/// A trusted backing provider. A failed release must quarantine the frame:
/// it must never return possibly-live backing to the free pool.
pub trait Frames {
    type Frame;
    fn page_size(&self) -> usize;
    fn allocate_zeroed(&mut self) -> Result<Self::Frame, Error>;
    fn release(&mut self, frame: Self::Frame) -> Result<(), Error>;
}

/// Owns backing tokens, not virtual addresses or external mappings.
///
/// Explicit release is required. Dropping this value leaks its backing rather
/// than making potentially-live frames available for reuse. Mapping/handle
/// references and automatic process-death cleanup are not implemented yet.
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
        frames.try_reserve_exact(count).map_err(|_| Error::MetadataExhausted)?;
        for _ in 0..count {
            match provider.allocate_zeroed() {
                Ok(frame) => frames.push(frame),
                Err(error) => {
                    let mut cleanup_failed = false;
                    for frame in frames {
                        cleanup_failed |= provider.release(frame).is_err();
                    }
                    return Err(if cleanup_failed { Error::Backend } else { error });
                }
            }
        }
        Ok(Self { size, resident_size, frames })
    }

    pub fn size(&self) -> u64 { self.size }
    pub fn resident_size(&self) -> u64 { self.resident_size }
    pub fn page_count(&self) -> usize { self.frames.len() }
    /// Trusted provider tokens; these do not expose raw frame capabilities.
    pub fn frames(&self) -> &[F] { &self.frames }

    pub fn release<B: Frames<Frame = F>>(self, provider: &mut B) -> Result<(), Error> {
        let mut failed = false;
        for frame in self.frames {
            failed |= provider.release(frame).is_err();
        }
        if failed { Err(Error::Backend) } else { Ok(()) }
    }
}

#[cfg(test)]
mod tests;
