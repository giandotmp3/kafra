//! Reader for GRF 0x200 archives, and the 0x300 ones ("Event Horizon") of the newer clients; and `build`, which
//! makes a small one (for tests and samples).

mod mixcrypt;

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;

use flate2::read::ZlibDecoder;

use crate::format::normalize;

const HEADER_SIZE: u64 = 46;
/// Entries without this flag are directories.
const FLAG_FILE: u8 = 0x01;

/// The entry that tools encrypting an archive's file data add to it.
const ENCRYPTED_MARK: &str = "__encryption.info";

struct Entry {
    compressed_size: u32,
    aligned_size: u32,
    size: u32,
    flags: u8,
    offset: u64,
}

/// An open GRF. Safe to share between threads: reads take `&self`.
pub struct Grf {
    file: Mutex<File>,
    entries: HashMap<String, Entry>,
}

impl Grf {
    /// The game data for tests that read it (`KAFRA_GRF`, or `./data.grf`), or None, to skip them, where there's
    /// none (game data stays out of the repository).
    #[cfg(test)]
    pub fn for_tests() -> Option<Self> {
        let path = std::env::var("KAFRA_GRF").unwrap_or_else(|_| "data.grf".into());
        match Self::open(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                eprintln!("skipped: no {path}");
                None
            }
            grf => Some(grf.unwrap_or_else(|error| panic!("{path}: {error}"))),
        }
    }

    /// Opens a GRF and reads its file table. Fails on versions other than 0x200 and 0x300, and on archives whose file
    /// data a third-party tool encrypted.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let mut file = File::open(path)?;
        let mut header = [0u8; HEADER_SIZE as usize];
        file.read_exact(&mut header)?;
        if !header.starts_with(b"Master of Magic") && !header.starts_with(b"Event Horizon") {
            return Err(invalid("not a GRF file"));
        }
        // 0x300 widens the table offset and each entry's offset to 64 bits, and puts a zero word before the table's sizes.
        let version = le32(&header[42..]);
        let (table_offset, table_pad, entry_size) = match version {
            0x200 => (u64::from(le32(&header[30..])), 0, 17),
            0x300 => (u64::from_le_bytes(header[30..38].try_into().unwrap()), 4, 21),
            _ => return Err(invalid("only GRF versions 0x200 and 0x300 are supported")),
        };

        file.seek(SeekFrom::Start(HEADER_SIZE + table_offset + table_pad))?;
        let mut sizes = [0u8; 8];
        file.read_exact(&mut sizes)?;
        let mut compressed_table = vec![0; le32(&sizes) as usize];
        file.read_exact(&mut compressed_table)?;
        let mut table = Vec::with_capacity(le32(&sizes[4..]) as usize);
        ZlibDecoder::new(&compressed_table[..]).read_to_end(&mut table)?;

        let mut entries = HashMap::new();
        let mut rest = &table[..];
        while let Some(name_end) = rest.iter().position(|&byte| byte == 0) {
            let fields = rest.get(name_end + 1..name_end + 1 + entry_size).ok_or_else(|| invalid("truncated file table"))?;
            let entry = Entry {
                compressed_size: le32(fields),
                aligned_size: le32(&fields[4..]),
                size: le32(&fields[8..]),
                flags: fields[12],
                offset: if version == 0x300 { u64::from_le_bytes(fields[13..21].try_into().unwrap()) } else { u64::from(le32(&fields[13..])) },
            };
            if entry.flags & FLAG_FILE != 0 {
                let (name, _) = encoding_rs::EUC_KR.decode_without_bom_handling(&rest[..name_end]);
                entries.insert(normalize(&name), entry);
            }
            rest = &rest[name_end + 1 + entry_size..];
        }
        if entries.contains_key(ENCRYPTED_MARK) {
            return Err(invalid("its file data is encrypted"));
        }
        Ok(Self { file: Mutex::new(file), entries })
    }

    /// Every file's path, lowercase with backslashes.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// Reads a file by its client path, e.g. `data\texture\...`; case and slash direction don't matter.
    pub fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        let entry = self.entries.get(&normalize(path)).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, path.to_owned()))?;
        let mut data = vec![0; entry.aligned_size as usize];
        {
            let mut file = self.file.lock().unwrap();
            file.seek(SeekFrom::Start(HEADER_SIZE + entry.offset))?;
            file.read_exact(&mut data)?;
        }
        mixcrypt::decrypt(entry.flags, entry.compressed_size, &mut data);
        inflate(&data, entry.size)
    }
}

/// A GRF of `version` 0x200 or 0x300 holding `files` (path, contents), each zlib-compressed and none encrypted.
pub fn build(version: u32, files: &[(&str, &[u8])]) -> Vec<u8> {
    let (mut body, mut table) = (Vec::new(), Vec::new());
    for (name, contents) in files {
        let stored = zlib(contents);
        table.extend(&*encoding_rs::EUC_KR.encode(name).0);
        table.push(0);
        table.extend((stored.len() as u32).to_le_bytes());
        table.extend((stored.len() as u32).to_le_bytes());
        table.extend((contents.len() as u32).to_le_bytes());
        table.push(FLAG_FILE);
        match version {
            0x300 => table.extend((body.len() as u64).to_le_bytes()),
            _ => table.extend((body.len() as u32).to_le_bytes()),
        }
        body.extend(stored);
    }
    let compressed_table = zlib(&table);
    let mut file = match version {
        0x300 => b"Event Horizon\0c\0".to_vec(),
        _ => b"Master of Magic\0".to_vec(),
    };
    file.extend(1u8..=14);
    match version {
        0x300 => file.extend((body.len() as u64).to_le_bytes()),
        _ => file.extend([(body.len() as u32).to_le_bytes(), 0u32.to_le_bytes()].concat()),
    }
    file.extend((files.len() as u32).to_le_bytes());
    file.extend(version.to_le_bytes());
    file.extend(&body);
    if version == 0x300 {
        file.extend(0u32.to_le_bytes());
    }
    file.extend((compressed_table.len() as u32).to_le_bytes());
    file.extend((table.len() as u32).to_le_bytes());
    file.extend(&compressed_table);
    file
}

fn zlib(bytes: &[u8]) -> Vec<u8> {
    use flate2::{Compression, write::ZlibEncoder};
    use std::io::Write;
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn inflate(data: &[u8], size: u32) -> io::Result<Vec<u8>> {
    let mut decompressed = Vec::with_capacity(size as usize);
    ZlibDecoder::new(data).read_to_end(&mut decompressed)?;
    Ok(decompressed)
}

fn le32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes[..4].try_into().unwrap())
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}

#[cfg(test)]
mod tests {
    /// Needs the official data.grf, at `KAFRA_GRF` or `./data.grf`.
    #[test]
    fn reads_plain_and_encrypted_files() {
        let Some(grf) = super::Grf::for_tests() else { return };

        // flags 1: plain zlib.
        let plain = grf.read("data/texture/유저인터페이스/login_interface/bg_login.tga").unwrap();
        assert_eq!(plain.len(), 158_972);

        // flags 3: mixcrypt; a wrong decryption fails zlib or yields garbage instead of a BMP.
        let encrypted = grf.read("DATA\\texture\\유저인터페이스\\login_interface\\win_login.bmp").unwrap();
        assert_eq!(&encrypted[..2], b"BM");
        assert_eq!(encrypted.len(), 34_672);

        assert!(grf.read("data\\does\\not\\exist.bmp").is_err());
    }

    fn read_back(test: &str, version: u32, files: &[(&str, &[u8])]) -> super::Grf {
        let path = std::env::temp_dir().join(format!("kafra-grf-{test}-{}.grf", std::process::id()));
        std::fs::write(&path, super::build(version, files)).unwrap();
        let grf = super::Grf::open(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        grf
    }

    #[test]
    fn reads_a_0x200_archive() {
        let grf = read_back("0x200", 0x200, &[("data\\a.txt", b"old grf")]);
        assert_eq!(grf.read("data/A.TXT").unwrap(), b"old grf");
        assert_eq!(grf.paths().collect::<Vec<_>>(), ["data\\a.txt"]);
    }

    /// 0x300: 64-bit offsets, the zero word before the table, "Event Horizon" for a magic.
    #[test]
    fn reads_a_0x300_archive() {
        let grf = read_back("0x300", 0x300, &[("data\\sub\\hello.txt", b"hello grf")]);
        assert_eq!(grf.read("DATA/sub/Hello.txt").unwrap(), b"hello grf");
    }

    /// Names are EUC-KR in the archive, UTF-8 when read.
    #[test]
    fn reads_korean_names() {
        let grf = read_back("korean", 0x200, &[("data\\유저인터페이스\\a.bmp", b"BM")]);
        assert_eq!(grf.read("data/유저인터페이스/a.bmp").unwrap(), b"BM");
    }
}
