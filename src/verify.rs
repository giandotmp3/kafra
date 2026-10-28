//! Checking a `.kafra` against the GRF it was made from, as far as each transform keeps a file:
//!
//! - raw: byte for byte.
//! - altitude (GAT): the same size and cell types, every height within half a hundredth.
//! - ground (GND): the same size, and byte for byte but for the lightmaps, which are lossy.
//! - image: the same size in pixels and alpha, the magenta colour keys exactly where they were; the colours are lossy.
//! - sound: a WAV of the same length, to a tenth of a second; the samples are lossy.

use image::{DynamicImage, ImageFormat};

use crate::format::normalize;
use crate::grf::Grf;
use crate::reader::Kafra;
use crate::store::in_parallel;
use crate::transform::{self, ALTITUDE, GROUND, IMAGE, RAW, SOUND};

/// How close a rebuilt file is to its original.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Match {
    /// Byte for byte.
    Exact,
    /// As close as its transform keeps it.
    Close,
}

/// What `verify` found.
#[derive(Debug, Default)]
pub struct Report {
    /// Files that came back byte for byte.
    pub exact: usize,
    /// Files that came back as close as their transform keeps them.
    pub close: usize,
    /// Files that didn't come back right, and why.
    pub failed: Vec<(String, String)>,
    /// Files in the `.kafra` that the GRF doesn't have.
    pub not_in_grf: Vec<String>,
    /// Files in the GRF that the `.kafra` doesn't have (left out when it was made, or missing).
    pub not_in_kafra: Vec<String>,
}

impl Report {
    /// Whether every file came back, and every one is in the GRF.
    pub fn ok(&self) -> bool {
        self.failed.is_empty() && self.not_in_grf.is_empty()
    }
}

/// Reads every file of `kafra` (under `prefix`, if given) and compares it with `grf`'s, on `jobs` threads.
pub fn verify(kafra: &Kafra, grf: &Grf, prefix: Option<&str>, jobs: usize) -> Report {
    let prefix = prefix.map(normalize).unwrap_or_default();
    let mut paths: Vec<(&str, u8)> =
        kafra.entries().filter(|(path, _)| path.starts_with(prefix.as_str())).map(|(path, info)| (path, info.transform.byte())).collect();
    paths.sort_unstable();
    let results = in_parallel(&paths, jobs.max(1), |&(path, transform), _| {
        let Ok(original) = grf.read(path) else { return None };
        Some(match kafra.read(path) {
            Ok(rebuilt) => compare(path, transform, &original, &rebuilt),
            Err(error) => Err(error.to_string()),
        })
    });
    let mut report = Report::default();
    for (&(path, _), result) in paths.iter().zip(results) {
        match result {
            None => report.not_in_grf.push(path.to_owned()),
            Some(Ok(Match::Exact)) => report.exact += 1,
            Some(Ok(Match::Close)) => report.close += 1,
            Some(Err(why)) => report.failed.push((path.to_owned(), why)),
        }
    }
    report.not_in_kafra = grf.paths().filter(|path| path.starts_with(prefix.as_str()) && !kafra.contains(path)).map(str::to_owned).collect();
    report.not_in_kafra.sort_unstable();
    report
}

/// Compares a rebuilt file with its original as far as `transform` keeps it.
pub fn compare(path: &str, transform: u8, original: &[u8], rebuilt: &[u8]) -> Result<Match, String> {
    if original == rebuilt {
        return Ok(Match::Exact);
    }
    match transform {
        RAW => Err("its bytes differ".into()),
        ALTITUDE => altitude(original, rebuilt).map(|()| Match::Close),
        GROUND => ground(original, rebuilt).map(|()| Match::Close),
        IMAGE => picture(path, original, rebuilt).map(|()| Match::Close),
        SOUND => sound(original, rebuilt).map(|()| Match::Close),
        _ => Err(format!("transform {transform}, which this checker doesn't know")),
    }
}

/// A picture as clients decode it: TGAs by their extension (they have no magic number), the rest by content.
pub fn decode_picture(path: &str, bytes: &[u8]) -> Option<DynamicImage> {
    if path.ends_with(".tga") { image::load_from_memory_with_format(bytes, ImageFormat::Tga) } else { image::load_from_memory(bytes) }.ok()
}

fn altitude(original: &[u8], rebuilt: &[u8]) -> Result<(), String> {
    if original.len() != rebuilt.len() || original.len() < 14 {
        return Err("its size differs".into());
    }
    if original[..14] != rebuilt[..14] {
        return Err("its header differs".into());
    }
    let width = u32::from_le_bytes(original[6..10].try_into().unwrap()) as usize;
    let height = u32::from_le_bytes(original[10..14].try_into().unwrap()) as usize;
    let end = 14 + width * height * 20;
    let (Some(before), Some(after)) = (original.get(14..end), rebuilt.get(14..end)) else { return Err("its cells are short".into()) };
    for (cell, (a, b)) in before.as_chunks::<20>().0.iter().zip(after.as_chunks::<20>().0).enumerate() {
        if a[16..] != b[16..] {
            return Err(format!("cell {cell}'s type differs"));
        }
        for corner in 0..4 {
            let height = |bytes: &[u8]| f32::from_le_bytes(bytes[corner * 4..corner * 4 + 4].try_into().unwrap());
            if (height(a) - height(b)).abs() > 0.0051 {
                return Err(format!("cell {cell}'s height is off by {}", (height(a) - height(b)).abs()));
            }
        }
    }
    if original[end..] != rebuilt[end..] { Err("what follows its cells differs".into()) } else { Ok(()) }
}

fn ground(original: &[u8], rebuilt: &[u8]) -> Result<(), String> {
    if original.len() != rebuilt.len() {
        return Err("its size differs".into());
    }
    let lightmaps = transform::lightmap_range(original).ok_or("it isn't a GND")?;
    if original[..lightmaps.start] != rebuilt[..lightmaps.start] || original[lightmaps.end..] != rebuilt[lightmaps.end..] {
        return Err("it differs outside its lightmaps".into());
    }
    Ok(())
}

fn picture(path: &str, original: &[u8], rebuilt: &[u8]) -> Result<(), String> {
    let before = decode_picture(path, original).ok_or("the original doesn't decode")?;
    let after = decode_picture(path, rebuilt).ok_or("it doesn't decode")?;
    if (before.width(), before.height()) != (after.width(), after.height()) {
        return Err("its size in pixels differs".into());
    }
    if before.color().has_alpha() != after.color().has_alpha() {
        return Err("its alpha differs".into());
    }
    if !before.color().has_alpha() {
        // The colour key: what `transform::key_alpha` takes for magenta comes back exactly #FF00FF, and nothing else does.
        let (before, after) = (before.to_rgb8(), after.to_rgb8());
        for (a, b) in before.pixels().zip(after.pixels()) {
            let keyed = a[0] >= 0xFE && a[1] <= 0x03 && a[2] >= 0xFE;
            if keyed != (b.0 == [0xFF, 0, 0xFF]) {
                return Err("its colour keys moved".into());
            }
        }
    }
    Ok(())
}

fn sound(original: &[u8], rebuilt: &[u8]) -> Result<(), String> {
    let after = wav_seconds(rebuilt).ok_or("it isn't a WAV")?;
    match wav_seconds(original) {
        Some(before) if (before - after).abs() > 0.1 => Err(format!("it lasts {after:.2} s, not {before:.2} s")),
        _ => Ok(()),
    }
}

/// How long a WAV lasts: its data's size over its byte rate.
fn wav_seconds(wav: &[u8]) -> Option<f64> {
    if wav.get(..4)? != b"RIFF" || wav.get(8..12)? != b"WAVE" {
        return None;
    }
    let (mut at, mut byte_rate, mut data) = (12, None, None);
    while let Some(header) = wav.get(at..at + 8) {
        let size = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
        match &header[..4] {
            b"fmt " => byte_rate = Some(u32::from_le_bytes(wav.get(at + 16..at + 20)?.try_into().unwrap())),
            b"data" => data = Some(size.min(wav.len().saturating_sub(at + 8))),
            _ => {}
        }
        at += 8 + size + size % 2;
    }
    let byte_rate = byte_rate.filter(|&rate| rate > 0)?;
    Some(data? as f64 / f64::from(byte_rate))
}
