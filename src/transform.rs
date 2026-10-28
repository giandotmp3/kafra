//! The transforms: how `store` re-encodes a file into an entry's data, and how a reader gives the entry back as
//! the file. Pictures are AVIF, their colour keys kept exact in a lossless alpha plane; sounds are Opus; a map's
//! ground is its geometry in columns plus its lightmaps as one AVIF laid out like the map; its altitudes are the cell
//! types and what the ground's heights don't predict. FORMAT.md has each one's layout.

// Grounds are split into columns (`store`) and rebuilt from them (`rebuild`) with the same parts: with only the
// rebuilding half, some go unused.
#![cfg_attr(not(feature = "store"), allow(dead_code))]

#[cfg(feature = "store")]
use std::collections::HashMap;

#[cfg(feature = "store")]
use image::RgbaImage;
#[cfg(feature = "rebuild")]
use image::{ExtendedColorType, ImageEncoder};

use crate::error::{Error, Result, corrupt};
use crate::format::{Cursor, le32};

/// How an entry's data becomes the file.
pub const RAW: u8 = 0;
/// An AVIF; the flags say what format to give it back in and how its alpha is used.
pub const IMAGE: u8 = 1;
/// An Ogg Opus stream, given back as a 16-bit WAV.
pub const SOUND: u8 = 2;
/// A GND: `ground`'s columns and lightmap atlas.
pub const GROUND: u8 = 3;
/// A GAT: `altitude`'s cell types and height errors against its GND.
pub const ALTITUDE: u8 = 4;

/// An image's source format, in the flags' low bits.
pub const BMP: u8 = 0;
/// A TGA source.
pub const TGA: u8 = 1;
/// A JPG source, given back as a BMP.
pub const JPG: u8 = 2;
/// A PNG source.
pub const PNG: u8 = 3;
/// Its colour keys are in the alpha: under 64 magenta (#FF00FF), under 192 black.
pub const KEYED: u8 = 4;
/// The source had an alpha channel, which the AVIF's alpha is.
pub const ALPHA: u8 = 8;

/// A file from an entry's data, its transform and flags; an `ALTITUDE` also needs its GND's `GROUND` entry.
/// Without the `rebuild` feature, only `RAW` and `ALTITUDE` entries rebuild: the rest need the AVIF and Opus decoders.
#[cfg_attr(not(feature = "rebuild"), allow(unused_variables))]
pub fn rebuild(transform: u8, flags: u8, data: &[u8], ground: Option<&[u8]>) -> Result<Vec<u8>> {
    match transform {
        RAW => Ok(data.to_vec()),
        ALTITUDE => rebuild_altitude(data, ground.ok_or_else(|| corrupt("an altitude without its ground"))?),
        #[cfg(feature = "rebuild")]
        IMAGE => image(data, flags),
        #[cfg(feature = "rebuild")]
        SOUND => sound(data),
        #[cfg(feature = "rebuild")]
        GROUND => rebuild_ground(data),
        #[cfg(not(feature = "rebuild"))]
        IMAGE | SOUND | GROUND => Err(Error::Unsupported("this transform needs the `rebuild` feature")),
        _ => Err(Error::UnknownTransform(transform)),
    }
}

// Pictures ------------------------------------------------------------------------------------------------------------

/// Puts a BMP's or JPG's colour keys into its alpha for `IMAGE` (0 for magenta; 128 for black, the near black the
/// client keys effect pictures by, if `black`), with black under them so they don't bleed into their neighbours.
/// Returns whether it has any.
#[cfg(feature = "store")]
pub fn key_alpha(image: &mut RgbaImage, black: bool) -> bool {
    let mut keyed = false;
    for pixel in image.pixels_mut() {
        let [r, g, b, _] = pixel.0;
        let alpha = if r >= 0xFE && g <= 0x03 && b >= 0xFE {
            0
        } else if black && r < 8 && g < 8 && b < 8 {
            128
        } else {
            255
        };
        keyed |= alpha < 255;
        pixel.0 = if alpha < 255 { [0, 0, 0, alpha] } else { [r, g, b, 255] };
    }
    keyed
}

/// An `IMAGE` entry back in its source's format: a BMP (also for a JPG, which the loaders read by its content), a
/// TGA or a PNG.
#[cfg(feature = "rebuild")]
fn image(avif: &[u8], flags: u8) -> Result<Vec<u8>> {
    let (width, height, mut pixels) = decode_avif(avif)?;
    let alpha = flags & ALPHA != 0;
    if !alpha {
        let keyed = flags & KEYED != 0;
        for pixel in pixels.chunks_exact_mut(4) {
            if keyed && pixel[3] < 64 {
                pixel[..3].copy_from_slice(&[0xFF, 0, 0xFF]);
            } else if keyed && pixel[3] < 192 {
                pixel[..3].copy_from_slice(&[0, 0, 0]);
            }
            pixel[3] = 255;
        }
    }
    let (bytes, color) = if alpha {
        (pixels, ExtendedColorType::Rgba8)
    } else {
        (pixels.chunks_exact(4).flat_map(|pixel| [pixel[0], pixel[1], pixel[2]]).collect(), ExtendedColorType::Rgb8)
    };
    let mut out = Vec::new();
    let result = match flags & 3 {
        TGA => image::codecs::tga::TgaEncoder::new(&mut out).write_image(&bytes, width, height, color),
        PNG => image::codecs::png::PngEncoder::new_with_quality(&mut out, image::codecs::png::CompressionType::Fast, Default::default())
            .write_image(&bytes, width, height, color),
        _ => image::codecs::bmp::BmpEncoder::new(&mut out).write_image(&bytes, width, height, color),
    };
    result.map_err(|error| Error::Decode(format!("re-encoding the picture: {error}")))?;
    Ok(out)
}

/// An AVIF as RGBA: (width, height, pixels). On one thread: rav1d's own threads stall on some pictures, and the
/// pictures here are small enough not to need them.
#[cfg(feature = "rebuild")]
fn decode_avif(avif: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    use zenpixels_convert::ext::PixelBufferConvertTypedExt;
    let config = zenavif::DecoderConfig::new().threads(1);
    let decoded = zenavif::decode_with(avif, &config, &zenavif::Unstoppable).map_err(|error| Error::Decode(format!("AVIF: {error}")))?;
    let decoded = decoded.to_rgba8();
    Ok((decoded.width(), decoded.height(), decoded.copy_to_contiguous_bytes()))
}

// Sounds --------------------------------------------------------------------------------------------------------------

/// A `SOUND` entry as a 16-bit PCM WAV at 48 kHz.
#[cfg(feature = "rebuild")]
fn sound(ogg: &[u8]) -> Result<Vec<u8>> {
    const RATE: i32 = 48_000;
    let opus = |error: opus_pure::Error| Error::Decode(format!("Opus: {error}"));
    let mut reader = opus_pure::OggOpusReader::new(ogg).map_err(opus)?;
    let channels = usize::from(reader.head().channel_count);
    let mut decoder = reader.head().decoder(RATE).map_err(opus)?;
    let mut trim = opus_pure::Trim::new(reader.head(), RATE, channels).map_err(opus)?;
    let mut block = vec![0.0f32; opus_pure::MAX_PACKET_SAMPLES * channels];
    let mut samples = Vec::new();
    for packet in reader.packets() {
        let packet = packet.map_err(opus)?;
        let frames = decoder.decode(&packet.data, opus_pure::MAX_PACKET_SAMPLES, &mut block).map_err(opus)?;
        samples.extend(trim.keep(&packet, &block[..frames * channels]).iter().map(|sample| (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16));
    }
    let data = (samples.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + data as usize);
    wav.extend(b"RIFF");
    wav.extend((36 + data).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend((channels as u16).to_le_bytes());
    wav.extend((RATE as u32).to_le_bytes());
    wav.extend((RATE as u32 * channels as u32 * 2).to_le_bytes());
    wav.extend((channels as u16 * 2).to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(data.to_le_bytes());
    wav.extend(samples.iter().flat_map(|sample| sample.to_le_bytes()));
    Ok(wav)
}

// Byte planes ---------------------------------------------------------------------------------------------------------

/// Values `width` bytes wide, byte by byte: every value's first byte, then every second byte... which zstd does far
/// better on for numbers.
#[cfg(feature = "store")]
fn split(bytes: &[u8], width: usize) -> Vec<u8> {
    let count = bytes.len() / width;
    (0..width).flat_map(|byte| (0..count).map(move |value| bytes[value * width + byte])).collect()
}

fn join(planes: &[u8], width: usize) -> Vec<u8> {
    let count = planes.len() / width;
    (0..count).flat_map(|value| (0..width).map(move |byte| planes[byte * count + value])).collect()
}

// Grounds -------------------------------------------------------------------------------------------------------------

/// Where a GND's parts are.
struct GroundLayout {
    version: (u8, u8),
    width: usize,
    height: usize,
    /// Where the lightmap header is, and the lightmaps' count and texel size.
    lightmap_header: usize,
    lightmaps: usize,
    texels: [usize; 2],
    surfaces: usize,
    cubes: usize,
    rest: usize,
}

impl GroundLayout {
    #[cfg(feature = "store")]
    fn read(bytes: &[u8]) -> Option<Self> {
        if bytes.get(..4)? != b"GRGN" {
            return None;
        }
        let i32_at = |at: usize| bytes.get(at..at + 4).map(|value| i32::from_le_bytes(value.try_into().unwrap()));
        let version = (*bytes.get(4)?, *bytes.get(5)?);
        let (width, height) = (usize::try_from(i32_at(6)?).ok()?, usize::try_from(i32_at(10)?).ok()?);
        let (textures, name_length) = (usize::try_from(i32_at(18)?).ok()?, usize::try_from(i32_at(22)?).ok()?);
        let lightmap_header = 26 + textures * name_length;
        let lightmaps = usize::try_from(i32_at(lightmap_header)?).ok()?;
        let texels = [usize::try_from(i32_at(lightmap_header + 4)?).ok()?, usize::try_from(i32_at(lightmap_header + 8)?).ok()?];
        let surface_count = lightmap_header + 16 + lightmaps * texels[0] * texels[1] * 4;
        let surfaces = usize::try_from(i32_at(surface_count)?).ok()?;
        let cubes = surface_count + 4 + surfaces * 40;
        let rest = cubes + width * height * (16 + 3 * Self::index_width(version));
        (rest <= bytes.len()).then_some(Self { version, width, height, lightmap_header, lightmaps, texels, surfaces, cubes, rest })
    }

    /// GND 1.7 widened the cubes' surface indices to 32 bits.
    fn index_width(version: (u8, u8)) -> usize {
        if version >= (1, 7) { 4 } else { 2 }
    }
}

/// Where a GND's lightmaps are: what `GROUND` keeps lossily. None if it isn't a GND.
#[cfg(feature = "store")]
pub(crate) fn lightmap_range(gnd: &[u8]) -> Option<std::ops::Range<usize>> {
    let layout = GroundLayout::read(gnd)?;
    let start = layout.lightmap_header + 16;
    Some(start..start + layout.lightmaps * layout.texels[0] * layout.texels[1] * 4)
}

/// A GND for `GROUND`: its geometry in columns, and its lightmaps as one picture (colour in RGB, shadow in alpha)
/// laid out like the map, for the caller to encode. The entry is `ground_entry` of the two.
#[cfg(feature = "store")]
pub struct Ground {
    /// Everything but the lightmaps (FORMAT.md, "3: ground").
    pub columns: Vec<u8>,
    /// The lightmap atlas, or None if the GND has no lightmaps.
    pub atlas: Option<RgbaImage>,
}

/// The columns: the GND up to its lightmaps; the surface count, the table of the surfaces' distinct texture
/// coordinates and each surface's place in it, their textures, their lightmaps (less their own index, mostly 0) and
/// colours; the cubes' corner heights corner by corner and their surface indices as differences from the last; and
/// what follows the cubes. Numbers as byte planes.
#[cfg(feature = "store")]
pub fn ground(bytes: &[u8]) -> Option<Ground> {
    let layout = GroundLayout::read(bytes)?;
    let surfaces = &bytes[layout.cubes - layout.surfaces * 40..layout.cubes];
    let mut table: Vec<&[u8]> = Vec::new();
    let mut places: HashMap<&[u8], u32> = HashMap::new();
    let mut indices = Vec::with_capacity(layout.surfaces * 4);
    let (mut textures, mut lightmaps, mut colors) = (Vec::new(), Vec::new(), Vec::<u8>::new());
    for (index, surface) in surfaces.chunks_exact(40).enumerate() {
        let place = *places.entry(&surface[..32]).or_insert_with(|| {
            table.push(&surface[..32]);
            table.len() as u32 - 1
        });
        indices.extend(place.to_le_bytes());
        textures.extend(&surface[32..34]);
        let lightmap = i32::from(u16::from_le_bytes(surface[34..36].try_into().unwrap()));
        lightmaps.extend((lightmap - index as i32).to_le_bytes());
        colors.extend(&surface[36..40]);
    }
    let cubes = cubes(bytes, &layout);
    let mut columns = Vec::new();
    columns.extend((layout.lightmap_header as u32 + 16).to_le_bytes());
    columns.extend(&bytes[..layout.lightmap_header + 16]);
    columns.extend((layout.surfaces as u32).to_le_bytes());
    columns.extend((table.len() as u32).to_le_bytes());
    columns.extend(split(&table.concat(), 4));
    columns.extend(split(&indices, 4));
    columns.extend(split(&textures, 2));
    columns.extend(split(&lightmaps, 4));
    columns.extend(colors);
    let heights: Vec<u8> = (0..4).flat_map(|corner| cubes.iter().flat_map(move |cube| cube.heights[corner].to_le_bytes())).collect();
    columns.extend(split(&heights, 4));
    for side in 0..3 {
        let mut last = 0i32;
        let deltas: Vec<u8> = cubes
            .iter()
            .flat_map(|cube| {
                let delta = cube.sides[side].wrapping_sub(last);
                last = cube.sides[side];
                delta.to_le_bytes()
            })
            .collect();
        columns.extend(split(&deltas, 4));
    }
    columns.extend(&bytes[layout.rest..]);

    let atlas = (layout.lightmaps > 0).then(|| {
        let placed = placements(&layout, &cubes, &lightmap_ids(surfaces));
        let [lw, lh] = layout.texels;
        let mut atlas = RgbaImage::new((placed.tiles[0] * lw) as u32, (placed.tiles[1] * lh) as u32);
        let start = layout.lightmap_header + 16;
        for (lightmap, [x, y]) in placed.positions.iter().enumerate() {
            let tile = &bytes[start + lightmap * lw * lh * 4..start + (lightmap + 1) * lw * lh * 4];
            for texel in 0..lw * lh {
                let color = &tile[lw * lh + texel * 3..lw * lh + texel * 3 + 3];
                let pixel = [color[0], color[1], color[2], tile[texel]];
                atlas.put_pixel((x * lw + texel % lw) as u32, (y * lh + texel / lw) as u32, image::Rgba(pixel));
            }
        }
        atlas
    });
    Some(Ground { columns, atlas })
}

/// The `GROUND` entry: the columns' length, the columns, and the lightmap atlas's AVIF (empty without lightmaps).
pub fn ground_entry(columns: &[u8], atlas_avif: &[u8]) -> Vec<u8> {
    let mut entry = (columns.len() as u32).to_le_bytes().to_vec();
    entry.extend(columns);
    entry.extend(atlas_avif);
    entry
}

struct Cube {
    heights: [f32; 4],
    /// The top, north and east surfaces, -1 for none.
    sides: [i32; 3],
}

#[cfg(feature = "store")]
fn cubes(bytes: &[u8], layout: &GroundLayout) -> Vec<Cube> {
    let index = GroundLayout::index_width(layout.version);
    let size = 16 + 3 * index;
    bytes[layout.cubes..layout.rest]
        .chunks_exact(size)
        .map(|cube| {
            let float = |at: usize| f32::from_le_bytes(cube[at..at + 4].try_into().unwrap());
            let side = |at: usize| {
                if index == 4 {
                    i32::from_le_bytes(cube[at..at + 4].try_into().unwrap())
                } else {
                    i32::from(i16::from_le_bytes(cube[at..at + 2].try_into().unwrap()))
                }
            };
            Cube { heights: [float(0), float(4), float(8), float(12)], sides: [side(16), side(16 + index), side(16 + 2 * index)] }
        })
        .collect()
}

#[cfg(feature = "store")]
fn lightmap_ids(surfaces: &[u8]) -> Vec<u16> {
    surfaces.chunks_exact(40).map(|surface| u16::from_le_bytes(surface[34..36].try_into().unwrap())).collect()
}

/// Where each lightmap goes in the atlas, in tiles: on a map-sized grid each for the tops, north walls and east walls
/// (side by side), where the first surface using it is, so neighbouring texels sit together and the picture
/// compresses; the lightmaps no surface uses in a square to the right.
struct Placements {
    positions: Vec<[usize; 2]>,
    tiles: [usize; 2],
}

fn placements(layout: &GroundLayout, cubes: &[Cube], surface_lightmaps: &[u16]) -> Placements {
    let (width, height, count) = (layout.width, layout.height, layout.lightmaps);
    let mut positions: Vec<Option<[usize; 2]>> = vec![None; count];
    for side in 0..3 {
        for (index, cube) in cubes.iter().enumerate() {
            let Ok(surface) = usize::try_from(cube.sides[side]) else { continue };
            let Some(&lightmap) = surface_lightmaps.get(surface) else { continue };
            if let Some(slot @ None) = positions.get_mut(usize::from(lightmap)) {
                *slot = Some([side * width + index % width, index / width]);
            }
        }
    }
    let unused = positions.iter().filter(|position| position.is_none()).count();
    let square = (unused as f64).sqrt().ceil() as usize;
    let mut next = 0;
    let positions = positions
        .into_iter()
        .map(|position| {
            position.unwrap_or_else(|| {
                next += 1;
                [3 * width + (next - 1) % square, (next - 1) / square]
            })
        })
        .collect();
    Placements { positions, tiles: [3 * width + square, height.max(square)] }
}

/// A `GROUND` entry back as the GND.
#[cfg(feature = "rebuild")]
fn rebuild_ground(entry: &[u8]) -> Result<Vec<u8>> {
    let length = le32(entry.get(..4).ok_or_else(|| corrupt("a short ground"))?) as usize;
    let columns = entry.get(4..4 + length).ok_or_else(|| corrupt("a short ground"))?;
    ground_from_columns(columns, || decode_avif(&entry[4 + length..]))
}

/// A GND from its columns and its lightmap atlas as RGBA (width, height, pixels), which `atlas` decodes if it has
/// lightmaps.
fn ground_from_columns(columns: &[u8], atlas: impl FnOnce() -> Result<(u32, u32, Vec<u8>)>) -> Result<Vec<u8>> {
    let short = || corrupt("a short ground");
    let mut cursor = Cursor { bytes: columns, at: 0 };
    let head_length = cursor.u32()? as usize;
    let head = cursor.take(head_length)?;
    let layout_of_head = |count: usize| -> Result<GroundLayout> {
        // The head is the GND up to its lightmaps: enough for `GroundLayout` but its own end.
        let i32_at = |at: usize| head.get(at..at + 4).map(|value| i32::from_le_bytes(value.try_into().unwrap())).ok_or_else(short);
        let lightmap_header = head_length - 16;
        Ok(GroundLayout {
            version: (head[4], head[5]),
            width: i32_at(6)?.max(0) as usize,
            height: i32_at(10)?.max(0) as usize,
            lightmap_header,
            lightmaps: i32_at(lightmap_header)?.max(0) as usize,
            texels: [i32_at(lightmap_header + 4)?.max(0) as usize, i32_at(lightmap_header + 8)?.max(0) as usize],
            surfaces: count,
            cubes: 0,
            rest: 0,
        })
    };
    let surface_count = cursor.u32()? as usize;
    let layout = layout_of_head(surface_count)?;
    let table_length = cursor.u32()? as usize;
    let table = join(cursor.take(table_length * 32)?, 4);
    let indices = join(cursor.take(surface_count * 4)?, 4);
    let textures = join(cursor.take(surface_count * 2)?, 2);
    let lightmaps = join(cursor.take(surface_count * 4)?, 4);
    let colors = cursor.take(surface_count * 4)?;
    let cube_count = layout.width * layout.height;
    let heights = join(cursor.take(cube_count * 16)?, 4);
    let mut sides = [Vec::new(), Vec::new(), Vec::new()];
    for side in &mut sides {
        let deltas = join(cursor.take(cube_count * 4)?, 4);
        let mut last = 0i32;
        *side = deltas
            .chunks_exact(4)
            .map(|delta| {
                last = last.wrapping_add(i32::from_le_bytes(delta.try_into().unwrap()));
                last
            })
            .collect::<Vec<i32>>();
    }
    let rest = &columns[cursor.at..];

    let mut surfaces = Vec::with_capacity(surface_count * 40);
    let mut surface_lightmaps = Vec::with_capacity(surface_count);
    for index in 0..surface_count {
        let place = le32(&indices[index * 4..]) as usize;
        surfaces.extend(table.get(place * 32..place * 32 + 32).ok_or_else(|| corrupt("a surface past the table"))?);
        surfaces.extend(&textures[index * 2..index * 2 + 2]);
        let lightmap = (i32::from_le_bytes(lightmaps[index * 4..index * 4 + 4].try_into().unwrap()) + index as i32) as u16;
        surface_lightmaps.push(lightmap);
        surfaces.extend(lightmap.to_le_bytes());
        surfaces.extend(&colors[index * 4..index * 4 + 4]);
    }
    let cubes: Vec<Cube> = (0..cube_count)
        .map(|index| {
            let height = |corner: usize| f32::from_le_bytes(heights[(corner * cube_count + index) * 4..][..4].try_into().unwrap());
            Cube { heights: [height(0), height(1), height(2), height(3)], sides: [sides[0][index], sides[1][index], sides[2][index]] }
        })
        .collect();

    let mut gnd = head.to_vec();
    let [lw, lh] = layout.texels;
    if layout.lightmaps > 0 {
        let (width, height, pixels) = atlas()?;
        let stride = width as usize;
        let placed = placements(&layout, &cubes, &surface_lightmaps);
        if placed.tiles[0] * lw != stride || placed.tiles[1] * lh != height as usize {
            return Err(corrupt("a lightmap atlas of the wrong size"));
        }
        for [x, y] in placed.positions {
            let texel = |t: usize| ((y * lh + t / lw) * stride + x * lw + t % lw) * 4;
            gnd.extend((0..lw * lh).map(|t| pixels[texel(t) + 3]));
            gnd.extend((0..lw * lh).flat_map(|t| pixels[texel(t)..texel(t) + 3].to_vec()));
        }
    }
    gnd.extend((surface_count as u32).to_le_bytes());
    gnd.extend(surfaces);
    let wide = GroundLayout::index_width(layout.version) == 4;
    for cube in &cubes {
        gnd.extend(cube.heights.iter().flat_map(|height| height.to_le_bytes()));
        for side in cube.sides {
            if wide {
                gnd.extend(side.to_le_bytes());
            } else {
                gnd.extend((side as i16).to_le_bytes());
            }
        }
    }
    gnd.extend(rest);
    Ok(gnd)
}

/// The cubes' corner heights in a `GROUND` entry's columns, without decoding its lightmaps: (width, heights).
fn ground_heights(entry: &[u8]) -> Result<(usize, Vec<[f32; 4]>)> {
    let short = || corrupt("a short ground");
    let length = le32(entry.get(..4).ok_or_else(short)?) as usize;
    let mut cursor = Cursor { bytes: entry.get(4..4 + length).ok_or_else(short)?, at: 0 };
    let head_length = cursor.u32()? as usize;
    let head = cursor.take(head_length)?;
    let (width, height) = (le32(&head[6..]) as usize, le32(&head[10..]) as usize);
    let surfaces = cursor.u32()? as usize;
    let table = cursor.u32()? as usize;
    cursor.take(table * 32 + surfaces * (4 + 2 + 4 + 4))?;
    let count = width * height;
    let planes = join(cursor.take(count * 16)?, 4);
    let height_of = |corner: usize, index: usize| f32::from_le_bytes(planes[(corner * count + index) * 4..][..4].try_into().unwrap());
    Ok((width, (0..count).map(|index| [height_of(0, index), height_of(1, index), height_of(2, index), height_of(3, index)]).collect()))
}

// Altitudes -----------------------------------------------------------------------------------------------------------

/// A GAT cell's corner heights as its GND cube's, bilinearly (two cells a side to a cube): what `altitude` keeps is
/// only how far each one is off.
fn predicted(cubes: &[[f32; 4]], ground_width: usize, [x, y]: [usize; 2]) -> [f32; 4] {
    let cube = cubes.get((y / 2) * ground_width + x / 2).copied().unwrap_or([0.0; 4]);
    let at = |u: f32, v: f32| {
        let south = cube[0] + (cube[1] - cube[0]) * u;
        let north = cube[2] + (cube[3] - cube[2]) * u;
        south + (north - south) * v
    };
    let (u, v) = ((x % 2) as f32 * 0.5, (y % 2) as f32 * 0.5);
    [at(u, v), at(u + 0.5, v), at(u, v + 0.5), at(u + 0.5, v + 0.5)]
}

/// A GAT for `ALTITUDE`, given its GND's `GROUND` entry: the GND's path, the GAT's header, each cell's type and its
/// corners' errors against `predicted` in hundredths (as byte planes). None where that can't hold the GAT within
/// half a hundredth, or a type doesn't fit a byte.
#[cfg(feature = "store")]
pub fn altitude(gat: &[u8], ground_path: &str, ground_entry: &[u8]) -> Option<Vec<u8>> {
    if gat.get(..4)? != b"GRAT" {
        return None;
    }
    let (width, height) = (le32(gat.get(6..)?) as usize, le32(gat.get(10..)?) as usize);
    let cells = gat.get(14..14 + width * height * 20)?;
    let (ground_width, cubes) = ground_heights(ground_entry).ok()?;
    let mut out = (ground_path.len() as u16).to_le_bytes().to_vec();
    out.extend(ground_path.as_bytes());
    out.extend(&gat[..14]);
    let mut errors = Vec::with_capacity(width * height * 8);
    for (index, cell) in cells.chunks_exact(20).enumerate() {
        let kind = i32::from_le_bytes(cell[16..20].try_into().unwrap());
        out.push(type_byte(kind)?);
        let guess = predicted(&cubes, ground_width, [index % width, index / width]);
        for corner in 0..4 {
            let real = f32::from_le_bytes(cell[corner * 4..corner * 4 + 4].try_into().unwrap());
            let error = ((real - guess[corner]) * 100.0).round();
            let error = i16::try_from(error as i64).ok()?;
            if (guess[corner] + f32::from(error) / 100.0 - real).abs() > 0.0051 {
                return None;
            }
            errors.extend(error.to_le_bytes());
        }
    }
    out.extend(split(&errors, 2));
    out.extend(&gat[14 + width * height * 20..]);
    Some(out)
}

/// A cell type as a byte: its low 7 bits, and its top bit (newer maps' water flag) as the byte's. None for any
/// other bit.
#[cfg(feature = "store")]
fn type_byte(kind: i32) -> Option<u8> {
    let kind = kind as u32;
    (kind & 0x7FFF_FF80 == 0).then_some((kind & 0x7F) as u8 | ((kind >> 31) as u8) << 7)
}

/// An `ALTITUDE` entry's GND path, and where the rest starts.
pub(crate) fn altitude_ground(entry: &[u8]) -> Result<(&str, usize)> {
    let length = u16::from_le_bytes(entry.get(..2).ok_or_else(|| corrupt("a short altitude"))?.try_into().unwrap()) as usize;
    let path = entry.get(2..2 + length).and_then(|path| std::str::from_utf8(path).ok()).ok_or_else(|| corrupt("a short altitude"))?;
    Ok((path, 2 + length))
}

fn rebuild_altitude(entry: &[u8], ground_entry: &[u8]) -> Result<Vec<u8>> {
    let short = || corrupt("a short altitude");
    let (_, start) = altitude_ground(entry)?;
    let header = entry.get(start..start + 14).ok_or_else(short)?;
    let (width, height) = (le32(&header[6..]) as usize, le32(&header[10..]) as usize);
    let count = width * height;
    let types = entry.get(start + 14..start + 14 + count).ok_or_else(short)?;
    let errors = join(entry.get(start + 14 + count..start + 14 + count + count * 8).ok_or_else(short)?, 2);
    let rest = &entry[start + 14 + count * 9..];
    let (ground_width, cubes) = ground_heights(ground_entry)?;
    let mut gat = header.to_vec();
    for (index, kind) in types.iter().enumerate() {
        let guess = predicted(&cubes, ground_width, [index % width, index / width]);
        for (corner, guess) in guess.iter().enumerate() {
            let error = i16::from_le_bytes(errors[(index * 4 + corner) * 2..][..2].try_into().unwrap());
            gat.extend((guess + f32::from(error) / 100.0).to_le_bytes());
        }
        gat.extend((u32::from(kind & 0x7F) | u32::from(kind >> 7) << 31).to_le_bytes());
    }
    gat.extend(rest);
    Ok(gat)
}

#[cfg(all(test, feature = "store"))]
mod tests {
    use super::*;
    use crate::grf::Grf;

    #[test]
    fn splits_and_joins_byte_planes() {
        let bytes: Vec<u8> = (0..1000u32).map(|value| (value * 7 % 251) as u8).collect();
        assert_eq!(join(&split(&bytes, 4), 4), bytes);
    }

    #[test]
    fn keeps_cell_types_with_the_water_flag() {
        assert_eq!(type_byte(5), Some(5));
        assert_eq!(type_byte(0x8000_0005u32 as i32), Some(0x85));
        assert_eq!(type_byte(0x4000_0001), None);
        assert_eq!(type_byte(128), None);
    }

    /// A ground's columns and atlas give back the GND byte for byte (the atlas here not encoded, so not lossy), and its
    /// altitude the GAT within half a hundredth.
    #[test]
    fn rebuilds_grounds_and_altitudes() {
        let Some(grf) = Grf::for_tests() else { return };
        // 1@iwp's cells have the water flag (the type's top bit).
        for map in ["prontera", "iz_int01", "gef_fild10", "moc_pryd04", "pay_dun00", "1@iwp"] {
            let Ok(gnd) = grf.read(&format!("data\\{map}.gnd")) else { continue };
            let Ground { columns, atlas } = ground(&gnd).unwrap();
            let atlas = atlas.map(|atlas| (atlas.width(), atlas.height(), atlas.into_raw()));
            assert!(ground_from_columns(&columns, || Ok(atlas.unwrap())).unwrap() == gnd, "{map}: GND");
            let entry = ground_entry(&columns, &[]);

            let gat = grf.read(&format!("data\\{map}.gat")).unwrap();
            let altitude = altitude(&gat, &format!("data\\{map}.gnd"), &entry).unwrap_or_else(|| panic!("{map}: GAT doesn't fit"));
            let rebuilt = rebuild_altitude(&altitude, &entry).unwrap();
            assert_eq!(rebuilt.len(), gat.len(), "{map}: GAT length");
            for (a, b) in rebuilt[14..].chunks_exact(20).zip(gat[14..].chunks_exact(20)) {
                assert_eq!(a[16..], b[16..]);
                for corner in 0..4 {
                    let value = |cell: &[u8]| f32::from_le_bytes(cell[corner * 4..corner * 4 + 4].try_into().unwrap());
                    assert!((value(a) - value(b)).abs() <= 0.0051, "{map}: height");
                }
            }
        }
    }
}
