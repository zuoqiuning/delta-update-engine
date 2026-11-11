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
    async fn download(
        &self,
        key: &str,
        dest: &Path,
        resume_from: u64,
        max_bytes: u64,
    ) -> Result<DownloadReceipt, EngineError>;
}

#[derive(Debug, Clone, Copy)]
pub struct DownloadReceipt {
    pub bytes_written: u64,
    pub resumed: bool,
}
