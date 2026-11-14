//! Filesystem abstraction used by the journal, committer and staging code.
//!
//! Fault injection is implemented by swapping this trait, not by changing
//! production control flow.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

pub mod atomic;
pub mod real;

/// Sharing-violation retries in this process since the last reset. The worker
/// resets it before applying a commit and writes the total into `worker.log`,
/// so a successful retry is observable after the fact instead of invisible.
static RETRY_COUNT: AtomicU64 = AtomicU64::new(0);

pub fn reset_retry_count() {
    RETRY_COUNT.store(0, Ordering::Relaxed);
}

pub fn retry_count() -> u64 {
    RETRY_COUNT.load(Ordering::Relaxed)
}

pub trait FileSystem: Send + Sync {
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn write_atomic(&self, path: &Path, data: &[u8]) -> io::Result<()>;
    fn remove(&self, path: &Path) -> io::Result<()>;
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    fn sync_dir(&self, path: &Path) -> io::Result<()>;
}

pub use real::RealFs;

/// Windows sharing/lock violation (antivirus scan, indexer, lingering handle).
pub fn is_file_in_use(error: &io::Error) -> bool {
    #[cfg(windows)]
    {
        const ERROR_SHARING_VIOLATION: i32 = 32;
        const ERROR_LOCK_VIOLATION: i32 = 33;
        matches!(
            error.raw_os_error(),
            Some(ERROR_SHARING_VIOLATION) | Some(ERROR_LOCK_VIOLATION)
        )
    }
    #[cfg(not(windows))]
    {
        let _ = error;
        false
    }
}

/// Retry sharing violations with 200 / 400 / 800 ms backoff.
///
/// Antivirus scanners typically hold a freshly written file for a few hundred
/// milliseconds and up to a few seconds. Every move/remove path uses the same
/// patience so same-volume and cross-volume behaviour does not diverge.
pub fn retry_file_in_use<T>(mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    retry_while(&mut op, is_file_in_use)
}

fn retry_while<T>(
    mut op: impl FnMut() -> io::Result<T>,
    retry: impl Fn(&io::Error) -> bool,
) -> io::Result<T> {
    const DELAYS: [std::time::Duration; 3] = [
        std::time::Duration::from_millis(200),
        std::time::Duration::from_millis(400),
        std::time::Duration::from_millis(800),
    ];
    let mut attempt = 0;
    loop {
        match op() {
            Ok(value) => return Ok(value),
            Err(error) if attempt < DELAYS.len() && retry(&error) => {
                RETRY_COUNT.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    attempt = attempt + 1,
                    error = %describe_io_error(&error),
                    "file in use; retrying"
                );
                std::thread::sleep(DELAYS[attempt]);
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Cleanup also sees `ERROR_DIR_NOT_EMPTY` (145): antivirus or the indexer
/// briefly opens files that were just moved out, and Windows then reports the
/// directory as non-empty even though it is empty.
fn is_transient_cleanup_error(error: &io::Error) -> bool {
    if is_file_in_use(error) {
        return true;
    }
    #[cfg(windows)]
    {
        const ERROR_DIR_NOT_EMPTY: i32 = 145;
        matches!(error.raw_os_error(), Some(ERROR_DIR_NOT_EMPTY))
    }
    #[cfg(not(windows))]
    {
        error.kind() == io::ErrorKind::DirectoryNotEmpty
    }
}

/// Whether this metadata describes a cloud placeholder that would hydrate on
/// read (OneDrive / Files On-Demand, HSM offline files).
///
/// Hashing a placeholder triggers a network download and can block `check` for
/// tens of seconds. `scan_local` skips these files and warns instead of
/// silently hydrating the user's install tree.
pub fn is_cloud_placeholder(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // `RECALL_ON_DATA_ACCESS` means reading the data would recall it from
        // the cloud; `OFFLINE` means the data is not locally available either.
        // `RECALL_ON_OPEN` alone is deliberately ignored: OneDrive can leave it
        // set on files that are already fully hydrated, so treating it as a
        // placeholder would skip files the user can read locally.
        const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
        const OFFLINE: u32 = 0x0000_1000;
        metadata.file_attributes() & (RECALL_ON_DATA_ACCESS | OFFLINE) != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

/// Human-readable I/O error.
///
/// On Windows, sharing/lock violations (OS error 32/33) are almost always a
/// virus scanner, search indexer, or the still-running old executable. Naming
/// the cause turns an opaque "os error 32" into an actionable message for
/// support and for the retry guidance in the troubleshooting docs.
pub fn describe_io_error(error: &io::Error) -> String {
    if is_file_in_use(error) {
        return format!(
            "{error}（文件正被其他进程占用，常见于杀毒软件实时扫描或旧进程尚未退出；稍后重试）"
        );
    }
    error.to_string()
}

/// Remove a directory tree, tolerating transient sharing violations.
///
/// Windows antivirus and indexers briefly open freshly written files, which
/// makes an otherwise empty staging directory fail with `ERROR_DIR_NOT_EMPTY`.
/// Returns `false` when the directory could not be removed; callers stay
/// best-effort but can surface a warning instead of silently leaking it.
pub fn remove_dir_all_best_effort(path: &Path) -> bool {
    match retry_while(|| std::fs::remove_dir_all(path), is_transient_cleanup_error) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The retry counter is process-wide, so tests that assert its value must
    /// not run concurrently with each other.
    static COUNTER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn counter_guard() -> std::sync::MutexGuard<'static, ()> {
        COUNTER_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    #[test]
    fn other_errors_pass_through_unchanged() {
        assert_eq!(describe_io_error(&io::Error::other("boom")), "boom");
    }

    #[cfg(windows)]
    #[test]
    fn sharing_violations_name_the_likely_cause() {
        for code in [32, 33] {
            let text = describe_io_error(&io::Error::from_raw_os_error(code));
            assert!(text.contains("占用"), "unexpected text: {text}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn sharing_violations_are_retried_then_succeed() {
        let _guard = counter_guard();
        let mut attempts = 0;
        let result = retry_file_in_use(|| {
            attempts += 1;
            if attempts < 3 {
                Err(io::Error::from_raw_os_error(32))
            } else {
                Ok(7)
            }
        });
        assert_eq!(result.unwrap(), 7);
        assert_eq!(attempts, 3);
    }

    #[cfg(windows)]
    #[test]
    fn sharing_violations_give_up_after_three_attempts() {
        let _guard = counter_guard();
        let mut attempts = 0;
        let result: io::Result<()> = retry_file_in_use(|| {
            attempts += 1;
            Err(io::Error::from_raw_os_error(33))
        });
        assert!(result.is_err());
        assert_eq!(attempts, 4);
    }

    #[cfg(windows)]
    #[test]
    fn unrelated_errors_are_not_retried() {
        let _guard = counter_guard();
        let mut attempts = 0;
        let result: io::Result<()> = retry_file_in_use(|| {
            attempts += 1;
            Err(io::Error::from(io::ErrorKind::NotFound))
        });
        assert!(result.is_err());
        assert_eq!(attempts, 1);
    }

    #[test]
    fn a_successful_operation_does_not_increment_the_counter() {
        let _guard = counter_guard();
        reset_retry_count();
        retry_file_in_use(|| Ok(())).unwrap();
        assert_eq!(retry_count(), 0);
    }

    #[test]
    fn regular_files_are_not_cloud_placeholders() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("regular.bin");
        std::fs::write(&path, b"data").unwrap();
        let metadata = std::fs::metadata(&path).unwrap();
        assert!(!is_cloud_placeholder(&metadata));
    }

    #[cfg(windows)]
    #[test]
    fn successful_retries_increment_the_worker_counter() {
        let _guard = counter_guard();
        reset_retry_count();
        let mut attempts = 0;
        let result = retry_file_in_use(|| {
            attempts += 1;
            if attempts < 3 {
                Err(io::Error::from_raw_os_error(32))
            } else {
                Ok(())
            }
        });
        assert!(result.is_ok());
        assert_eq!(retry_count(), 2);
    }

    #[cfg(windows)]
    #[test]
    fn directory_not_empty_is_retried_for_cleanup() {
        let _guard = counter_guard();
        let mut attempts = 0;
        let result = retry_while(
            || {
                attempts += 1;
                if attempts < 3 {
                    Err(io::Error::from_raw_os_error(145))
                } else {
                    Ok(())
                }
            },
            is_transient_cleanup_error,
        );
        assert!(result.is_ok());
        assert_eq!(attempts, 3);
    }

    #[cfg(windows)]
    #[test]
    fn permission_denied_is_not_retried_for_cleanup() {
        let mut attempts = 0;
        let result: io::Result<()> = retry_while(
            || {
                attempts += 1;
                Err(io::Error::from_raw_os_error(5))
            },
            is_transient_cleanup_error,
        );
        assert!(result.is_err());
        assert_eq!(attempts, 1);
    }
}
