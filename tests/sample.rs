//! Reads the sample (`tests/fixtures`, from examples/make_sample.rs): no game data, so it runs anywhere.

use std::path::Path;

use kafra::{Error, Kafra, Transform};

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn sample() -> Kafra {
    Kafra::open(fixture("sample.kafra")).unwrap()
}

#[test]
fn lists_every_file() {
    let kafra = sample();
    let mut paths: Vec<&str> = kafra.paths().collect();
    paths.sort_unstable();
    assert_eq!(
        paths,
        [
            "data\\copy\\readme.txt",
            "data\\readme.txt",
            "data\\sample.gat",
            "data\\sample.gnd",
            "data\\sprite\\noise.bin",
            "data\\texture\\sample\\alpha.tga",
            "data\\texture\\sample\\keyed.bmp",
            "data\\texture\\sample\\plain.png",
            "data\\wav\\sample.wav",
            "data\\유저인터페이스\\note.txt",
        ]
    );
    assert_eq!(kafra.entry("data\\sample.gnd").unwrap().transform, Transform::Ground);
    assert_eq!(kafra.entry("data\\sample.gat").unwrap().transform, Transform::Altitude);
    assert_eq!(kafra.entry("data\\wav\\sample.wav").unwrap().transform, Transform::Sound);
    assert_eq!(kafra.entry("data\\readme.txt").unwrap().transform, Transform::Raw);
}

#[test]
fn finds_paths_with_either_slash_in_any_case() {
    let kafra = sample();
    assert!(kafra.contains("DATA/Readme.TXT"));
    assert_eq!(kafra.read("data/유저인터페이스/note.txt").unwrap(), kafra.read("DATA\\유저인터페이스\\NOTE.txt").unwrap());
    assert!(matches!(kafra.read("data/missing.txt"), Err(Error::NotFound(_))));
}

/// Every file comes back as `verify` holds its transform to, against the GRF it was made from.
#[test]
fn every_file_comes_back() {
    let kafra = sample();
    let grf = kafra::grf::Grf::open(fixture("sample.grf")).unwrap();
    let report = kafra::verify::verify(&kafra, &grf, None, 2);
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert!(report.not_in_grf.is_empty() && report.not_in_kafra.is_empty());
    // Raw files and the altitude's cell types are exact; pictures, sounds and lightmaps are lossy.
    assert_eq!(report.exact + report.close, 10);
    assert!(report.exact >= 4, "{report:?}");
}

#[test]
fn gives_back_the_stored_data() {
    let kafra = sample();
    let avif = kafra.read_stored("data/texture/sample/plain.png").unwrap();
    assert_eq!(&avif[4..8], b"ftyp");
    let opus = kafra.read_stored("data/wav/sample.wav").unwrap();
    assert_eq!(&opus[..4], b"OggS");
    let wav = kafra.read("data/wav/sample.wav").unwrap();
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(wav.len() as u32, kafra.entry("data/wav/sample.wav").unwrap().size);
}

/// Reads go on from several threads at once.
#[test]
fn reads_from_several_threads() {
    let kafra = sample();
    let paths: Vec<&str> = kafra.paths().collect();
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                for path in &paths {
                    kafra.read(path).unwrap();
                }
            });
        }
    });
}

/// Against a GRF whose files differ, `verify` names each one and why.
#[test]
fn verify_finds_what_differs() {
    let kafra = sample();
    let original = kafra::grf::Grf::open(fixture("sample.grf")).unwrap();
    let mut files: Vec<(String, Vec<u8>)> = original.paths().map(|path| (path.to_owned(), original.read(path).unwrap())).collect();
    for (path, data) in &mut files {
        match path.as_str() {
            "data\\readme.txt" => data[0] ^= 1,
            "data\\sample.gat" => data[14 + 16] ^= 1,
            _ => {}
        }
    }
    files.retain(|(path, _)| path != "data\\sprite\\noise.bin");
    files.push(("data\\extra.txt".into(), b"only in the GRF".to_vec()));
    let listed: Vec<(&str, &[u8])> = files.iter().map(|(path, data)| (path.as_str(), data.as_slice())).collect();
    let path = std::env::temp_dir().join(format!("kafra-verify-{}.grf", std::process::id()));
    std::fs::write(&path, kafra::grf::build(0x200, &listed)).unwrap();
    let changed = kafra::grf::Grf::open(&path).unwrap();
    std::fs::remove_file(path).unwrap();

    let report = kafra::verify::verify(&kafra, &changed, None, 2);
    let mut failed: Vec<&str> = report.failed.iter().map(|(path, _)| path.as_str()).collect();
    failed.sort_unstable();
    assert_eq!(failed, ["data\\readme.txt", "data\\sample.gat"], "{:?}", report.failed);
    assert!(report.failed.iter().any(|(_, why)| why.contains("type")), "{:?}", report.failed);
    assert_eq!(report.not_in_grf, ["data\\sprite\\noise.bin"]);
    assert_eq!(report.not_in_kafra, ["data\\extra.txt"]);
    assert!(!report.ok());
}
