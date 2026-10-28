//! Writing a `.kafra`'s chunks, entries and index (FORMAT.md). `store` decides what goes in them.

use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;

use crate::format::{Chunk, Entry, HEADER_SIZE, MAGIC, STORED, VERSION, ZSTD};

/// Writes a `.kafra`: chunks as they come, the entries in them, and the index at the end. The paths are as a reader
/// looks them up (`format::normalize`).
pub struct Writer {
    file: io::BufWriter<File>,
    position: u64,
    chunks: Vec<Chunk>,
    entries: Vec<(String, Entry)>,
}

impl Writer {
    /// Creates the file at `path`, leaving room for the header that `finish` writes.
    pub fn create(path: &Path) -> io::Result<Self> {
        let mut file = io::BufWriter::new(File::create(path)?);
        file.write_all(&[0; HEADER_SIZE as usize])?;
        Ok(Self { file, position: HEADER_SIZE, chunks: Vec::new(), entries: Vec::new() })
    }

    /// Adds a chunk: `data` as it is, or zstd-compressed as `compressed`. Returns its number.
    pub fn chunk(&mut self, data: &[u8], compressed: Option<&[u8]>) -> io::Result<u32> {
        let (bytes, codec) = compressed.map_or((data, STORED), |compressed| (compressed, ZSTD));
        self.file.write_all(bytes)?;
        self.chunks.push(Chunk { offset: self.position, stored: bytes.len() as u32, size: data.len() as u32, codec });
        self.position += bytes.len() as u64;
        Ok(self.chunks.len() as u32 - 1)
    }

    /// A file whose data is `length` bytes at `offset` in a chunk's (uncompressed) data.
    #[allow(clippy::too_many_arguments)]
    pub fn entry(&mut self, path: &str, chunk: u32, offset: u32, length: u32, transform: u8, flags: u8, size: u32) {
        self.entries.push((path.to_owned(), Entry { chunk, offset, length, transform, flags, size }));
    }

    /// Writes the index, compressed by `compress` (zstd), and the header.
    pub fn finish(mut self, compress: impl Fn(&[u8]) -> io::Result<Vec<u8>>) -> io::Result<u64> {
        let mut index = Vec::new();
        index.extend((self.chunks.len() as u32).to_le_bytes());
        for chunk in &self.chunks {
            index.extend(chunk.offset.to_le_bytes());
            index.extend(chunk.stored.to_le_bytes());
            index.extend(chunk.size.to_le_bytes());
            index.push(chunk.codec);
        }
        index.extend((self.entries.len() as u32).to_le_bytes());
        for (path, entry) in &self.entries {
            index.extend((path.len() as u16).to_le_bytes());
            index.extend(path.as_bytes());
            for value in [entry.chunk, entry.offset, entry.length] {
                index.extend(value.to_le_bytes());
            }
            index.extend([entry.transform, entry.flags]);
            index.extend(entry.size.to_le_bytes());
        }
        let compressed = compress(&index)?;
        self.file.write_all(&compressed)?;
        let mut file = self.file.into_inner().map_err(io::IntoInnerError::into_error)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(MAGIC)?;
        file.write_all(&[VERSION])?;
        file.write_all(&self.position.to_le_bytes())?;
        file.write_all(&(compressed.len() as u32).to_le_bytes())?;
        file.write_all(&(index.len() as u32).to_le_bytes())?;
        Ok(self.position + compressed.len() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::le32;

    /// The header points at the index, which lists the chunks and the entries in them (FORMAT.md).
    #[test]
    fn writes_the_header_and_index() {
        let path = std::env::temp_dir().join(format!("kafra-writer-{}.kafra", std::process::id()));
        let compress = |data: &[u8]| Ok(ruzstd::encoding::compress_to_vec(data, ruzstd::encoding::CompressionLevel::Fastest));
        let mut writer = Writer::create(&path).unwrap();
        let chunk = writer.chunk(b"raw bytes", None).unwrap();
        writer.entry("data\\c.bin", chunk, 4, 5, 0, 0, 5);
        let size = writer.finish(compress).unwrap();
        let file = std::fs::read(&path).unwrap();
        std::fs::remove_file(path).unwrap();

        assert_eq!(file.len() as u64, size);
        assert_eq!(&file[..6], b"KAFRA\x01");
        let offset = u64::from_le_bytes(file[6..14].try_into().unwrap()) as usize;
        assert_eq!(&file[HEADER_SIZE as usize..offset], b"raw bytes");
        let mut index = Vec::new();
        let mut source = &file[offset..offset + le32(&file[14..]) as usize];
        std::io::Read::read_to_end(&mut ruzstd::decoding::StreamingDecoder::new(&mut source).unwrap(), &mut index).unwrap();
        assert_eq!(index.len(), le32(&file[18..]) as usize);
        let mut expected = 1u32.to_le_bytes().to_vec();
        expected.extend((HEADER_SIZE).to_le_bytes());
        expected.extend([9, 0, 0, 0, 9, 0, 0, 0, STORED]);
        expected.extend(1u32.to_le_bytes());
        expected.extend(10u16.to_le_bytes());
        expected.extend(b"data\\c.bin");
        expected.extend([0, 0, 0, 0, 4, 0, 0, 0, 5, 0, 0, 0, 0, 0, 5, 0, 0, 0]);
        assert_eq!(index, expected);
    }
}
