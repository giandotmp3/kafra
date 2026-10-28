use std::fmt;
use std::io;

/// What can go wrong reading a `.kafra`.
#[derive(Debug)]
pub enum Error {
    /// Reading the file failed.
    Io(io::Error),
    /// No file at that path.
    NotFound(String),
    /// Not a `.kafra` at all.
    BadMagic,
    /// A `.kafra` of a version this reader doesn't know.
    UnsupportedVersion(u8),
    /// The file is damaged: something in it doesn't add up.
    Corrupt(String),
    /// An entry's AVIF or Opus data doesn't decode.
    Decode(String),
    /// An entry with a transform this reader doesn't know.
    UnknownTransform(u8),
    /// This build can't do it (see the crate's features).
    Unsupported(&'static str),
}

/// A `Result` whose error is this crate's `Error`.
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::NotFound(path) => write!(f, "no file {path}"),
            Self::BadMagic => write!(f, "not a .kafra file"),
            Self::UnsupportedVersion(version) => write!(f, "a .kafra of version {version}, which this reader doesn't know"),
            Self::Corrupt(what) => write!(f, "a damaged .kafra: {what}"),
            Self::Decode(what) => write!(f, "{what}"),
            Self::UnknownTransform(transform) => write!(f, "an entry with transform {transform}, which this reader doesn't know"),
            Self::Unsupported(what) => write!(f, "{what}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub(crate) fn corrupt(what: &str) -> Error {
    Error::Corrupt(what.to_owned())
}
