//! Filesystem abstraction used by the journal, committer and staging code.
//!
//! Fault injection is implemented by swapping this trait, not by changing
//! production control flow.

use std::io;
use std::path::Path;

pub mod atomic;
pub mod real;

pub trait FileSystem: Send + Sync {
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn write_atomic(&self, path: &Path, data: &[u8]) -> io::Result<()>;
    fn remove(&self, path: &Path) -> io::Result<()>;
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    fn sync_dir(&self, path: &Path) -> io::Result<()>;
}

pub use real::RealFs;

/// Remove a directory tree, tolerating transient sharing violations.
///
/// Windows antivirus and indexers briefly open freshly written files, which
/// makes an otherwise empty staging directory fail with `ERROR_DIR_NOT_EMPTY`.
/// Cleanup is best-effort, but one retry window avoids leaking the whole
/// staging tree for the lifetime of the installation.
pub fn remove_dir_all_best_effort(path: &Path) {
    for attempt in 0..3 {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return,
            Err(_) if attempt < 2 => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(_) => return,
        }
    }
}
