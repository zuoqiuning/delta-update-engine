//! Manifest parsing, diff classification and known-file history.

pub mod diff;
pub mod hash_cache;
pub mod history;
pub mod model;
pub mod parse;

pub use diff::{diff, DiffResult, LocalFile, LocalManifest, ModifiedFile};
pub use hash_cache::HashCache;
pub use history::KnownFiles;
pub use model::{FileEntry, Manifest, Strategy};
pub use parse::{parse_manifest, SUPPORTED_SCHEMA};
