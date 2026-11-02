//! Provisioning descriptor: names/selectors only, never runtime addresses.
//! Magic followed by repeated LE (selector:u64, name_size:u64, ASCII name).
//! The sealed memory object's logical length bounds the stream. No padding.
use crate::startup::{Entry, Error};
const MAGIC: &[u8; 8] = b"HORNBND2";

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
pub struct Bindings<'a>(&'a [u8]);
impl<'a> Bindings<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        if !bytes.starts_with(MAGIC) {
            return Err(Error::Invalid);
        }
        let mut rest = &bytes[8..];
        let mut previous: Option<&[u8]> = None;
        while !rest.is_empty() {
            let head = rest.get(..16).ok_or(Error::Invalid)?;
            let selector = u64::from_le_bytes(head[..8].try_into().map_err(|_| Error::Invalid)?);
            let length = usize::try_from(u64::from_le_bytes(
                head[8..].try_into().map_err(|_| Error::Invalid)?,
            ))
            .map_err(|_| Error::Invalid)?;
            let end = 16usize.checked_add(length).ok_or(Error::Invalid)?;
            let name = rest.get(16..end).ok_or(Error::Invalid)?;
            if selector == 0
                || !crate::startup::valid_name(name)
                || previous.is_some_and(|p| p >= name)
            {
                return Err(Error::Invalid);
            }
            previous = Some(name);
            rest = &rest[end..];
        }
        Ok(Self(&bytes[8..]))
    }
    pub fn iter(self) -> impl Iterator<Item = Entry<'a>> + Clone {
        let mut rest = self.0;
        core::iter::from_fn(move || {
            if rest.is_empty() {
                return None;
            }
            // parse() validated all ranges and ASCII; borrowed bytes stay immutable.
            let selector = u64::from_le_bytes(rest[..8].try_into().unwrap());
            let length = u64::from_le_bytes(rest[8..16].try_into().unwrap()) as usize;
            let name = core::str::from_utf8(&rest[16..16 + length]).unwrap();
            rest = &rest[16 + length..];
            Some(Entry { name, selector })
        })
    }
}

pub fn encode(entries: &[Entry<'_>], out: &mut [u8]) -> Result<usize, Error> {
    let mut size = 8usize;
    let mut previous = None;
    for entry in entries {
        if entry.selector == 0
            || !crate::startup::valid_name(entry.name.as_bytes())
            || previous.is_some_and(|p: &str| p >= entry.name)
        {
            return Err(Error::Invalid);
        }
        previous = Some(entry.name);
        size = size
            .checked_add(16)
            .and_then(|n| n.checked_add(entry.name.len()))
            .ok_or(Error::Invalid)?;
    }
    if out.len() < size {
        return Err(Error::BufferTooSmall);
    }
    out[..8].copy_from_slice(MAGIC);
    let mut cursor = 8;
    for entry in entries {
        out[cursor..cursor + 8].copy_from_slice(&entry.selector.to_le_bytes());
        out[cursor + 8..cursor + 16].copy_from_slice(&(entry.name.len() as u64).to_le_bytes());
        out[cursor + 16..cursor + 16 + entry.name.len()].copy_from_slice(entry.name.as_bytes());
        cursor += 16 + entry.name.len();
    }
    Ok(size)
}
