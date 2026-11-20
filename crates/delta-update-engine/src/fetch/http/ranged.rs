//! Range-based parallel download: chunks, retries and atomic merge.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use reqwest::header::RANGE;
use url::Url;

use crate::error::EngineError;

use super::support::{network_error, rate_limited};
use super::{DownloadReceipt, HttpFetcher, ProgressSink, MAX_RETRIES, RETRY_BASE_DELAY_MS};

enum ChunkOutcome {
    Written,
    Unsupported,
}

impl HttpFetcher {
    /// Returns `Ok(None)` when the server does not honor ranges or the data
    /// does not match the expected size, so the caller can fall back to the
    /// regular single-stream download. All partial chunks are cleaned up.
    pub(super) async fn try_download_ranged(
        &self,
        url: &Url,
        dest: &Path,
        total: u64,
        progress: ProgressSink<'_>,
    ) -> Result<Option<DownloadReceipt>, EngineError> {
        use futures_util::StreamExt;

        if total < self.limits.range_chunk_threshold || self.limits.range_chunk_size == 0 {
            return Ok(None);
        }
        // One chunk has no parallelism and only adds directory/merge work.
        if total.div_ceil(self.limits.range_chunk_size) < 2 {
            return Ok(None);
        }
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|err| EngineError::PermissionDenied(err.to_string()))?;
        }
        remove_chunks(dest).await;
        let chunk_dir = chunk_dir(dest);
        tokio::fs::create_dir_all(&chunk_dir)
            .await
            .map_err(|err| EngineError::PermissionDenied(err.to_string()))?;

        let chunk_count = total.div_ceil(self.limits.range_chunk_size) as usize;
        let received = AtomicU64::new(0);
        let outcomes: Vec<Result<ChunkOutcome, EngineError>> =
            futures_util::stream::iter(0..chunk_count)
                .map(|index| self.download_chunk(url, dest, index, total, progress, &received))
                .buffer_unordered(self.limits.range_concurrency.max(1))
                .collect()
                .await;

        let mut unsupported = false;
        for outcome in outcomes {
            match outcome {
                Ok(ChunkOutcome::Written) => {}
                Ok(ChunkOutcome::Unsupported) => unsupported = true,
                Err(error) => {
                    remove_chunks(dest).await;
                    return Err(error);
                }
            }
        }
        if unsupported {
            remove_chunks(dest).await;
            return Ok(None);
        }

        let merged = self.merge_chunks(dest, chunk_count, total).await;
        remove_chunks(dest).await;
        match merged {
            Ok(Some(written)) => Ok(Some(DownloadReceipt {
                bytes_written: written,
                resumed: false,
            })),
            Ok(None) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Concatenate chunk files into a temporary sibling and atomically rename
    /// it into place, so a crash never leaves a half-written destination.
    async fn merge_chunks(
        &self,
        dest: &Path,
        chunk_count: usize,
        total: u64,
    ) -> Result<Option<u64>, EngineError> {
        let merge_path = merge_path(dest);
        let mut output = tokio::fs::File::create(&merge_path)
            .await
            .map_err(|err| EngineError::PermissionDenied(err.to_string()))?;
        let mut written = 0_u64;
        for index in 0..chunk_count {
            let path = chunk_path(dest, index);
            let mut part = match tokio::fs::File::open(&path).await {
                Ok(part) => part,
                Err(error) => {
                    let _ = tokio::fs::remove_file(&merge_path).await;
                    return Err(EngineError::PermissionDenied(error.to_string()));
                }
            };
            match tokio::io::copy(&mut part, &mut output).await {
                Ok(bytes) => written += bytes,
                Err(error) => {
                    let _ = tokio::fs::remove_file(&merge_path).await;
                    return Err(EngineError::PermissionDenied(error.to_string()));
                }
            }
            let _ = tokio::fs::remove_file(&path).await;
        }
        if let Err(error) = output.sync_all().await {
            let _ = tokio::fs::remove_file(&merge_path).await;
            return Err(EngineError::PermissionDenied(error.to_string()));
        }
        drop(output);
        if written != total {
            let _ = tokio::fs::remove_file(&merge_path).await;
            return Ok(None);
        }
        // Windows rename does not replace an existing file.
        let _ = tokio::fs::remove_file(dest).await;
        if let Err(error) = tokio::fs::rename(&merge_path, dest).await {
            let _ = tokio::fs::remove_file(&merge_path).await;
            return Err(EngineError::PermissionDenied(error.to_string()));
        }
        Ok(Some(written))
    }

    /// Download one chunk with retries. A transient failure must not throw
    /// away the chunks that already succeeded.
    ///
    /// `received` sums the bytes written across all chunks so the caller sees
    /// one monotone progress value. A retried chunk is reported again, which
    /// can push the value past the real total by the size of the failed
    /// attempt; the caller clamps it to the planned size.
    async fn download_chunk(
        &self,
        url: &Url,
        dest: &Path,
        index: usize,
        total: u64,
        progress: ProgressSink<'_>,
        received: &AtomicU64,
    ) -> Result<ChunkOutcome, EngineError> {
        let mut attempt = 0;
        loop {
            let mut reported = 0_u64;
            let mut report = |written: u64| {
                if written <= reported {
                    return;
                }
                let delta = written - reported;
                reported = written;
                let done = received.fetch_add(delta, Ordering::Relaxed) + delta;
                if let Some(sink) = progress {
                    sink(done);
                }
            };
            match self
                .download_chunk_once(url, dest, index, total, &mut report)
                .await
            {
                Ok(outcome) => return Ok(outcome),
                Err(error) if error.retryable() && attempt < MAX_RETRIES => {
                    attempt += 1;
                    let delay = error.retry_after().unwrap_or_else(|| {
                        Duration::from_millis(RETRY_BASE_DELAY_MS << (attempt - 1))
                    });
                    tokio::time::sleep(delay).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn download_chunk_once(
        &self,
        url: &Url,
        dest: &Path,
        index: usize,
        total: u64,
        report: &mut (dyn FnMut(u64) + Send),
    ) -> Result<ChunkOutcome, EngineError> {
        use futures_util::StreamExt;
        use tokio::io::AsyncWriteExt;

        let start = self.limits.range_chunk_size.saturating_mul(index as u64);
        if start >= total {
            return Ok(ChunkOutcome::Unsupported);
        }
        let end = (start + self.limits.range_chunk_size).min(total) - 1;
        let _permit = self.permit().await?;
        let response = self
            .client
            .get(url.clone())
            .header(RANGE, format!("bytes={start}-{end}"))
            .send()
            .await
            .map_err(|err| EngineError::from(network_error(&err)))?;
        self.check_host(response.url())?;
        if let Some(error) = rate_limited(&response) {
            return Err(error);
        }
        if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            return Ok(ChunkOutcome::Unsupported);
        }

        let expected = end - start + 1;
        let path = chunk_path(dest, index);
        let mut file = tokio::fs::File::create(&path)
            .await
            .map_err(|err| EngineError::PermissionDenied(err.to_string()))?;
        let mut written = 0_u64;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|err| EngineError::from(network_error(&err)))?;
            written += chunk.len() as u64;
            if written > expected {
                drop(file);
                let _ = tokio::fs::remove_file(&path).await;
                return Ok(ChunkOutcome::Unsupported);
            }
            file.write_all(&chunk)
                .await
                .map_err(|err| EngineError::PermissionDenied(err.to_string()))?;
            report(written);
        }
        file.sync_all()
            .await
            .map_err(|err| EngineError::PermissionDenied(err.to_string()))?;
        if written != expected {
            drop(file);
            let _ = tokio::fs::remove_file(&path).await;
            return Ok(ChunkOutcome::Unsupported);
        }
        Ok(ChunkOutcome::Written)
    }
}

fn chunk_dir(dest: &Path) -> PathBuf {
    let name = dest
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("object");
    dest.with_file_name(format!(".{name}.chunks"))
}

fn chunk_path(dest: &Path, index: usize) -> PathBuf {
    chunk_dir(dest).join(format!("chunk-{index}"))
}

fn merge_path(dest: &Path) -> PathBuf {
    let name = dest
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("object");
    dest.with_file_name(format!(".{name}.merging"))
}

pub(super) async fn remove_chunks(dest: &Path) {
    let _ = tokio::fs::remove_dir_all(chunk_dir(dest)).await;
    let _ = tokio::fs::remove_file(merge_path(dest)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stale_chunk_artifacts_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("App.exe");
        let chunks = chunk_dir(&dest);
        std::fs::create_dir_all(&chunks).unwrap();
        std::fs::write(chunks.join("chunk-0"), b"stale").unwrap();
        std::fs::write(merge_path(&dest), b"stale").unwrap();

        remove_chunks(&dest).await;

        assert!(!chunks.exists());
        assert!(!merge_path(&dest).exists());
    }
}
