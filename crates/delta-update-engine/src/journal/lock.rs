use std::fs::{File, OpenOptions};
use std::path::Path;

use fs4::{FileExt, TryLockError};

use crate::error::EngineError;

/// Cross-process exclusive update lock.
///
/// The OS releases the lock when the process exits, so a stale file never
/// blocks the next attempt.
pub struct UpdateLock {
    file: File,
}

impl UpdateLock {
    pub fn acquire(path: &Path) -> Result<Self, EngineError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| EngineError::Internal(format!("update lock: {err}")))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(|err| EngineError::Internal(format!("update lock: {err}")))?;
        FileExt::try_lock(&file).map_err(|err| match err {
            TryLockError::WouldBlock => EngineError::UpdateLocked,
            TryLockError::Error(err) => EngineError::Internal(format!("update lock: {err}")),
        })?;
        Ok(Self { file })
    }
}

impl Drop for UpdateLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}
