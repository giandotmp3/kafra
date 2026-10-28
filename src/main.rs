//! The `kafra` tool: stores a GRF in a `.kafra`, and shows, extracts and checks what one holds.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use kafra::grf::Grf;
use kafra::{Kafra, Transform, normalize};

const USAGE: &str = "usage:
  kafra store <data.grf> <out.kafra> [--jobs <n>] [--only <path prefix>]... [--exclude <path prefix | .extension>]...
  kafra info <file.kafra>
  kafra list <file.kafra> [<path prefix>]
  kafra extract <file.kafra> <folder> [<path prefix>] [--stored]
  kafra verify <file.kafra> <data.grf> [<path prefix>] [--jobs <n>]

Paths take either slash, in any case. `--only` and `--exclude` repeat; `--exclude .bik` leaves out an extension.
`extract --stored` writes the entries' data as stored (.avif, .opus...) instead of the original files.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (command, rest) = args.split_first().map_or(("", &[][..]), |(command, rest)| (command.as_str(), rest));
    let (positional, options) = split_options(rest);
    let result = match (command, positional.as_slice()) {
        ("store", [grf, out]) => store(grf, out, &options),
        ("info", [file]) => info(file),
        ("list", [file, prefix @ ..]) if prefix.len() <= 1 => list(file, prefix.first().copied()),
        ("extract", [file, folder, prefix @ ..]) if prefix.len() <= 1 => extract(file, folder, prefix.first().copied(), options.flag("--stored")),
        ("verify", [file, grf, prefix @ ..]) if prefix.len() <= 1 => verify(file, grf, prefix.first().copied(), &options),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("kafra: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The `--name value` options and `--flag`s, after the positional arguments they're mixed with.
struct Options<'a>(Vec<(&'a str, Option<&'a str>)>);

impl Options<'_> {
    fn values(&self, name: &str) -> Vec<String> {
        self.0.iter().filter(|(option, _)| *option == name).filter_map(|(_, value)| value.map(str::to_owned)).collect()
    }

    fn flag(&self, name: &str) -> bool {
        self.0.iter().any(|(option, _)| *option == name)
    }

    fn jobs(&self) -> Result<usize, String> {
        match self.values("--jobs").last() {
            Some(jobs) => jobs.parse().map_err(|_| format!("--jobs {jobs}: not a number")),
            None => Ok(kafra::store::Options::default().jobs),
        }
    }
}

fn split_options(args: &[String]) -> (Vec<&str>, Options<'_>) {
    const WITH_VALUES: [&str; 3] = ["--jobs", "--only", "--exclude"];
    let (mut positional, mut options) = (Vec::new(), Vec::new());
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if WITH_VALUES.contains(&arg.as_str()) {
            options.push((arg.as_str(), args.next().map(String::as_str)));
        } else if arg.starts_with("--") {
            options.push((arg.as_str(), None));
        } else {
            positional.push(arg.as_str());
        }
    }
    (positional, Options(options))
}

type Outcome = Result<bool, Box<dyn std::error::Error>>;

fn store(grf: &str, out: &str, options: &Options) -> Outcome {
    let grf = Grf::open(grf).map_err(|error| format!("{grf}: {error}"))?;
    let options = kafra::store::Options { jobs: options.jobs()?, only: options.values("--only"), exclude: options.values("--exclude") };
    let summary = kafra::store::store(&grf, Path::new(out), &options)?;
    eprintln!("{out}: {} ({} distinct), {}", files(summary.files), summary.distinct, bytes(summary.size));
    Ok(true)
}

fn open(file: &str) -> Result<Kafra, String> {
    Kafra::open(file).map_err(|error| format!("{file}: {error}"))
}

fn info(file: &str) -> Outcome {
    let kafra = open(file)?;
    let size = std::fs::metadata(file)?.len();
    println!("{file}: KAFRA version {}, {} in {} chunks, {}", kafra::FORMAT_VERSION, files(kafra.len()), kafra.chunk_count(), bytes(size));
    // Sizes before chunk compression: what each kind's data is, not its share of the file.
    let mut by_transform: BTreeMap<&str, (usize, u64, u64)> = BTreeMap::new();
    let mut by_extension: BTreeMap<String, (usize, u64, u64)> = BTreeMap::new();
    for (path, entry) in kafra.entries() {
        let extension = path.rsplit_once('.').map_or("", |(_, extension)| extension);
        for totals in [by_transform.entry(entry.transform.name()).or_default(), by_extension.entry(extension.to_owned()).or_default()] {
            totals.0 += 1;
            totals.1 += u64::from(entry.stored_size);
            totals.2 += u64::from(entry.size);
        }
    }
    let table = |title: &str, rows: Vec<(String, (usize, u64, u64))>| {
        println!("\n{title:<12} {:>9} {:>12} {:>12}", "files", "stored", "original");
        for (name, (files, stored, size)) in rows {
            println!("{name:<12} {files:>9} {:>12} {:>12}", bytes(stored), bytes(size));
        }
    };
    table("transform", by_transform.into_iter().map(|(name, totals)| (name.to_owned(), totals)).collect());
    let mut extensions: Vec<_> = by_extension.into_iter().collect();
    extensions.sort_by_key(|(_, (_, stored, _))| std::cmp::Reverse(*stored));
    extensions.truncate(12);
    table("extension", extensions);
    Ok(true)
}

fn sorted_entries<'a>(kafra: &'a Kafra, prefix: Option<&str>) -> Vec<(&'a str, kafra::EntryInfo)> {
    let prefix = prefix.map(normalize).unwrap_or_default();
    let mut entries: Vec<_> = kafra.entries().filter(|(path, _)| path.starts_with(prefix.as_str())).collect();
    entries.sort_unstable_by_key(|(path, _)| *path);
    entries
}

fn list(file: &str, prefix: Option<&str>) -> Outcome {
    let kafra = open(file)?;
    println!("{:<9} {:>10} {:>10}  path", "transform", "stored", "size");
    for (path, entry) in sorted_entries(&kafra, prefix) {
        println!("{:<9} {:>10} {:>10}  {path}", entry.transform.name(), entry.stored_size, entry.size);
    }
    Ok(true)
}

fn extract(file: &str, folder: &str, prefix: Option<&str>, stored: bool) -> Outcome {
    let kafra = open(file)?;
    let (mut written, mut failed) = (0, 0);
    for (path, entry) in sorted_entries(&kafra, prefix) {
        let Some(mut target) = safe_path(Path::new(folder), path) else {
            eprintln!("{path}: a path that would land outside {folder}; skipped");
            failed += 1;
            continue;
        };
        let data = if stored {
            target.as_mut_os_string().push(stored_extension(entry.transform));
            kafra.read_stored(path)
        } else {
            kafra.read(path)
        };
        let result = data
            .map_err(|error| error.to_string())
            .and_then(|data| std::fs::create_dir_all(target.parent().unwrap()).and_then(|()| std::fs::write(&target, data)).map_err(|error| error.to_string()));
        match result {
            Ok(()) => written += 1,
            Err(error) => {
                eprintln!("{path}: {error}");
                failed += 1;
            }
        }
    }
    eprintln!("{} written to {folder}{}", files(written), if failed > 0 { format!(", {failed} failed") } else { String::new() });
    Ok(failed == 0)
}

/// "1 file", "2 files".
fn files(count: usize) -> String {
    format!("{count} file{}", if count == 1 { "" } else { "s" })
}

/// A size in B, kB, MB or GB.
fn bytes(size: u64) -> String {
    match size {
        0..1_000 => format!("{size} B"),
        1_000..1_000_000 => format!("{:.1} kB", size as f64 / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1} MB", size as f64 / 1e6),
        _ => format!("{:.2} GB", size as f64 / 1e9),
    }
}

/// Where an entry goes under `folder`: its path's parts as folders, refused if one could climb out (`..`, a drive).
fn safe_path(folder: &Path, path: &str) -> Option<PathBuf> {
    let mut target = folder.to_path_buf();
    for part in path.split('\\') {
        if part.is_empty() || part == "." || part == ".." || part.contains(['/', ':', '\0']) {
            return None;
        }
        target.push(part);
    }
    Some(target)
}

/// What `extract --stored` adds to an entry's name: what its data is.
fn stored_extension(transform: Transform) -> &'static str {
    match transform {
        Transform::Raw => "",
        Transform::Image => ".avif",
        Transform::Sound => ".opus",
        Transform::Ground | Transform::Altitude => ".columns",
        Transform::Unknown(_) => ".stored",
    }
}

fn verify(file: &str, grf: &str, prefix: Option<&str>, options: &Options) -> Outcome {
    let kafra = open(file)?;
    let grf = Grf::open(grf).map_err(|error| format!("{grf}: {error}"))?;
    let report = kafra::verify::verify(&kafra, &grf, prefix, options.jobs()?);
    for (path, why) in &report.failed {
        println!("FAILED  {path}: {why}");
    }
    for path in &report.not_in_grf {
        println!("EXTRA   {path}: not in the GRF");
    }
    println!(
        "{} exact, {} as close as their transform keeps them, {} failed, {} not in the GRF; {} of the GRF's files not in the .kafra",
        report.exact,
        report.close,
        report.failed.len(),
        report.not_in_grf.len(),
        report.not_in_kafra.len()
    );
    Ok(report.ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_only_inside_the_folder() {
        let folder = Path::new("out");
        assert_eq!(safe_path(folder, "data\\유저인터페이스\\a.bmp"), Some(folder.join("data").join("유저인터페이스").join("a.bmp")));
        for path in ["data\\..\\..\\etc\\passwd", "..\\a", "\\a", "data\\\\a", "c:\\a", "data\\.\\a", "data/../a"] {
            assert_eq!(safe_path(folder, path), None, "{path}");
        }
    }
}
