use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use fs4::{FileExt, TryLockError};
use serde::{Deserialize, Serialize};

use crate::error::EngineError;

/// Metadata written while the lock is held.
///
/// A file that still has metadata was left by a process that died without
/// running `Drop` (a crash or a kill), because a normal release truncates it.
/// The OS already released the lock, so acquiring it proves the holder is gone;
/// the metadata is used only to report what happened. `boot_id` separates "died
/// in this boot" from "died before the last restart".
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
struct LockInfo {
    pid: u32,
    started_at_unix: u64,
    boot_id: u64,
}

impl LockInfo {
    fn current() -> Self {
        let start = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        Self {
            pid: std::process::id(),
            started_at_unix: start,
            boot_id: start.saturating_sub(sysinfo::System::uptime()),
        }
    }
}

const MAX_LOCK_INFO_BYTES: u64 = 4096;

fn read_info(file: &mut File) -> Option<LockInfo> {
    let mut bytes = Vec::new();
    file.seek(SeekFrom::Start(0)).ok()?;
    Read::by_ref(&mut *file)
        .take(MAX_LOCK_INFO_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_info(file: &mut File) -> Result<(), EngineError> {
    let bytes = serde_json::to_vec(&LockInfo::current())
        .map_err(|err| EngineError::Internal(format!("update lock: {err}")))?;
    file.set_len(0)
        .and_then(|()| file.seek(SeekFrom::Start(0)).map(|_| ()))
        .and_then(|()| file.write_all(&bytes))
        .and_then(|()| file.sync_data())
        .map_err(|err| EngineError::Internal(format!("update lock: {err}")))
}

/// Cross-process exclusive update lock.
///
/// The OS releases the lock when the process exits, so a stale file never
/// blocks the next attempt. The holder writes `{pid, started_at_unix, boot_id}`
/// and truncates it in `Drop`; a file with metadata therefore means the previous
/// holder was killed mid-update, which is reported as a cleared stale lock.
///
/// **Not reentrant.** `fs4` documents that acquiring an advisory lock twice
/// through the same process (or a cloned handle) is platform-dependent and may
/// deadlock instead of returning `WouldBlock`. Never call [`UpdateLock::acquire`]
/// while the same call stack already holds one for the same journal directory;
/// split the work into a locked and an unlocked helper instead.
pub struct UpdateLock {
    file: File,
}

impl UpdateLock {
    pub fn acquire(path: &Path) -> Result<Self, EngineError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| EngineError::Internal(format!("update lock: {err}")))?;
        }
        let mut file = OpenOptions::new()
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
        if let Some(previous) = read_info(&mut file) {
            let current = LockInfo::current();
            tracing::info!(
                pid = previous.pid,
                age_secs = current
                    .started_at_unix
                    .saturating_sub(previous.started_at_unix),
                boot_changed = previous.boot_id != current.boot_id,
                "cleared an update lock left by a process that exited during an update"
            );
        }
        if let Err(error) = write_info(&mut file) {
            // Never leave a half-written record behind: it would look like a
            // crash to the next process even though this one released cleanly.
            let _ = file.set_len(0);
            return Err(error);
        }
        Ok(Self { file })
    }
}

impl Drop for UpdateLock {
    fn drop(&mut self) {
        // Empty metadata marks a clean release; anything left behind is a crash.
        // `Drop` runs on early returns and unwinding too, so the only way to
        // leave metadata behind is a kill or an abort.
        let _ = self.file.set_len(0);
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_release_leaves_no_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update.lock");
        drop(UpdateLock::acquire(&path).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"");
    }

    #[test]
    fn metadata_from_a_dead_holder_does_not_block_the_next_acquisition() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update.lock");
        // A process killed mid-update leaves the file and its metadata behind.
        std::fs::write(&path, br#"{"pid":424242,"started_at_unix":1,"boot_id":1}"#).unwrap();
        let lock = UpdateLock::acquire(&path).unwrap();
        drop(lock);
        assert_eq!(std::fs::read(&path).unwrap(), b"");
    }

    #[test]
    fn unparsable_metadata_is_treated_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update.lock");
        std::fs::write(&path, b"partial write").unwrap();
        assert!(UpdateLock::acquire(&path).is_ok());
    }

    #[test]
    fn panic_releases_and_truncates_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update.lock");
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = std::panic::catch_unwind(|| {
            let _lock = UpdateLock::acquire(&path).unwrap();
            panic!("update thread panicked while holding the lock");
        });
        std::panic::set_hook(hook);

        assert!(result.is_err());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"",
            "unwinding must clear the crash marker"
        );
        assert!(UpdateLock::acquire(&path).is_ok());
    }
}
