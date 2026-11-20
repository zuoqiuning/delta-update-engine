//! Transport abstraction. The engine never depends on a concrete HTTP client.

use std::path::Path;

use async_trait::async_trait;

use crate::error::EngineError;

pub mod http;
pub mod resume;
pub mod signing;

pub use http::{FetchLimits, HttpFetcher};
pub use signing::{HttpSigningService, ResolvedUrl, SignedUrlProvider};

#[async_trait]
pub trait Fetcher: Send + Sync {
    async fn fetch_manifest(&self) -> Result<Vec<u8>, EngineError>;
    async fn fetch_signature(&self) -> Result<Vec<u8>, EngineError>;

    /// Additional detached signature `.minisig.<key_id>`, when the manifest
    /// declares one. `Ok(None)` means "not published or not supported"; the
    /// default keeps external fetchers working exactly like 1.0.
    async fn fetch_attachment(&self, _key_id: &str) -> Result<Option<Vec<u8>>, EngineError> {
        Ok(None)
    }

    /// Single-object envelope carrying the manifest bytes and its signature.
    ///
    /// `Ok(None)` means the publisher does not provide it (or this fetcher
    /// cannot reach it); the engine then falls back to `latest.json` plus
    /// `.minisig`, which has a short mismatch window during publishing.
    async fn fetch_envelope(&self) -> Result<Option<Vec<u8>>, EngineError> {
        Ok(None)
    }

    /// Download `key` into `dest`, resuming at `resume_from` bytes.
    async fn download(
        &self,
        key: &str,
        dest: &Path,
        resume_from: u64,
        max_bytes: u64,
    ) -> Result<DownloadReceipt, EngineError>;

    /// Same as [`Self::download`], but reports how many bytes of `dest` are
    /// written so far.
    ///
    /// `on_progress` is called from the download task and must be cheap and
    /// non-blocking. The default implementation ignores it, so fetchers that
    /// cannot report progress keep working unchanged.
    async fn download_with_progress(
        &self,
        key: &str,
        dest: &Path,
        resume_from: u64,
        max_bytes: u64,
        _on_progress: &(dyn Fn(u64) + Send + Sync),
    ) -> Result<DownloadReceipt, EngineError> {
        self.download(key, dest, resume_from, max_bytes).await
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DownloadReceipt {
    pub bytes_written: u64,
    pub resumed: bool,
}
