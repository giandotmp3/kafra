//! Makes the sample the tests read: `sample.grf`, of files made here (no game data), and `sample.kafra` made from
//! it, one file or more of every transform. Needs avifenc and ffmpeg on the PATH.
//! `cargo run --release --example make_sample [-- <folder>]` (tests/fixtures by default)

use std::path::Path;

use image::{DynamicImage, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};
use kafra::{Kafra, Transform};

const GROUND_SIZE: usize = 4;
const LIGHTMAP_TEXELS: usize = 8;

fn main() {
    let folder = std::env::args().nth(1).unwrap_or_else(|| "tests/fixtures".into());
    let folder = Path::new(&folder);
    std::fs::create_dir_all(folder).unwrap();

    let readme = b"KAFRA sample: every file here was made by examples/make_sample.rs.\n".repeat(20);
    let ground = ground();
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("data\\readme.txt", readme.clone()),
        // The same data as readme.txt: stored once.
        ("data\\copy\\readme.txt", readme),
        ("data\\유저인터페이스\\note.txt", "이름 (a Korean path)\n".repeat(10).into_bytes()),
        ("data\\sprite\\noise.bin", noise(4096)),
        ("data\\texture\\sample\\keyed.bmp", picture(keyed(), ImageFormat::Bmp)),
        ("data\\texture\\sample\\alpha.tga", picture(alpha(), ImageFormat::Tga)),
        ("data\\texture\\sample\\plain.png", picture(plain(), ImageFormat::Png)),
        ("data\\wav\\sample.wav", wav()),
        ("data\\sample.gat", altitude()),
        ("data\\sample.gnd", ground),
    ];
    let listed: Vec<(&str, &[u8])> = files.iter().map(|(path, data)| (*path, data.as_slice())).collect();
    let grf_path = folder.join("sample.grf");
    std::fs::write(&grf_path, kafra::grf::build(0x200, &listed)).unwrap();

    let kafra_path = folder.join("sample.kafra");
    let grf = kafra::grf::Grf::open(&grf_path).unwrap();
    let summary = kafra::store::store(&grf, &kafra_path, &kafra::store::Options::default()).unwrap();

    // Every transform made it in: a file that didn't come back right would have gone in raw.
    let kafra = Kafra::open(&kafra_path).unwrap();
    for (path, transform) in [
        ("data\\texture\\sample\\keyed.bmp", Transform::Image),
        ("data\\texture\\sample\\alpha.tga", Transform::Image),
        ("data\\texture\\sample\\plain.png", Transform::Image),
        ("data\\wav\\sample.wav", Transform::Sound),
        ("data\\sample.gnd", Transform::Ground),
        ("data\\sample.gat", Transform::Altitude),
    ] {
        assert_eq!(kafra.entry(path).unwrap().transform, transform, "{path}");
    }
    eprintln!("{}: {} files ({} distinct), {} bytes", kafra_path.display(), summary.files, summary.distinct, summary.size);
}

fn picture(image: DynamicImage, format: ImageFormat) -> Vec<u8> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, format).unwrap();
    bytes.into_inner()
}

/// Bytes that don't compress, from a fixed seed.
fn noise(length: usize) -> Vec<u8> {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

/// A gradient on a magenta background: the colour key.
fn keyed() -> DynamicImage {
    DynamicImage::ImageRgb8(RgbImage::from_fn(96, 96, |x, y| {
        let inside = (16..80).contains(&x) && (16..80).contains(&y) && (x + y) % 40 > 4;
        if inside { Rgb([(x * 2) as u8 + 40, (y * 2) as u8 + 40, 160]) } else { Rgb([255, 0, 255]) }
    }))
}

/// A disc fading out to the edges.
fn alpha() -> DynamicImage {
    DynamicImage::ImageRgba8(RgbaImage::from_fn(96, 96, |x, y| {
        let distance = ((x as f32 - 48.0).powi(2) + (y as f32 - 48.0).powi(2)).sqrt();
        Rgba([200, (x * 2) as u8, (y * 2) as u8, (255.0 - distance * 5.0).clamp(0.0, 255.0) as u8])
    }))
}

fn plain() -> DynamicImage {
    DynamicImage::ImageRgb8(RgbImage::from_fn(96, 96, |x, y| Rgb([(x * 2) as u8, (y * 2) as u8, ((x + y) % 256) as u8])))
}

/// One second of a falling tone, 22,050 Hz mono, 16-bit.
fn wav() -> Vec<u8> {
    const RATE: u32 = 22_050;
    let samples: Vec<i16> = (0..RATE)
        .map(|index| {
            let time = index as f32 / RATE as f32;
            let tone = (std::f32::consts::TAU * (660.0 - 220.0 * time) * time).sin();
            (tone * (1.0 - time) * 12_000.0) as i16
        })
        .collect();
    let data = (samples.len() * 2) as u32;
    let mut wav = b"RIFF".to_vec();
    wav.extend((36 + data).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(RATE.to_le_bytes());
    wav.extend((RATE * 2).to_le_bytes());
    wav.extend(2u16.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(data.to_le_bytes());
    wav.extend(samples.iter().flat_map(|sample| sample.to_le_bytes()));
    wav
}

/// A cube's corner height on the sample map's slope.
fn slope(x: usize, y: usize) -> f32 {
    -(x as f32 * 3.0 + y as f32 * 2.0)
}

/// A GND 1.7 of 4 by 4 cubes, each with a top surface and a lightmap of its own, and two lightmaps no surface uses.
fn ground() -> Vec<u8> {
    let cubes = GROUND_SIZE * GROUND_SIZE;
    let lightmaps = cubes + 2;
    let mut gnd = b"GRGN".to_vec();
    gnd.extend([1, 7]);
    gnd.extend((GROUND_SIZE as i32).to_le_bytes());
    gnd.extend((GROUND_SIZE as i32).to_le_bytes());
    gnd.extend(10.0f32.to_le_bytes());
    gnd.extend(1i32.to_le_bytes());
    gnd.extend(80i32.to_le_bytes());
    let mut name = b"sample\\ground.bmp".to_vec();
    name.resize(80, 0);
    gnd.extend(name);
    for value in [lightmaps as i32, LIGHTMAP_TEXELS as i32, LIGHTMAP_TEXELS as i32, 1] {
        gnd.extend(value.to_le_bytes());
    }
    let texels = LIGHTMAP_TEXELS * LIGHTMAP_TEXELS;
    for lightmap in 0..lightmaps {
        gnd.extend((0..texels).map(|texel| (128 + lightmap * 4 + texel % LIGHTMAP_TEXELS * 8) as u8));
        gnd.extend((0..texels).flat_map(|texel| [(lightmap * 12) as u8, (texel * 3) as u8, 90]));
    }
    gnd.extend((cubes as i32).to_le_bytes());
    for surface in 0..cubes {
        for value in [0.0f32, 1.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0] {
            gnd.extend(value.to_le_bytes());
        }
        gnd.extend(0u16.to_le_bytes());
        gnd.extend((surface as u16).to_le_bytes());
        gnd.extend([255, 255, 255, 255]);
    }
    for index in 0..cubes {
        let (x, y) = (index % GROUND_SIZE, index / GROUND_SIZE);
        for height in [slope(x, y), slope(x + 1, y), slope(x, y + 1), slope(x + 1, y + 1)] {
            gnd.extend(height.to_le_bytes());
        }
        for side in [index as i32, -1, -1] {
            gnd.extend(side.to_le_bytes());
        }
    }
    gnd
}

/// The GAT for `ground`: two cells a side to a cube, near its slope, of a few types, one with the water flag.
fn altitude() -> Vec<u8> {
    let size = GROUND_SIZE * 2;
    let mut gat = b"GRAT".to_vec();
    gat.extend([1, 2]);
    gat.extend((size as i32).to_le_bytes());
    gat.extend((size as i32).to_le_bytes());
    for index in 0..size * size {
        let (x, y) = (index % size, index / size);
        for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let height = slope(x + dx, y + dy) / 2.0 + ((index * 7 + dx * 3) % 5) as f32 * 0.25;
            gat.extend(height.to_le_bytes());
        }
        let kind = if index == 9 { 0x8000_0001u32 } else { (index % 3) as u32 };
        gat.extend(kind.to_le_bytes());
    }
    gat
}
