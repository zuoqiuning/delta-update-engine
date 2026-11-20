//! Download one object with retries and verify it against the manifest.

use std::path::Path;

use tokio_util::sync::CancellationToken;

use crate::crypto::sha256_file;
use crate::error::{EngineError, PatchError};
use crate::fetch::Fetcher;

use super::{io_error, MAX_RETRIES};

pub(super) async fn download_verified(
    fetcher: &dyn Fetcher,
    key: &str,
    dest: &Path,
    expected_size: u64,
    expected_sha256: Option<&str>,
    cancel: &CancellationToken,
    progress: &(dyn Fn(u64) + Send + Sync),
) -> Result<(), EngineError> {
    if let Some(expected) = expected_sha256 {
        if tokio::fs::metadata(dest)
            .await
            .is_ok_and(|meta| meta.len() == expected_size)
        {
            let actual = hash_file(dest).await.map_err(|err| io_error(err, key))?;
            if actual == expected {
                return Ok(());
            }
        }
        // A same-size but corrupt object must not be resumed from EOF: most
        // object stores answer `Range: bytes=<size>-` with 416, which is not
        // retryable. Drop it and start a clean download.
        let _ = std::fs::remove_file(dest);
    }

    for attempt in 0..=MAX_RETRIES {
        download_with_retry(fetcher, key, dest, expected_size, cancel, progress).await?;
        let metadata = tokio::fs::metadata(dest)
            .await
            .map_err(|err| io_error(err, key))?;
        if metadata.len() == expected_size {
            if let Some(expected) = expected_sha256 {
                let actual = hash_file(dest).await.map_err(|err| io_error(err, key))?;
                if actual == expected {
                    return Ok(());
                }
            } else {
                return Ok(());
            }
        }
        let _ = std::fs::remove_file(dest);
        if attempt == MAX_RETRIES {
            return Err(PatchError::TargetHashMismatch(format!(
                "{key}: downloaded object does not match the manifest"
            ))
            .into());
        }
    }
    unreachable!("retry loop always returns")
}

async fn download_with_retry(
    fetcher: &dyn Fetcher,
    key: &str,
    dest: &Path,
    max_bytes: u64,
    cancel: &CancellationToken,
    progress: &(dyn Fn(u64) + Send + Sync),
) -> Result<(), EngineError> {
    let mut attempt = 0;
    loop {
        let mut resume = tokio::fs::metadata(dest)
            .await
            .map(|meta| meta.len())
            .unwrap_or(0);
        if max_bytes > 0 && resume >= max_bytes {
            // A stale or oversized partial file cannot be resumed.
            let _ = std::fs::remove_file(dest);
            resume = 0;
        }
        let result = tokio::select! {
            _ = cancel.cancelled() => return Err(EngineError::Cancelled),
            result = fetcher.download_with_progress(key, dest, resume, max_bytes, progress) => result,
        };
        match result {
            Ok(_) => return Ok(()),
            Err(error) if error.retryable() && attempt < MAX_RETRIES => {
                attempt += 1;
                let delay = error
                    .retry_after()
                    .unwrap_or_else(|| std::time::Duration::from_millis(200_u64 << (attempt - 1)));
                tokio::time::sleep(delay).await;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Hash a downloaded or staged file on the blocking pool.
pub(super) async fn hash_file(path: &Path) -> std::io::Result<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || sha256_file(&path))
        .await
        .map_err(std::io::Error::other)?
}

pub(super) async fn verify_staged(
    path: &str,
    dest: &Path,
    entry: &crate::manifest::model::FileEntry,
) -> Result<(), EngineError> {
    let metadata = std::fs::metadata(dest).map_err(|err| io_error(err, path))?;
    if metadata.len() != entry.size {
        return Err(PatchError::TargetHashMismatch(format!(
            "{path}: staged size {} does not match {}",
            metadata.len(),
            entry.size
        ))
        .into());
    }
    let actual = hash_file(dest).await.map_err(|err| io_error(err, path))?;
    if actual != entry.sha256 {
        return Err(PatchError::TargetHashMismatch(format!(
            "{path}: staged hash does not match the manifest"
        ))
        .into());
    }
    Ok(())
}
