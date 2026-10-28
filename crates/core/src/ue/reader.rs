//! Bounds-checked little-endian reader for Unreal's serialization formats.

use anyhow::{bail, Context, Result};

pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.data.len());
        let end = end.with_context(|| format!("read of {n} bytes at {} past end ({})", self.pos, self.data.len()))?;
        let out = &self.data[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.bytes(n).map(|_| ())
    }

    pub fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.bytes(N)?.try_into().expect("length checked"))
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    /// `TArray` length prefix, sanity-checked against the bytes left so corrupt input
    /// fails instead of allocating gigabytes.
    pub fn count(&mut self, min_elem_size: usize) -> Result<usize> {
        let n = self.i32()?;
        if n < 0 || (n as usize).saturating_mul(min_elem_size.max(1)) > self.remaining() {
            bail!("implausible array length {n} at {}", self.pos - 4);
        }
        Ok(n as usize)
    }

    /// `FString`: i32 length including the terminator; negative means UTF-16.
    pub fn fstring(&mut self) -> Result<String> {
        let len = self.i32()?;
        if len == 0 {
            return Ok(String::new());
        }
        if len > 0 {
            let raw = self.bytes(len as usize)?;
            let raw = raw.strip_suffix(&[0]).unwrap_or(raw);
            return Ok(String::from_utf8_lossy(raw).into_owned());
        }
        let units = len.unsigned_abs() as usize;
        let raw = self.bytes(units.checked_mul(2).context("FString too long")?)?;
        let chars: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let chars = chars.strip_suffix(&[0]).unwrap_or(&chars);
        Ok(String::from_utf16_lossy(chars))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_ansi_and_utf16_fstrings() {
        let mut data = Vec::new();
        data.extend(4i32.to_le_bytes());
        data.extend(b"abc\0");
        data.extend((-3i32).to_le_bytes());
        data.extend([b'h', 0, b'i', 0, 0, 0]);
        data.extend(0i32.to_le_bytes());
        let mut r = Reader::new(&data);
        assert_eq!(r.fstring().unwrap(), "abc");
        assert_eq!(r.fstring().unwrap(), "hi");
        assert_eq!(r.fstring().unwrap(), "");
        assert!(r.u8().is_err());
    }

    #[test]
    fn rejects_implausible_counts() {
        let data = 1000i32.to_le_bytes();
        assert!(Reader::new(&data).count(4).is_err());
    }
}
