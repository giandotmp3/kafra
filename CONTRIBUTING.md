# Contributing

Bug reports, fixes and improvements are welcome. Please open an issue first for larger changes, such as a new
transform or a change to the format, so the approach can be agreed before you write it.

## Checks

A change should pass these before it is submitted:

```sh
cargo fmt --check
cargo clippy --all-targets
cargo clippy --lib --no-default-features
cargo clippy --lib --no-default-features --features rebuild
cargo test --release
```

The tests need no game data and no external tools. Tests that read a real `data.grf` run when one is at the path in
`KAFRA_GRF` (default `./data.grf`), and are skipped otherwise. If your change affects how files are stored, also run
`kafra store` and `kafra verify` on a real `data.grf` and include the results.

## Test fixtures

`tests/fixtures/sample.grf` and `tests/fixtures/sample.kafra` are generated. If a change alters what `kafra store`
writes, regenerate them (this needs `avifenc` and `ffmpeg`) and commit the result:

```sh
cargo run --release --example make_sample
```

## Format changes

[FORMAT.md](FORMAT.md) is the specification, and other readers are written from it. A change to the file layout or to
how a transform's data is read must:

- update FORMAT.md in the same change;
- bump `VERSION` in `src/format.rs` if existing readers would misread the new files.

A new transform needs a new number. Readers that don't know it still open the archive, and report the transform when
the file is read.

## Game data

Do not commit game data, or files derived from it, to the repository. Fixtures must be generated, as `make_sample`
does.

## License

Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in this project, as
defined in the Apache-2.0 license, will be dual-licensed under the MIT License and the Apache License, Version 2.0,
without any additional terms or conditions.
