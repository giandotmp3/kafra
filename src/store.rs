//! Storing a GRF into a `.kafra`: every file (but what `Options` leaves out), maps' textures, illustrations and cards
//! as AVIF at quality 40, effects, minimaps, collection pictures and large UI pictures at 60, sounds as 32 kbit/s mono
//! Opus, grounds and altitudes as columns, all in zstd chunks but the AVIF and Opus files. Identical files are encoded
//! and stored once.

use std::cmp::Reverse;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::io;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use image::{DynamicImage, ImageFormat};

use crate::format::normalize;
use crate::grf::Grf;
use crate::transform as kafra;
use crate::verify;
use crate::writer::Writer;

const UI: &str = "data\\texture\\유저인터페이스\\";
/// A zstd chunk's size before compression: a file read decompresses its whole chunk.
const CHUNK: usize = 4 << 20;
const ZSTD_LEVEL: i32 = 19;
const LIGHTMAP_QUALITY: u8 = 60;
const LIGHTMAP_YUV: &str = "444";

/// A file as it goes into the `.kafra`.
#[derive(Clone)]
struct Encoded {
    transform: u8,
    flags: u8,
    data: Vec<u8>,
    /// The file's size as read back.
    size: u32,
}

impl Encoded {
    fn raw(data: Vec<u8>) -> Self {
        Self { transform: kafra::RAW, flags: 0, size: data.len() as u32, data }
    }

    /// Already compressed, so stored as it is.
    fn stored(&self) -> bool {
        matches!(self.transform, kafra::IMAGE | kafra::SOUND)
    }
}

/// What to store.
#[derive(Clone, Debug)]
pub struct Options {
    /// How many files are encoded at once.
    pub jobs: usize,
    /// Only the files under these path prefixes, if any.
    pub only: Vec<String>,
    /// Not the files under these path prefixes, or (`.bik`) with these extensions.
    pub exclude: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self { jobs: std::thread::available_parallelism().map_or(4, usize::from), only: Vec::new(), exclude: Vec::new() }
    }
}

/// What `store` wrote.
#[derive(Clone, Copy, Debug)]
pub struct Summary {
    /// How many files it holds.
    pub files: usize,
    /// How many of them have data of their own; the rest share another file's.
    pub distinct: usize,
    /// The `.kafra`'s size in bytes.
    pub size: u64,
}

/// Stores `grf`'s files into a `.kafra` at `out`, reading every one back and checking it (`verify::compare`) before
/// it's written: one that doesn't come back right goes in as it was. Says how it goes on stderr. Needs avifenc and
/// ffmpeg on the PATH.
pub fn store(grf: &Grf, out: &Path, options: &Options) -> io::Result<Summary> {
    for tool in ["avifenc", "ffmpeg"] {
        if Command::new(tool).arg("-version").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_err() {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("storing needs {tool} on the PATH")));
        }
    }
    let only: Vec<String> = options.only.iter().map(|prefix| normalize(prefix)).collect();
    let exclude: Vec<String> = options.exclude.iter().map(|pattern| normalize(pattern)).collect();
    let jobs = options.jobs.max(1);
    let mut paths: Vec<String> = grf
        .paths()
        .filter(|path| !excluded(path, &exclude) && (only.is_empty() || only.iter().any(|prefix| path.starts_with(prefix.as_str()))))
        .map(str::to_owned)
        .collect();
    paths.sort_unstable();
    let temp = std::env::temp_dir().join(format!("kafra-{}", std::process::id()));
    std::fs::create_dir_all(&temp)?;

    // Grounds before altitudes, which are told against them.
    let (altitudes, others): (Vec<String>, Vec<String>) = paths.into_iter().partition(|path| path.ends_with(".gat"));
    let encodings = Encodings::default();
    let mut encoded = in_parallel(&others, jobs, |path, worker| store_file(grf, path, &temp.join(worker.to_string()), &encodings));
    drop(encodings);
    let grounds: HashMap<&str, &[u8]> = others
        .iter()
        .zip(&encoded)
        .filter_map(|(path, encoded)| Some((path.as_str(), encoded.as_ref().filter(|encoded| encoded.transform == kafra::GROUND)?.data.as_slice())))
        .collect();
    let stored_altitudes = in_parallel(&altitudes, jobs, |path, _| store_altitude(grf, path, &grounds));
    drop(grounds);
    encoded.extend(stored_altitudes);
    std::fs::remove_dir_all(&temp).ok();
    // Files the GRF can't give back stay out.
    let (paths, encoded): (Vec<String>, Vec<Encoded>) =
        others.into_iter().chain(altitudes).zip(encoded).filter_map(|(path, encoded)| Some((path, encoded?))).unzip();

    write(out, &paths, encoded, jobs)
}

/// Whether `--exclude` leaves a file out: one under a path prefix, or one with an extension (`.bik`).
fn excluded(path: &str, exclude: &[String]) -> bool {
    exclude.iter().any(|pattern| if pattern.starts_with('.') { path.ends_with(pattern.as_str()) } else { path.starts_with(pattern.as_str()) })
}

/// A path's extension, or "" without one.
fn extension(path: &str) -> &str {
    path.rsplit_once('.').map_or("", |(_, extension)| extension)
}

/// `work` on every item on `jobs` threads (numbered for their temporary files), the results in the items' order.
pub(crate) fn in_parallel<I: Sync, T: Send>(items: &[I], jobs: usize, work: impl Fn(&I, usize) -> T + Sync) -> Vec<T> {
    let next = AtomicUsize::new(0);
    let done = Mutex::new(Vec::with_capacity(items.len()));
    std::thread::scope(|scope| {
        for worker in 0..jobs {
            let (next, done, work) = (&next, &done, &work);
            scope.spawn(move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(index) else { break };
                    let result = work(item, worker);
                    let mut done = done.lock().unwrap();
                    done.push((index, result));
                    if done.len() % 5000 == 0 {
                        eprintln!("{} / {}", done.len(), items.len());
                    }
                }
            });
        }
    });
    let mut done = done.into_inner().unwrap();
    done.sort_unstable_by_key(|(index, _)| *index);
    done.into_iter().map(|(_, result)| result).collect()
}

/// The encodings made so far, by the original's content and what its encoding takes from its path, so that a file
/// under several paths (many pictures are) is encoded once. None: it's kept as it was.
type Encodings = Mutex<HashMap<(u64, u64, String, Option<u8>, Option<u8>, bool), Arc<OnceLock<Option<Encoded>>>>>;

fn store_file(grf: &Grf, path: &str, temp: &Path, encodings: &Encodings) -> Option<Encoded> {
    let bytes = read(grf, path)?;
    if !matches!(extension(path), "bmp" | "tga" | "jpg" | "jpeg" | "png" | "wav" | "gnd") {
        return Some(Encoded::raw(bytes));
    }
    // A picture's encoding takes its format from the extension, its quality (by its size in pixels) and its keying
    // from the path; a sound's and a ground's take nothing from it.
    let key = (digest(&bytes, 1), digest(&bytes, 2), extension(path).to_owned(), quality(path, 0), quality(path, u32::MAX), effect(path));
    let encoding = encodings.lock().unwrap().entry(key).or_default().clone();
    let encoded = encoding.get_or_init(|| encode(path, &bytes, temp)).clone();
    Some(encoded.unwrap_or_else(|| Encoded::raw(bytes)))
}

/// A picture, sound or ground encoded and checked, or None to keep it as it was.
fn encode(path: &str, bytes: &[u8], temp: &Path) -> Option<Encoded> {
    let mut encoded = match extension(path) {
        "wav" => sound(bytes, temp),
        "gnd" => ground(bytes, temp),
        _ => picture(path, bytes, temp),
    }?;
    match check(path, bytes, &mut encoded, None) {
        Ok(()) => Some(encoded),
        Err(error) => {
            eprintln!("{path}: {error}; kept as it was");
            None
        }
    }
}

fn read(grf: &Grf, path: &str) -> Option<Vec<u8>> {
    grf.read(path).map_err(|error| eprintln!("{path}: {error}; left out")).ok()
}

fn store_altitude(grf: &Grf, path: &str, grounds: &HashMap<&str, &[u8]>) -> Option<Encoded> {
    let bytes = read(grf, path)?;
    let ground_path = path.replace(".gat", ".gnd");
    let Some(ground) = grounds.get(ground_path.as_str()) else { return Some(Encoded::raw(bytes)) };
    let Some(data) = kafra::altitude(&bytes, &ground_path, ground) else { return Some(Encoded::raw(bytes)) };
    let mut encoded = Encoded { transform: kafra::ALTITUDE, flags: 0, data, size: bytes.len() as u32 };
    Some(match check(path, &bytes, &mut encoded, Some(ground)) {
        Ok(()) => encoded,
        Err(error) => {
            eprintln!("{path}: {error}; kept as it was");
            Encoded::raw(bytes)
        }
    })
}

/// Reads an encoded file back as a reader will, and checks it against the original as far as the transform keeps it.
/// A picture's or sound's size is set to what it rebuilds to; a ground's or altitude's must be the original's.
fn check(path: &str, original: &[u8], encoded: &mut Encoded, ground: Option<&[u8]>) -> Result<(), String> {
    let rebuilt = kafra::rebuild(encoded.transform, encoded.flags, &encoded.data, ground).map_err(|error| error.to_string())?;
    if matches!(encoded.transform, kafra::IMAGE | kafra::SOUND) {
        encoded.size = rebuilt.len() as u32;
    } else if rebuilt.len() as u32 != encoded.size {
        return Err("the size differs read back".into());
    }
    verify::compare(path, encoded.transform, original, &rebuilt).map(|_| ())
}

/// The quality a picture is encoded at, or None to keep it as it is: buttons (which a client may redraw from their
/// bitmaps), item icons and small UI pictures stay.
fn quality(path: &str, pixels: u32) -> Option<u8> {
    if let Some(rest) = path.strip_prefix(UI) {
        let name = rest.rsplit('\\').next().unwrap();
        return if name.starts_with("btn_") || name.starts_with("esc_") || rest.starts_with("item\\") {
            None
        } else if rest.starts_with("illust\\") || rest.starts_with("cardbmp\\") {
            Some(40)
        } else if rest.starts_with("map\\") || rest.starts_with("collection\\") || pixels >= 65536 {
            Some(60)
        } else {
            None
        };
    }
    if effect(path) {
        return Some(60);
    }
    path.starts_with("data\\texture\\").then_some(40)
}

/// Effects' pictures: drawn on black, which the client keys out of the ones without alpha.
fn effect(path: &str) -> bool {
    path.starts_with("data\\texture\\effect\\") || path.starts_with("data\\sprite\\이팩트\\")
}

fn picture(path: &str, bytes: &[u8], temp: &Path) -> Option<Encoded> {
    let image = verify::decode_picture(path, bytes)?;
    let quality = quality(path, image.width() * image.height())?;
    let format = match extension(path) {
        "tga" => kafra::TGA,
        "png" => kafra::PNG,
        "jpg" | "jpeg" => kafra::JPG,
        _ => kafra::BMP,
    };
    let (mut rgba, alpha) = (image.to_rgba8(), image.color().has_alpha());
    let flags = if alpha {
        format | kafra::ALPHA
    } else if kafra::key_alpha(&mut rgba, effect(path)) {
        format | kafra::KEYED
    } else {
        format
    };
    // Colour keys are kept exactly: their alpha lossless. A picture without either has no alpha plane.
    let alpha_quality = if alpha { quality } else { 100 };
    let input = if flags & (kafra::ALPHA | kafra::KEYED) != 0 {
        DynamicImage::ImageRgba8(rgba)
    } else {
        DynamicImage::ImageRgb8(DynamicImage::ImageRgba8(rgba).into_rgb8())
    };
    let data = avif(&input, quality, alpha_quality, "420", temp)?;
    // Small pictures don't gain from it. The size is the rebuilt one, which `check` sets.
    (data.len() < bytes.len() / 2).then_some(Encoded { transform: kafra::IMAGE, flags, data, size: 0 })
}

fn avif(image: &DynamicImage, quality: u8, alpha_quality: u8, yuv: &str, temp: &Path) -> Option<Vec<u8>> {
    std::fs::create_dir_all(temp).ok()?;
    let (input, output) = (temp.join("in.png"), temp.join("out.avif"));
    image.save_with_format(&input, ImageFormat::Png).ok()?;
    // Without CDEF or the loop filter: the same size and quality on these pictures (they lose under 0.1 dB), and they
    // decode in half the time.
    let status = Command::new("avifenc")
        .args(["-q", &quality.to_string(), "--qalpha", &alpha_quality.to_string(), "-y", yuv, "-s", "6", "-j", "1"])
        .args(["-a", "enable-cdef=0", "-a", "loopfilter-control=0"])
        .arg(&input)
        .arg(&output)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("avifenc");
    status.success().then(|| std::fs::read(&output).ok()).flatten()
}

fn sound(bytes: &[u8], temp: &Path) -> Option<Encoded> {
    std::fs::create_dir_all(temp).ok()?;
    let (input, output) = (temp.join("in.wav"), temp.join("out.opus"));
    std::fs::write(&input, bytes).ok()?;
    let status = Command::new("ffmpeg")
        .args(["-nostdin", "-loglevel", "error", "-y", "-i"])
        .arg(&input)
        .args(["-ac", "1", "-c:a", "libopus", "-b:a", "32k", "-vbr", "on"])
        .arg(&output)
        .status()
        .expect("ffmpeg");
    let data = status.success().then(|| std::fs::read(&output).ok()).flatten()?;
    // The size is the rebuilt one, which `check` sets.
    (data.len() < bytes.len()).then_some(Encoded { transform: kafra::SOUND, flags: 0, data, size: 0 })
}

/// A GND as columns and its lightmaps as an AVIF at quality 60: shadows in the alpha, at the same quality.
fn ground(bytes: &[u8], temp: &Path) -> Option<Encoded> {
    let ground = kafra::ground(bytes)?;
    let atlas = match ground.atlas {
        Some(atlas) => avif(&DynamicImage::ImageRgba8(atlas), LIGHTMAP_QUALITY, LIGHTMAP_QUALITY, LIGHTMAP_YUV, temp)?,
        None => Vec::new(),
    };
    Some(Encoded { transform: kafra::GROUND, flags: 0, data: kafra::ground_entry(&ground.columns, &atlas), size: bytes.len() as u32 })
}

/// Writes the `.kafra`: each distinct data once, the stored ones in chunks of their own, the rest in zstd chunks by kind
/// (extension), then path.
fn write(out: &Path, paths: &[String], encoded: Vec<Encoded>, jobs: usize) -> io::Result<Summary> {
    // The distinct data, by content.
    let mut distinct: Vec<usize> = Vec::new();
    let mut first: HashMap<(u64, u64), usize> = HashMap::new();
    let mut data_of = Vec::with_capacity(encoded.len());
    for (index, file) in encoded.iter().enumerate() {
        let key = (digest(&file.data, 1), digest(&file.data, 2));
        let found = *first.entry(key).or_insert_with(|| {
            distinct.push(index);
            index
        });
        data_of.push(found);
    }
    let distinct_count = distinct.len();
    eprintln!("{} files, {distinct_count} distinct", encoded.len());

    let kind = |index: usize| extension(&paths[index]).to_owned();
    distinct.sort_by_key(|&index| (encoded[index].stored(), kind(index), paths[index].clone()));
    // Chunks: (stored, the data indices in it).
    let mut chunks: Vec<(bool, Vec<usize>)> = Vec::new();
    let mut filled = 0;
    for index in distinct {
        let stored = encoded[index].stored();
        match chunks.last_mut() {
            Some((last_stored, members)) if *last_stored == stored && filled + encoded[index].data.len() <= CHUNK => members.push(index),
            _ => {
                chunks.push((stored, vec![index]));
                filled = 0;
            }
        }
        filled += encoded[index].data.len();
    }
    let joined: Vec<Vec<u8>> = chunks.iter().map(|(_, members)| members.iter().flat_map(|&index| encoded[index].data.iter().copied()).collect()).collect();
    // The biggest first, so that no big chunk is left compressing alone at the end.
    let mut to_compress: Vec<usize> = (0..chunks.len()).filter(|&chunk| !chunks[chunk].0).collect();
    to_compress.sort_by_key(|&chunk| Reverse(joined[chunk].len()));
    let mut compressed: Vec<Option<Vec<u8>>> = vec![None; chunks.len()];
    for (chunk, data) in to_compress.iter().zip(in_parallel(&to_compress, jobs, |&chunk, _| zstd::bulk::compress(&joined[chunk], ZSTD_LEVEL))) {
        compressed[*chunk] = Some(data?);
    }

    let mut writer = Writer::create(out)?;
    let mut placed: HashMap<usize, (u32, u32)> = HashMap::new();
    // What each kind of file weighs in the `.kafra`: its share of its chunks, the distinct data counted once.
    let mut weights: BTreeMap<String, (u64, f64)> = BTreeMap::new();
    for ((_, members), (data, compressed)) in chunks.iter().zip(joined.iter().zip(&compressed)) {
        let chunk = writer.chunk(data, compressed.as_deref())?;
        let ratio = compressed.as_ref().map_or(1.0, |compressed| compressed.len() as f64 / data.len() as f64);
        let mut offset = 0;
        for &index in members {
            placed.insert(index, (chunk, offset));
            offset += encoded[index].data.len() as u32;
            let transform = ["raw", "image", "sound", "ground", "altitude"][encoded[index].transform as usize];
            let weight = weights.entry(format!("{transform} {}", kind(index))).or_default();
            weight.0 += 1;
            weight.1 += encoded[index].data.len() as f64 * ratio;
        }
    }
    for (index, file) in encoded.iter().enumerate() {
        let (chunk, offset) = placed[&data_of[index]];
        writer.entry(&paths[index], chunk, offset, file.data.len() as u32, file.transform, file.flags, file.size);
    }
    let mut weights: Vec<_> = weights.into_iter().filter(|(_, (_, bytes))| *bytes >= 1e6).collect();
    weights.sort_by(|a, b| b.1.1.total_cmp(&a.1.1));
    for (kind, (files, bytes)) in weights {
        eprintln!("{kind:>16}: {files:>7} distinct files, {:>7.1} MB", bytes / 1e6);
    }
    let size = writer.finish(|index| zstd::bulk::compress(index, ZSTD_LEVEL))?;
    Ok(Summary { files: paths.len(), distinct: distinct_count, size })
}

fn digest(bytes: &[u8], seed: u64) -> u64 {
    let mut hasher = DefaultHasher::new();
    seed.hash(&mut hasher);
    bytes.hash(&mut hasher);
    hasher.finish()
}
