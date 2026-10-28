//! What the reader and the writer share: the header and the index's parts (FORMAT.md).

use crate::error::{Result, corrupt};

/// The magic, then the version.
pub const MAGIC: &[u8; 5] = b"KAFRA";
/// The format version this crate reads and writes.
pub const VERSION: u8 = 1;
/// The magic and version, the index's u64 offset, u32 stored size and u32 size.
pub const HEADER_SIZE: u64 = 22;

/// A chunk's codec.
pub const STORED: u8 = 0;
pub const ZSTD: u8 = 1;

pub struct Cursor<'a> {
    pub bytes: &'a [u8],
    pub at: usize,
}

impl<'a> Cursor<'a> {
    pub fn take(&mut self, length: usize) -> Result<&'a [u8]> {
        let bytes = self.at.checked_add(length).and_then(|end| self.bytes.get(self.at..end)).ok_or_else(|| corrupt("truncated"))?;
        self.at += length;
        Ok(bytes)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    pub fn u32(&mut self) -> Result<u32> {
        Ok(le32(self.take(4)?))
    }

    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
}

pub fn le32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes[..4].try_into().unwrap())
}

/// A path as entries are keyed: lowercase (ASCII), with backslashes.
pub fn normalize(path: &str) -> String {
    path.replace('/', "\\").to_ascii_lowercase()
}

#[derive(Clone, Copy)]
pub struct Chunk {
    pub offset: u64,
    pub stored: u32,
    pub size: u32,
    pub codec: u8,
}

#[derive(Clone, Copy)]
pub struct Entry {
    pub chunk: u32,
    pub offset: u32,
    pub length: u32,
    pub transform: u8,
    pub flags: u8,
    pub size: u32,
}
