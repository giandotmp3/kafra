# The .kafra format, version 1

A `.kafra` holds a GRF's files. Many of them are re-encoded far smaller, and a reader gives each one back in its
original format. Numbers are little-endian.

## Layout

```
header     22 bytes
chunks     one after another
index      zstd-compressed, at the end
```

### Header

| Offset | Size | What |
|---|---|---|
| 0 | 5 | `KAFRA` |
| 5 | 1 | the version: 1 |
| 6 | 8 | u64: where the index starts |
| 14 | 4 | u32: the index's size, compressed |
| 18 | 4 | u32: the index's size |

A reader refuses a version it doesn't know.

### Index

Once decompressed (zstd):

```
u32 chunk count
per chunk:
    u64 offset      where it starts in the file
    u32 stored      its size in the file
    u32 size        its size decompressed
    u8  codec       0 stored, 1 zstd
u32 entry count
per entry:
    u16 path length
    ... path        UTF-8, lowercase ASCII, backslashes: data\texture\...
    u32 chunk       which chunk its data is in
    u32 offset      where its data starts in the chunk, decompressed
    u32 length      its data's length
    u8  transform   how its data becomes the file (below)
    u8  flags       the transform's flags
    u32 size        the size of the file it rebuilds
```

A stored chunk's two sizes are equal. A reader decompresses a zstd chunk whole, and reads a stored one in place.
Several entries can point at the same data. Paths are looked up as the GRF does: ASCII case doesn't matter, and `/`
reads as `\`.

## Transforms

| Number | Name | The data | The file |
|---|---|---|---|
| 0 | raw | the file | itself |
| 1 | image | an AVIF | a BMP, TGA or PNG |
| 2 | sound | an Ogg Opus stream | a 16-bit PCM WAV |
| 3 | ground | columns and an AVIF lightmap atlas | the GND |
| 4 | altitude | cell types and height errors | the GAT |

A reader that meets a transform it doesn't know reports it when the file is read, not when the `.kafra` is opened.

**Byte planes.** Some columns hold values *w* bytes wide stored by byte: every value's first byte, then every value's
second byte, and so on. zstd compresses numbers far better that way.

### 1: image

The flags' low two bits give the source format: 0 BMP, 1 TGA, 2 JPG, 3 PNG. Bit 2 (`KEYED`) means the picture had
colour keys. Bit 3 (`ALPHA`) means it had an alpha channel.

To rebuild one, decode the AVIF to RGBA, then:

- **With `ALPHA`:** keep the alpha.
- **Otherwise:** drop the alpha. With `KEYED`, first colour every pixel whose alpha is under 64 `#FF00FF` (magenta),
  and every one under 192 black.
- **Write the result as:** a TGA if the source was a TGA, a PNG if a PNG, and otherwise a BMP. JPGs come back as
  BMPs, because loaders tell them apart by content.

`kafra store` puts the keys in the alpha, losslessly. Magenta (R ≥ 254, G ≤ 3, B ≥ 254) gets alpha 0. In effect
pictures, near-black (each channel under 8) gets alpha 128.

### 2: sound

An Ogg Opus stream. It rebuilds as a 16-bit PCM WAV at 48 kHz with the stream's channel count, with the stream's
pre-skip and end trim applied, and a 44-byte header.

### 3: ground

```
u32   columns length
...   columns
...   lightmap atlas: an AVIF, or nothing if the GND has no lightmaps
```

The columns are:

| What | How |
|---|---|
| u32 *head length*, then the head | the GND's first bytes, up to and including the 16-byte lightmap header |
| u32 *S* | the surface count |
| u32 *T*, then *T* × 32 bytes | the surfaces' distinct texture coordinates (u[4], v[4]), as 4-byte planes |
| *S* × u32 | each surface's place in that table, as 4-byte planes |
| *S* × u16 | each surface's texture, as 2-byte planes |
| *S* × i32 | each surface's lightmap minus its own index, as 4-byte planes |
| *S* × 4 bytes | each surface's colour, as it is |
| 4 × *W*·*H* × f32 | the cubes' heights, corner by corner (all cubes' first corners, then their second corners, and so on), as 4-byte planes |
| 3 × *W*·*H* × i32 | the cubes' top, north and east surfaces (−1 for none), each side's values as differences from the previous cube's (wrapping), as 4-byte planes |
| the rest | the GND's bytes after the cubes |

*W* and *H* are the GND's width and height, at offsets 6 and 10. From GND 1.7 on, a cube's surface indices are 32
bits wide; before it, 16.

**The atlas** holds every lightmap as one tile of *w* × *h* texels, where *w* and *h* are the lightmap header's
texel size. Each texel's colour goes in RGB and its shadow in alpha. The tiles are laid out on a grid
3*W* + *q* tiles wide and max(*H*, *q*) tiles high:

1. For each side (top, then north, then east) and each cube in order: if the cube has a surface on that side, and
   that surface's lightmap isn't placed yet, place it at tile (*side* · *W* + *x*, *y*), where (*x*, *y*) is the
   cube's position.
2. Place the *n*-th lightmap that no surface uses at tile (3*W* + *n* mod *q*, *n* div *q*). *q* is the smallest whole
   number whose square holds every unused lightmap.

Neighbouring texels end up next to each other, so the picture compresses. Rebuilding the GND writes each lightmap
back from its tile, its shadows first and then its colours, between the head and the surfaces.

### 4: altitude

```
u16   ground path length
...   ground path        the entry, in this .kafra, of the GND it's told against
14    the GAT's header
W·H   cell types         a byte each: the type's low 7 bits, and its top bit (the water flag) as bit 7
W·H·4 × i16              each corner's error in hundredths, as 2-byte planes
...   the rest           the GAT's bytes after its cells
```

A cell's corner height is its prediction plus its error ÷ 100. The prediction comes from the ground's cube under the
cell, two cells a side to a cube: cube (*x* div 2, *y* div 2), corners *c*0–*c*3, or zeros if there's no such cube.
With *u* = (*x* mod 2) ÷ 2 and *v* = (*y* mod 2) ÷ 2, and

```
at(u, v) = south + (north − south) · v,  south = c0 + (c1 − c0) · u,  north = c2 + (c3 − c2) · u
```

the four corners are at(*u*, *v*), at(*u* + ½, *v*), at(*u*, *v* + ½) and at(*u* + ½, *v* + ½), in f32.

`kafra store` only uses this when every height comes back within half a hundredth. Otherwise the GAT goes in raw.

## What comes back exactly

| Transform | What a reader gets |
|---|---|
| raw | the file, byte for byte |
| altitude | the same size and cell types; heights within 0.0051 |
| ground | byte for byte, except the lightmaps, which are lossy |
| image | the same size in pixels and alpha, colour keys exact; the colours are lossy |
| sound | a WAV the same length (to a tenth of a second); the samples are lossy |

An entry's *size* is the size this crate's reader rebuilds. That's exact for every transform except PNG pictures,
where another PNG encoder can produce a different size.
