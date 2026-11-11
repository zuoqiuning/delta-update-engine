use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use delta_update_engine::fs::{FileSystem, RealFs};

/// Delegates to the real filesystem and injects one-shot faults at a chosen
/// rename destination.
///
/// Targeting the destination keeps tests independent of operation order: the
/// journal may add or reorder unrelated ops without invalidating a fixture.
#[derive(Default)]
pub struct FaultyFs {
    inner: RealFs,
    fail: Mutex<Option<(PathBuf, io::ErrorKind)>>,
    corrupt: Mutex<Option<PathBuf>>,
}

impl FaultyFs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fail the next rename whose destination is `to`.
    pub fn fail_on_rename(self, to: impl Into<PathBuf>, kind: io::ErrorKind) -> Self {
        let mut next = self;
        next.fail = Mutex::new(Some((to.into(), kind)));
        next
    }

    /// Let the next rename to `to` succeed, then corrupt that destination.
    pub fn corrupt_on_rename(self, to: impl Into<PathBuf>) -> Self {
        let mut next = self;
        next.corrupt = Mutex::new(Some(to.into()));
        next
    }
}

impl FileSystem for FaultyFs {
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.inner.create_dir_all(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        if let Some(kind) = take_fault(&self.fail, to) {
            return Err(io::Error::new(
                kind,
                format!("injected failure renaming to {}", to.display()),
            ));
        }
        self.inner.rename(from, to)?;
        if take_target(&self.corrupt, to).is_some() {
            std::fs::write(to, b"corrupted-after-rename")
                .map_err(|err| io::Error::other(format!("corrupt {}: {err}", to.display())))?;
        }
        Ok(())
    }

    fn write_atomic(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.inner.write_atomic(path, data)
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        self.inner.remove(path)
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.inner.read(path)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.sync_dir(path)
    }
}

/// Take the one-shot failure if it targets `path`.
fn take_fault(
    slot: &Mutex<Option<(PathBuf, io::ErrorKind)>>,
    path: &Path,
) -> Option<io::ErrorKind> {
    let mut guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.as_ref().is_some_and(|(target, _)| target == path) {
        guard.take().map(|(_, kind)| kind)
    } else {
        None
    }
}

/// Take the one-shot corruption target if it matches `path`.
fn take_target(slot: &Mutex<Option<PathBuf>>, path: &Path) -> Option<PathBuf> {
    let mut guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.as_ref().is_some_and(|target| target == path) {
        guard.take()
    } else {
        None
    }
}
