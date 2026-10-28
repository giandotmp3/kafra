//! Reading a `.kafra`: its index when it opens, then any file by its path, in its original format.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::error::{Error, Result, corrupt};
use crate::format::{Chunk, Cursor, Entry, HEADER_SIZE, MAGIC, STORED, VERSION, ZSTD, le32, normalize};
use crate::transform;

/// How many decompressed chunks are kept: files that sit together are often read together.
const CACHED_CHUNKS: usize = 8;

/// How an entry's data becomes its file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transform {
    /// Kept as it was.
    Raw,
    /// A picture (BMP, TGA, JPG or PNG) stored as AVIF.
    Image,
    /// A WAV stored as Opus.
    Sound,
    /// A map's GND, its lightmaps as AVIF.
    Ground,
    /// A map's GAT, told against its GND.
    Altitude,
    /// One this reader doesn't know: a newer writer's.
    Unknown(u8),
}

impl Transform {
    fn from_byte(byte: u8) -> Self {
        match byte {
            transform::RAW => Self::Raw,
            transform::IMAGE => Self::Image,
            transform::SOUND => Self::Sound,
            transform::GROUND => Self::Ground,
            transform::ALTITUDE => Self::Altitude,
            other => Self::Unknown(other),
        }
    }

    /// Its number in the index.
    pub fn byte(self) -> u8 {
        match self {
            Self::Raw => transform::RAW,
            Self::Image => transform::IMAGE,
            Self::Sound => transform::SOUND,
            Self::Ground => transform::GROUND,
            Self::Altitude => transform::ALTITUDE,
            Self::Unknown(byte) => byte,
        }
    }

    /// Its name, as `kafra info` and `kafra list` show it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Image => "image",
            Self::Sound => "sound",
            Self::Ground => "ground",
            Self::Altitude => "altitude",
            Self::Unknown(_) => "unknown",
        }
    }
}

/// What the index says about a file.
#[derive(Clone, Copy, Debug)]
pub struct EntryInfo {
    /// How its data becomes the file.
    pub transform: Transform,
    /// Its data's size in the `.kafra`, before any chunk compression.
    pub stored_size: u32,
    /// The size of the file it rebuilds.
    pub size: u32,
}

/// An open `.kafra`. Safe to share between threads: reads take `&self`.
pub struct Kafra {
    file: Mutex<File>,
    chunks: Vec<Chunk>,
    entries: HashMap<String, Entry>,
    /// The zstd chunks decompressed last, the newest at the end.
    cache: Mutex<Vec<(u32, Arc<Vec<u8>>)>>,
}

impl Kafra {
    /// Opens a `.kafra` and reads its index, checking that every chunk is inside the file and every entry inside its
    /// chunk.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        let mut header = [0; HEADER_SIZE as usize];
        file.read_exact(&mut header).map_err(|_| Error::BadMagic)?;
        if &header[..5] != MAGIC {
            return Err(Error::BadMagic);
        }
        if header[5] != VERSION {
            return Err(Error::UnsupportedVersion(header[5]));
        }
        let index_offset = u64::from_le_bytes(header[6..14].try_into().unwrap());
        let (stored, size) = (le32(&header[14..]), le32(&header[18..]));
        if index_offset < HEADER_SIZE || index_offset + u64::from(stored) > length {
            return Err(corrupt("the index is outside the file"));
        }
        let mut compressed = vec![0; stored as usize];
        file.seek(SeekFrom::Start(index_offset))?;
        file.read_exact(&mut compressed)?;
        let index = unzstd(&compressed, size)?;

        let mut cursor = Cursor { bytes: &index, at: 0 };
        let chunk_count = cursor.u32()? as usize;
        let mut chunks = Vec::with_capacity(chunk_count.min(index.len() / 21));
        for _ in 0..chunk_count {
            let chunk = Chunk { offset: cursor.u64()?, stored: cursor.u32()?, size: cursor.u32()?, codec: cursor.u8()? };
            if chunk.offset < HEADER_SIZE || chunk.offset + u64::from(chunk.stored) > index_offset {
                return Err(corrupt("a chunk outside the file"));
            }
            match chunk.codec {
                STORED if chunk.stored != chunk.size => return Err(corrupt("a stored chunk whose sizes differ")),
                STORED | ZSTD => {}
                _ => return Err(corrupt("a chunk of an unknown codec")),
            }
            chunks.push(chunk);
        }
        let count = cursor.u32()? as usize;
        let mut entries = HashMap::with_capacity(count.min(index.len() / 20));
        for _ in 0..count {
            let length = usize::from(cursor.u16()?);
            let path = std::str::from_utf8(cursor.take(length)?).map_err(|_| corrupt("a path that isn't UTF-8"))?;
            let entry =
                Entry { chunk: cursor.u32()?, offset: cursor.u32()?, length: cursor.u32()?, transform: cursor.u8()?, flags: cursor.u8()?, size: cursor.u32()? };
            let chunk = chunks.get(entry.chunk as usize).ok_or_else(|| corrupt("an entry in a chunk that isn't there"))?;
            if u64::from(entry.offset) + u64::from(entry.length) > u64::from(chunk.size) {
                return Err(corrupt("an entry past the end of its chunk"));
            }
            entries.insert(normalize(path), entry);
        }
        Ok(Self { file: Mutex::new(file), chunks, entries, cache: Mutex::new(Vec::new()) })
    }

    /// How many files it has.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether it has no files.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many chunks the files' data is in.
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Every file's path, lowercase with backslashes, in no order.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// Every file's path and what the index says about it, in no order.
    pub fn entries(&self) -> impl Iterator<Item = (&str, EntryInfo)> {
        self.entries.iter().map(|(path, entry)| (path.as_str(), info(entry)))
    }

    /// Whether it has a file; case and slash direction don't matter.
    pub fn contains(&self, path: &str) -> bool {
        self.entries.contains_key(&normalize(path))
    }

    /// What the index says about a file, or None if there's none at that path; case and slash direction don't matter.
    pub fn entry(&self, path: &str) -> Option<EntryInfo> {
        self.entries.get(&normalize(path)).map(info)
    }

    /// A file in its original format, e.g. `data\prontera.gnd`; case and slash direction don't matter.
    pub fn read(&self, path: &str) -> Result<Vec<u8>> {
        let entry = self.find(path)?;
        let data = self.data(entry)?;
        let ground = if entry.transform == transform::ALTITUDE {
            let (ground_path, _) = transform::altitude_ground(&data)?;
            let ground = self.entries.get(&normalize(ground_path)).ok_or_else(|| corrupt("an altitude without its ground"))?;
            if ground.transform != transform::GROUND {
                return Err(corrupt("an altitude told against a file that isn't a ground"));
            }
            Some(self.data(*ground)?)
        } else {
            None
        };
        let file = transform::rebuild(entry.transform, entry.flags, &data, ground.as_deref())?;
        if file.len() != entry.size as usize {
            return Err(corrupt("a file that rebuilds to the wrong size"));
        }
        Ok(file)
    }

    /// A file's data as stored, its transform not undone: an AVIF, an Ogg Opus stream, a ground's columns...
    pub fn read_stored(&self, path: &str) -> Result<Vec<u8>> {
        self.data(self.find(path)?)
    }

    fn find(&self, path: &str) -> Result<Entry> {
        self.entries.get(&normalize(path)).copied().ok_or_else(|| Error::NotFound(path.to_owned()))
    }

    fn data(&self, entry: Entry) -> Result<Vec<u8>> {
        let chunk = self.chunks[entry.chunk as usize];
        let (start, end) = (entry.offset as usize, entry.offset as usize + entry.length as usize);
        if chunk.codec == STORED {
            let mut data = vec![0; entry.length as usize];
            let mut file = self.file.lock().unwrap();
            file.seek(SeekFrom::Start(chunk.offset + start as u64))?;
            file.read_exact(&mut data)?;
            return Ok(data);
        }
        Ok(self.chunk(entry.chunk)?[start..end].to_vec())
    }

    fn chunk(&self, index: u32) -> Result<Arc<Vec<u8>>> {
        if let Some((_, data)) = self.cache.lock().unwrap().iter().find(|(cached, _)| *cached == index) {
            return Ok(data.clone());
        }
        let chunk = self.chunks[index as usize];
        let mut compressed = vec![0; chunk.stored as usize];
        {
            let mut file = self.file.lock().unwrap();
            file.seek(SeekFrom::Start(chunk.offset))?;
            file.read_exact(&mut compressed)?;
        }
        let data = Arc::new(unzstd(&compressed, chunk.size)?);
        let mut cache = self.cache.lock().unwrap();
        if cache.len() == CACHED_CHUNKS {
            cache.remove(0);
        }
        cache.push((index, data.clone()));
        Ok(data)
    }
}

fn info(entry: &Entry) -> EntryInfo {
    EntryInfo { transform: Transform::from_byte(entry.transform), stored_size: entry.length, size: entry.size }
}

/// zstd data that must come to `size` bytes; it stops reading past them.
fn unzstd(bytes: &[u8], size: u32) -> Result<Vec<u8>> {
    let mut source = bytes;
    let decoder = ruzstd::decoding::StreamingDecoder::new(&mut source).map_err(|error| corrupt(&format!("zstd: {error}")))?;
    let mut out = Vec::new();
    decoder.take(u64::from(size) + 1).read_to_end(&mut out).map_err(|error| corrupt(&format!("zstd: {error}")))?;
    if out.len() != size as usize {
        return Err(corrupt("a chunk of the wrong size"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::Writer;

    fn compress(data: &[u8]) -> std::io::Result<Vec<u8>> {
        Ok(ruzstd::encoding::compress_to_vec(data, ruzstd::encoding::CompressionLevel::Fastest))
    }

    /// A `.kafra` of raw files: two in a zstd chunk, one in a stored chunk. Returns its bytes.
    fn small() -> Vec<u8> {
        let path = std::env::temp_dir().join(format!("kafra-reader-{}-{:?}.kafra", std::process::id(), std::thread::current().id()));
        let mut writer = Writer::create(&path).unwrap();
        let chunk = b"hello kafra, hello kafra\x01\x02\x03";
        let zstd = writer.chunk(chunk, Some(&compress(chunk).unwrap())).unwrap();
        writer.entry("data\\a.txt", zstd, 0, 24, transform::RAW, 0, 24);
        writer.entry("data\\b.bin", zstd, 24, 3, transform::RAW, 0, 3);
        let stored = writer.chunk(b"raw bytes", None).unwrap();
        writer.entry("data\\c.bin", stored, 4, 5, transform::RAW, 0, 5);
        writer.finish(compress).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        bytes
    }

    fn open(bytes: &[u8]) -> Result<Kafra> {
        let path = std::env::temp_dir().join(format!("kafra-reader-open-{}-{:?}.kafra", std::process::id(), std::thread::current().id()));
        std::fs::write(&path, bytes).unwrap();
        let kafra = Kafra::open(&path);
        std::fs::remove_file(path).unwrap();
        kafra
    }

    #[test]
    fn reads_what_the_writer_wrote() {
        let kafra = open(&small()).unwrap();
        assert_eq!((kafra.len(), kafra.chunk_count()), (3, 2));
        assert_eq!(kafra.read("data/A.txt").unwrap(), b"hello kafra, hello kafra");
        assert_eq!(kafra.read("data\\b.bin").unwrap(), [1, 2, 3]);
        assert_eq!(kafra.read("data\\c.bin").unwrap(), b"bytes");
        assert_eq!(kafra.read_stored("data\\c.bin").unwrap(), b"bytes");
        let info = kafra.entry("data\\b.bin").unwrap();
        assert_eq!((info.transform, info.stored_size, info.size), (Transform::Raw, 3, 3));
        assert!(matches!(kafra.read("data\\missing"), Err(Error::NotFound(_))));
    }

    #[test]
    fn refuses_what_isnt_a_kafra() {
        assert!(matches!(open(b"Master of Magic\0 and the rest of a GRF"), Err(Error::BadMagic)));
        assert!(matches!(open(b"KAF"), Err(Error::BadMagic)));
        let mut newer = small();
        newer[5] = 2;
        assert!(matches!(open(&newer), Err(Error::UnsupportedVersion(2))));
    }

    /// Cut anywhere, a `.kafra` gives an error: never a panic, nor a file.
    #[test]
    fn refuses_a_truncated_file() {
        let bytes = small();
        for length in 0..bytes.len() {
            assert!(open(&bytes[..length]).is_err(), "cut at {length}");
        }
    }

    /// Any byte of the index changed, it opens with an error or reads with errors: never a panic.
    #[test]
    fn survives_a_damaged_index() {
        let bytes = small();
        let index = u64::from_le_bytes(bytes[6..14].try_into().unwrap()) as usize;
        for at in (6..HEADER_SIZE as usize).chain(index..bytes.len()) {
            for value in [0x00, 0x7F, 0xFF] {
                let mut damaged = bytes.clone();
                damaged[at] = value;
                if let Ok(kafra) = open(&damaged) {
                    for path in kafra.paths().map(str::to_owned).collect::<Vec<_>>() {
                        let _ = kafra.read(&path);
                    }
                }
            }
        }
    }

    #[test]
    fn refuses_an_entry_past_its_chunk() {
        let path = std::env::temp_dir().join(format!("kafra-reader-past-{}.kafra", std::process::id()));
        let mut writer = Writer::create(&path).unwrap();
        let stored = writer.chunk(b"short", None).unwrap();
        writer.entry("data\\a.bin", stored, 3, 10, transform::RAW, 0, 10);
        writer.finish(compress).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert!(matches!(open(&bytes), Err(Error::Corrupt(_))));
    }

    /// A newer writer's transform: the file opens and lists it, and reading it says what's wrong.
    #[test]
    fn reports_an_unknown_transform() {
        let path = std::env::temp_dir().join(format!("kafra-reader-transform-{}.kafra", std::process::id()));
        let mut writer = Writer::create(&path).unwrap();
        let stored = writer.chunk(b"data", None).unwrap();
        writer.entry("data\\a.new", stored, 0, 4, 200, 0, 4);
        writer.finish(compress).unwrap();
        let kafra = Kafra::open(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(kafra.entry("data\\a.new").unwrap().transform, Transform::Unknown(200));
        assert!(matches!(kafra.read("data\\a.new"), Err(Error::UnknownTransform(200))));
        assert_eq!(kafra.read_stored("data\\a.new").unwrap(), b"data");
    }
}
