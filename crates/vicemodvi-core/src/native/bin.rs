//! Bounds-checked little-endian reads over untrusted bytes.
//!
//! Every read names an absolute offset and fails with an error instead of
//! panicking when the bytes are not there. Offsets that come from the data
//! are combined with checked arithmetic before they are used.

use std::ops::Range;

use crate::{Error, Result};

/// Virtual base of the system (CPU) section in resource pointers.
pub(crate) const SYSTEM_BASE: u64 = 0x5000_0000;
/// Virtual base of the graphics (GPU) section in resource pointers.
pub(crate) const GRAPHICS_BASE: u64 = 0x6000_0000;

pub(crate) fn outside() -> Error {
    Error::invalid("A resource field points outside its section.")
}

/// `start..start + len` when it lies inside `0..total`.
pub(crate) fn range(total: usize, start: usize, len: usize) -> Result<Range<usize>> {
    match start.checked_add(len) {
        Some(end) if end <= total => Ok(start..end),
        _ => Err(outside()),
    }
}

/// A read-only view with typed accessors.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Bin<'a>(pub &'a [u8]);

impl<'a> Bin<'a> {
    pub fn len(self) -> usize {
        self.0.len()
    }

    pub fn bytes(self, at: usize, len: usize) -> Result<&'a [u8]> {
        Ok(&self.0[range(self.0.len(), at, len)?])
    }

    fn array<const N: usize>(self, at: usize) -> Result<[u8; N]> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.bytes(at, N)?);
        Ok(out)
    }

    pub fn u8(self, at: usize) -> Result<u8> {
        Ok(self.array::<1>(at)?[0])
    }

    pub fn u16(self, at: usize) -> Result<u16> {
        self.array(at).map(u16::from_le_bytes)
    }

    pub fn i16(self, at: usize) -> Result<i16> {
        self.array(at).map(i16::from_le_bytes)
    }

    pub fn u32(self, at: usize) -> Result<u32> {
        self.array(at).map(u32::from_le_bytes)
    }

    pub fn u64(self, at: usize) -> Result<u64> {
        self.array(at).map(u64::from_le_bytes)
    }

    pub fn f32(self, at: usize) -> Result<f32> {
        self.array(at).map(f32::from_le_bytes)
    }

    pub fn vec3(self, at: usize) -> Result<[f32; 3]> {
        Ok([self.f32(at)?, self.f32(at + 4)?, self.f32(at + 8)?])
    }

    /// Like [`Bin::u64`], but zero when the bytes are missing.
    pub fn u64_or_zero(self, at: usize) -> u64 {
        self.u64(at).unwrap_or(0)
    }

    /// Like [`Bin::u16`], but zero when the bytes are missing.
    pub fn u16_or_zero(self, at: usize) -> u16 {
        self.u16(at).unwrap_or(0)
    }

    /// Offset of the system-section pointer stored at `at`. The target must
    /// leave `need` readable bytes.
    pub fn system(self, at: usize, need: usize) -> Result<usize> {
        let target = system_offset(self.u64(at)?, self.0.len())?;
        range(self.0.len(), target, need)?;
        Ok(target)
    }
}

/// Converts a system pointer to an offset below `limit`.
pub(crate) fn system_offset(pointer: u64, limit: usize) -> Result<usize> {
    pointer
        .checked_sub(SYSTEM_BASE)
        .filter(|&offset| offset <= limit as u64)
        .map(|offset| offset as usize)
        .ok_or_else(|| Error::invalid("Invalid system-section pointer."))
}

/// A NUL-terminated UTF-8 name of 1 to `max - 1` bytes without control
/// characters.
pub(crate) fn c_name(data: &[u8], at: usize, max: usize) -> Option<String> {
    let window = data.get(at..)?;
    let window = &window[..window.len().min(max)];
    let end = window.iter().position(|&b| b == 0)?;
    let text = std::str::from_utf8(&window[..end]).ok()?;
    (end > 0 && !text.chars().any(char::is_control)).then(|| text.to_string())
}

pub(crate) fn all_zero(data: &[u8]) -> bool {
    data.iter().all(|&b| b == 0)
}

pub(crate) fn align(value: u64, boundary: u64) -> u64 {
    value.div_ceil(boundary) * boundary
}

/// Little-endian output buffer.
#[derive(Default)]
pub(crate) struct Out(pub Vec<u8>);

impl Out {
    pub fn with_capacity(capacity: usize) -> Self {
        Self(Vec::with_capacity(capacity))
    }

    pub fn u32(&mut self, value: u32) -> &mut Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub fn raw(&mut self, bytes: &[u8]) -> &mut Self {
        self.0.extend_from_slice(bytes);
        self
    }

    pub fn zeros(&mut self, count: usize) -> &mut Self {
        self.0.resize(self.0.len() + count, 0);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_fail_outside_the_buffer() {
        let data = Bin(&[1, 0, 0, 0, 2]);
        assert_eq!(data.u32(0).unwrap(), 1);
        assert!(data.u32(2).is_err());
        assert!(data.bytes(usize::MAX, 2).is_err());
        assert_eq!(c_name(b"abc\0", 0, 256).as_deref(), Some("abc"));
        assert!(c_name(b"\0", 0, 256).is_none());
        assert!(c_name(b"abc", 0, 256).is_none());
    }
}
