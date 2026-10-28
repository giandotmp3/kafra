//! KAFRA: a Ragnarok Online GRF's files in one `.kafra`, many of them re-encoded far smaller, read back in their
//! original formats so no loader changes. FORMAT.md describes the file.
//!
//! ```no_run
//! let kafra = kafra::Kafra::open("data.kafra")?;
//! let ground = kafra.read("data/prontera.gnd")?; // the GND as it was
//! # Ok::<(), kafra::Error>(())
//! ```
//!
//! Features: `rebuild` (on by default) brings the AVIF and Opus decoders that pictures, sounds and grounds need;
//! without it, `Kafra::read` gives back only raw files and altitudes, and `Kafra::read_stored` any entry's data. `store`
//! brings the GRF reader, `store::store` and `verify`; `cli` the `kafra` tool.

mod error;
mod format;
mod reader;
pub mod transform;
pub mod writer;

#[cfg(feature = "store")]
pub mod grf;
#[cfg(feature = "store")]
pub mod store;
#[cfg(feature = "store")]
pub mod verify;

pub use error::{Error, Result};
pub use format::{VERSION as FORMAT_VERSION, normalize};
pub use reader::{EntryInfo, Kafra, Transform};
