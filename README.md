# KAFRA

[![CI](https://github.com/giandotmp3/kafra/actions/workflows/ci.yml/badge.svg)](https://github.com/giandotmp3/kafra/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE.md)
[![Rust 1.93+](https://img.shields.io/badge/rust-1.93%2B-orange.svg)](https://www.rust-lang.org/)

KAFRA converts a Ragnarok Online `data.grf` into a `.kafra` archive. The archive holds the same files, but pictures,
sounds and map data are re-encoded in modern formats that take far less space. A reader gets every file back in its
original format, so existing loaders don't need to change.

- **Smaller assets.** Pictures are stored as AVIF and sounds as Opus. Map grounds (`.gnd`) and altitudes (`.gat`) are
  split into columns that compress well. Everything else goes into zstd chunks. Identical files are stored once.
- **Checked before writing.** `kafra store` reads back every file it re-encodes and compares it with the original. Any
  file that doesn't come back within its transform's limits is stored unchanged.
- **Lightweight reader.** The library can read `.kafra` files without the encoders, and a `Kafra` can be shared
  between threads.
- **Documented format.** [FORMAT.md](FORMAT.md) specifies the file layout and every transform, so other readers can be
  written from it.

KAFRA reads GRF versions 0x200 and 0x300, including EUC-KR (Korean) paths and DES-scrambled entries. It does not
support archives whose file data was encrypted by third-party tools.

## Contents

- [Installation](#installation)
- [Usage](#usage)
- [Results](#results)
- [Fidelity](#fidelity)
- [Library](#library)
- [Format](#format)
- [Development](#development)
- [Contributing](#contributing)
- [License](#license)

## Installation

KAFRA needs Rust 1.93 or later.

```sh
cargo install --git https://github.com/giandotmp3/kafra
```

Or from a clone of the repository:

```sh
git clone https://github.com/giandotmp3/kafra.git
cd kafra
cargo install --path .
```

This installs the `kafra` command into Cargo's bin folder (`~/.cargo/bin` by default).

Creating a `.kafra` with `kafra store` also needs these tools on your `PATH`:

| Tool | Used for |
|---|---|
| [`avifenc`](https://github.com/AOMediaCodec/libavif) (libavif) | encoding pictures and ground lightmaps |
| [`ffmpeg`](https://ffmpeg.org/) | encoding sounds as Opus |

The other commands need neither tool.

## Usage

```text
kafra store   <data.grf> <out.kafra> [--jobs <n>] [--only <prefix>]... [--exclude <prefix | .ext>]...
kafra info    <file.kafra>
kafra list    <file.kafra> [<prefix>]
kafra extract <file.kafra> <folder> [<prefix>] [--stored]
kafra verify  <file.kafra> <data.grf> [<prefix>] [--jobs <n>]
```

| Command | Description |
|---|---|
| `store` | Creates a `.kafra` from a GRF. Every file is included by default. `--only` keeps only the files under the given path prefixes, and `--exclude` leaves out a path prefix or a file extension. Both options can be repeated. |
| `info` | Shows the format version, the file and chunk counts, and sizes by transform and by extension. |
| `list` | Lists each entry with its transform, its stored size and its original size. |
| `extract` | Writes the files to a folder in their original formats. With `--stored`, it writes the encoded data instead (`.avif`, `.opus`, …). Paths that would land outside the folder are refused. |
| `verify` | Rebuilds every file and compares it with the copy in the GRF. Exits with status 1 if any file fails. |

Paths can use either slash, and ASCII case doesn't matter, as in a GRF. `--jobs` defaults to the number of available
CPUs.

The exit status is `0` on success, `1` on failure and `2` on a usage error.

### Examples

Try the commands on the small sample archive in the repository, which holds no game data:

```sh
kafra info tests/fixtures/sample.kafra
kafra list tests/fixtures/sample.kafra data/texture
kafra extract tests/fixtures/sample.kafra out
kafra verify tests/fixtures/sample.kafra tests/fixtures/sample.grf
```

Create an archive without the robe sprites and the videos, then check it:

```sh
kafra store data.grf data.kafra \
    --exclude data/sprite/로브/ --exclude data/palette/로브/ \
    --exclude .bik --exclude .avi --exclude .mp4 --exclude .wmv --exclude .mpg

kafra verify data.kafra data.grf
```

## Results

`kafra store` with default options on a 4.59 GB official `data.grf` (GRF 0x300, 240,751 files) produced a 926.0 MB
`.kafra`, **79.8% smaller**. `kafra verify` then found 0 failures. By asset type:

| Asset type | Files | Uncompressed | In `data.grf` | In `.kafra` | Saved |
|---|--:|--:|--:|--:|--:|
| Pictures (`.bmp` `.tga` `.jpg` `.png`) | 65,627 | 4.71 GB | 1.98 GB | 215.3 MB | 89.1% |
| Sprites (`.spr` `.act`) | 149,820 | 8.07 GB | 1.26 GB | 407.8 MB | 67.7% |
| Map grounds (`.gnd`) | 1,059 | 3.68 GB | 549.3 MB | 112.8 MB | 79.5% |
| Map altitudes (`.gat`) | 1,082 | 1.87 GB | 244.3 MB | 17.4 MB | 92.9% |
| Sounds (`.wav`) | 3,478 | 211.7 MB | 189.3 MB | 18.8 MB | 90.1% |
| Videos (`.bik` `.avi` `.mp4` `.wmv` `.mpg`) | 6 | 83.1 MB | 81.2 MB | 80.6 MB | 0.7% |
| Models (`.rsm` `.rsm2`) | 9,322 | 126.0 MB | 46.5 MB | 34.9 MB | 24.9% |
| Map resources (`.rsw`) | 1,082 | 298.7 MB | 41.7 MB | 26.3 MB | 36.9% |
| Effects and palettes (`.str` `.pal` `.imf`) | 8,460 | 46.5 MB | 6.4 MB | 2.7 MB | 58.1% |
| Other | 815 | 63.8 MB | 12.2 MB | 7.6 MB | 38.3% |
| **Total** | **240,751** | **19.17 GB** | **4.41 GB** | **924.1 MB** | **79.1%** |

*In `data.grf`* is the zlib-compressed data the GRF holds for those files. *In `.kafra`* is each file's share of its
chunk after compression, with identical files counted once. The totals leave out the GRF's file table and the
`.kafra`'s index, which is why they differ from the file sizes. Sprites, models and the other raw types aren't
re-encoded: their savings come from zstd in place of zlib, and from storing duplicates once. One GRF entry that can't
be read is left out.

### Without garments and videos

With the options in the [example](#examples) (garment sprites and palettes under `로브`, and videos, left out), the same
`data.grf` gives a 700.0 MB `.kafra`. That leaves out 100,745 files, 843.6 MB of the GRF. The table compares only the
files that went in:

| Asset type | Files | Uncompressed | In `data.grf` | In `.kafra` | Saved |
|---|--:|--:|--:|--:|--:|
| Pictures (`.bmp` `.tga` `.jpg` `.png`) | 65,627 | 4.71 GB | 1.98 GB | 214.4 MB | 89.2% |
| Map grounds (`.gnd`) | 1,059 | 3.68 GB | 549.3 MB | 112.8 MB | 79.5% |
| Sprites (`.spr` `.act`) | 49,083 | 2.89 GB | 501.5 MB | 263.8 MB | 47.4% |
| Map altitudes (`.gat`) | 1,082 | 1.87 GB | 244.3 MB | 17.3 MB | 92.9% |
| Sounds (`.wav`) | 3,478 | 211.7 MB | 189.3 MB | 18.8 MB | 90.1% |
| Models (`.rsm` `.rsm2`) | 9,322 | 126.0 MB | 46.5 MB | 34.9 MB | 24.9% |
| Map resources (`.rsw`) | 1,082 | 298.7 MB | 41.7 MB | 26.3 MB | 36.9% |
| Effects and palettes (`.str` `.pal` `.imf`) | 8,460 | 46.5 MB | 6.4 MB | 2.7 MB | 57.6% |
| Other | 813 | 63.8 MB | 12.2 MB | 7.7 MB | 36.7% |
| **Total** | **140,006** | **13.90 GB** | **3.57 GB** | **698.7 MB** | **80.4%** |

The remaining sprites save less than the full set does (47.4% against 67.7%). The garment sprites compress unusually
well: 762 MB in the GRF becomes about 144 MB in the full `.kafra`.

## Fidelity

Some transforms are lossy. `store` checks each file against these guarantees before writing it, and `verify` checks
them again afterwards:

| Transform | Applies to | Guarantee |
|---|---|---|
| raw | all other files, and any file a transform couldn't keep within its limits | byte for byte |
| altitude | `.gat` | same size and cell types; heights within 0.0051 |
| ground | `.gnd` | byte for byte, except the lightmaps (lossy) |
| image | `.bmp` `.tga` `.jpg` `.png` under `data\texture\`, and effect sprites (not item icons, buttons or small UI pictures) | same dimensions and alpha; colour keys exact; colours lossy. JPEGs come back as BMPs. |
| sound | `.wav` | a 16-bit 48 kHz WAV of the same duration, within 0.1 s; samples lossy |

## Library

KAFRA is not on crates.io yet. Depend on it through its git repository:

```toml
[dependencies]
# The reader and its decoders only:
kafra = { git = "https://github.com/giandotmp3/kafra", default-features = false, features = ["rebuild"] }
```

```rust
let kafra = kafra::Kafra::open("data.kafra")?;

kafra.paths();                            // every path in the archive
kafra.contains("data/prontera.gnd");      // either slash, any ASCII case
kafra.entry("data/prontera.gnd");         // Option<EntryInfo>: transform, stored size, original size
kafra.read("data/prontera.gnd")?;         // the file in its original format
kafra.read_stored("data/prontera.gnd")?;  // the encoded data, without undoing the transform
```

`Kafra` is `Send + Sync`. Every error is a `kafra::Error`: `NotFound`, `BadMagic`, `UnsupportedVersion`, `Corrupt`,
`Decode`, `UnknownTransform`, `Unsupported` or `Io`. A truncated or damaged archive is reported as an error.

### Cargo features

| Feature | Default | Provides |
|---|---|---|
| `rebuild` | yes | The AVIF and Opus decoders needed to rebuild pictures, sounds and grounds. Without it, `read` rebuilds only raw files and altitudes and returns `Error::Unsupported` for the rest. `read_stored` still works for every entry. |
| `store` | yes | The GRF reader, `store::store` and `verify::verify`. Implies `rebuild`. |
| `cli` | yes | The `kafra` binary. Implies `store`. |

## Format

[FORMAT.md](FORMAT.md) specifies the `.kafra` layout and every transform, so a reader can be written in any language
without this crate. The format has a version number in its header, currently 1. A reader refuses a version it doesn't
know, and reports a transform it doesn't know only when that file is read, so newer transforms don't stop older readers
from opening an archive.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets
cargo test --release
```

The tests need no game data and no external tools. They read `tests/fixtures/sample.grf` and
`tests/fixtures/sample.kafra`, a small synthetic archive with at least one file for every transform. To regenerate them
(this needs `avifenc` and `ffmpeg`):

```sh
cargo run --release --example make_sample
```

Some tests also run against a real `data.grf`, at the path in `KAFRA_GRF` (default `./data.grf`). They are skipped if
the file doesn't exist.

## Contributing

Contributions are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for the checks a change should pass and the rules for
changing the format, and [CHANGELOG.md](CHANGELOG.md) for what has changed between releases.

## License

Licensed under either the [MIT License](LICENSE.md#mit-license) or the
[Apache License, Version 2.0](LICENSE.md#apache-license-version-20), at your option. Third-party notices are in
[LICENSE.md](LICENSE.md#third-party-notices).

Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in this project, as
defined in the Apache-2.0 license, will be dual-licensed as above, without any additional terms or conditions.

KAFRA does not include any game data. Ragnarok Online is a trademark of Gravity Co., Ltd. This project is not
affiliated with or endorsed by Gravity.
