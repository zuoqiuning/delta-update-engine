use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, RANGE};
use url::Url;

use crate::config::is_allowed_endpoint;
use crate::error::{EngineError, NetworkError, PatchError, SignatureError};

use super::resume::resume_offset;
use super::signing::{
    HttpSigningService, ResolvedUrl, SignedUrlProvider, REFRESH_MARGIN, SIGNED_URL_TTL_SECONDS,
};
use super::{DownloadReceipt, Fetcher};

const MAX_RETRIES: u32 = 3;
const MAX_REFRESHES: u32 = 3;
const RETRY_BASE_DELAY_MS: u64 = 200;
/// Objects at least this large are fetched as parallel Range chunks.
const RANGE_CHUNK_THRESHOLD: u64 = 8 * 1024 * 1024;
const RANGE_CHUNK_SIZE: u64 = 8 * 1024 * 1024;
const RANGE_CONCURRENCY: usize = 4;

#[derive(Debug, Clone, Copy)]
pub struct FetchLimits {
    pub manifest_max_bytes: u64,
    pub signature_max_bytes: u64,
    pub object_max_bytes: u64,
}

impl Default for FetchLimits {
    fn default() -> Self {
        Self {
            manifest_max_bytes: 8 * 1024 * 1024,
            signature_max_bytes: 64 * 1024,
            object_max_bytes: 4 * 1024 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone)]
struct CachedUrl {
    url: Url,
    expires_at: SystemTime,
}

pub struct HttpFetcher {
    client: reqwest::Client,
    endpoint: Url,
    manifest_key: String,
    signing: Option<Arc<dyn SignedUrlProvider>>,
    cache: Mutex<HashMap<String, CachedUrl>>,
    limits: FetchLimits,
    allowed_hosts: Option<Vec<String>>,
    allow_insecure_http: bool,
    /// Applied only to manifest/signature requests, never to object downloads.
    headers: HeaderMap,
}

impl HttpFetcher {
    /// Build a fetcher with the workspace's standard client policy: 10s
    /// connect timeout, 30s per-read timeout (large objects stay streamable),
    /// and no total request timeout.
    pub fn new_default(
        endpoint: Url,
        signing_endpoint: Option<Url>,
        limits: FetchLimits,
        allowed_hosts: Option<Vec<String>>,
        allow_insecure_http: bool,
    ) -> Result<Self, EngineError> {
        Self::new_with_proxy(
            endpoint,
            signing_endpoint,
            limits,
            allowed_hosts,
            allow_insecure_http,
            None,
        )
    }

    /// Same policy as [`Self::new_default`], with an optional HTTP/SOCKS proxy
    /// for enterprise networks. The proxy URL is validated by `reqwest`.
    pub fn new_with_proxy(
        endpoint: Url,
        signing_endpoint: Option<Url>,
        limits: FetchLimits,
        allowed_hosts: Option<Vec<String>>,
        allow_insecure_http: bool,
        proxy: Option<&str>,
    ) -> Result<Self, EngineError> {
        Self::new_with_options(
            endpoint,
            signing_endpoint,
            limits,
            allowed_hosts,
            allow_insecure_http,
            proxy,
            Vec::new(),
        )
    }

    /// Same as [`Self::new_with_proxy`], plus custom headers applied to the
    /// manifest and signature requests. Object downloads intentionally do not
    /// receive these headers: they may live on a third-party object host.
    pub fn new_with_options(
        endpoint: Url,
        signing_endpoint: Option<Url>,
        limits: FetchLimits,
        allowed_hosts: Option<Vec<String>>,
        allow_insecure_http: bool,
        proxy: Option<&str>,
        headers: Vec<(String, String)>,
    ) -> Result<Self, EngineError> {
        let headers = build_headers(headers)?;
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30))
            .user_agent(concat!("delta-update/", env!("CARGO_PKG_VERSION")));
        if let Some(proxy) = proxy {
            let proxy = reqwest::Proxy::all(proxy)
                .map_err(|err| EngineError::ConfigInvalid(format!("proxy: {err}")))?;
            builder = builder.proxy(proxy);
        }
        let client = builder
            .build()
            .map_err(|err| EngineError::ConfigInvalid(format!("http client: {err}")))?;
        Self::from_client(
            client,
            endpoint,
            signing_endpoint,
            limits,
            allowed_hosts,
            allow_insecure_http,
            headers,
        )
    }

    /// `signing_endpoint` is the presigned-URL service described in the
    /// protocol, not the signature object itself. Without it the signature URL
    /// is derived from the endpoint origin.
    pub fn new(
        client: reqwest::Client,
        endpoint: Url,
        signing_endpoint: Option<Url>,
        limits: FetchLimits,
        allowed_hosts: Option<Vec<String>>,
        allow_insecure_http: bool,
    ) -> Result<Self, EngineError> {
        Self::from_client(
            client,
            endpoint,
            signing_endpoint,
            limits,
            allowed_hosts,
            allow_insecure_http,
            HeaderMap::new(),
        )
    }

    fn from_client(
        client: reqwest::Client,
        endpoint: Url,
        signing_endpoint: Option<Url>,
        limits: FetchLimits,
        allowed_hosts: Option<Vec<String>>,
        allow_insecure_http: bool,
        headers: HeaderMap,
    ) -> Result<Self, EngineError> {
        if !is_allowed_endpoint(&endpoint, allow_insecure_http) {
            return Err(EngineError::EndpointInsecure(endpoint.to_string()));
        }
        let manifest_key = endpoint.path().trim_start_matches('/').to_string();
        let signing = match signing_endpoint {
            Some(url) => {
                if manifest_key.is_empty() {
                    return Err(EngineError::ConfigInvalid(
                        "endpoint must include the manifest key path".into(),
                    ));
                }
                let service = HttpSigningService::new(client.clone(), url, allow_insecure_http)?;
                Some(Arc::new(service) as Arc<dyn SignedUrlProvider>)
            }
            None => {
                if endpoint.query().is_some() {
                    return Err(EngineError::ConfigInvalid(
                        "a presigned endpoint requires signing_endpoint".into(),
                    ));
                }
                None
            }
        };

        Ok(Self {
            client,
            endpoint,
            manifest_key,
            signing,
            cache: Mutex::new(HashMap::new()),
            limits,
            allowed_hosts,
            allow_insecure_http,
            headers,
        })
    }

    /// Resolve an object key to a download URL, refreshing signed URLs before
    /// they expire and falling back to a still-valid URL when refresh fails.
    async fn resolve(&self, key: &str) -> Result<Url, EngineError> {
        let Some(signing) = &self.signing else {
            return if key == self.manifest_key {
                Ok(self.endpoint.clone())
            } else {
                self.origin_url(key)
            };
        };

        if let Some(cached) = self.cached(key) {
            if !needs_refresh(&cached) {
                return Ok(cached.url);
            }
            match signing.resolve(key, SIGNED_URL_TTL_SECONDS).await {
                Ok(resolved) => {
                    self.check_host(&resolved.url)?;
                    self.store(key, resolved.clone());
                    return Ok(resolved.url);
                }
                Err(error) if !has_expired(&cached) => {
                    tracing::warn!(key, %error, "signed URL refresh failed; reusing the current URL");
                    return Ok(cached.url);
                }
                Err(error) => return Err(error),
            }
        }

        let resolved = signing.resolve(key, SIGNED_URL_TTL_SECONDS).await?;
        self.check_host(&resolved.url)?;
        self.store(key, resolved.clone());
        Ok(resolved.url)
    }

    fn cached(&self, key: &str) -> Option<CachedUrl> {
        lock(&self.cache).get(key).cloned()
    }

    fn store(&self, key: &str, resolved: ResolvedUrl) {
        lock(&self.cache).insert(
            key.to_string(),
            CachedUrl {
                url: resolved.url,
                expires_at: resolved.expires_at,
            },
        );
    }

    fn invalidate(&self, key: &str) {
        lock(&self.cache).remove(key);
    }

    fn origin_url(&self, key: &str) -> Result<Url, EngineError> {
        crate::path::validate_rel_path(key)?;
        let mut base = self.endpoint.clone();
        base.set_path("/");
        base.set_query(None);
        let url = base
            .join(key)
            .map_err(|err| EngineError::Internal(format!("invalid object key: {err}")))?;
        self.check_host(&url)?;
        Ok(url)
    }

    fn check_host(&self, url: &Url) -> Result<(), EngineError> {
        if !is_allowed_endpoint(url, self.allow_insecure_http) {
            return Err(EngineError::EndpointInsecure(url.to_string()));
        }
        if let Some(hosts) = &self.allowed_hosts {
            let host = url.host_str().unwrap_or_default();
            if !hosts
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(host))
            {
                return Err(EngineError::ConfigInvalid(format!(
                    "host is not in allowed_hosts: {host}"
                )));
            }
        }
        Ok(())
    }

    async fn get_limited(
        &self,
        key: &str,
        max_bytes: u64,
        kind: LimitedKind,
    ) -> Result<Vec<u8>, EngineError> {
        let mut attempt = 0;
        let mut refreshes = 0;
        loop {
            let url = self.resolve(key).await?;
            match self.try_get_limited(url, max_bytes, kind).await {
                Ok(bytes) => return Ok(bytes),
                Err(error) if self.signing.is_some() && is_expired_status(&error) => {
                    if refreshes == MAX_REFRESHES {
                        return Err(NetworkError::PresignExpired.into());
                    }
                    refreshes += 1;
                    self.invalidate(key);
                }
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

    async fn try_get_limited(
        &self,
        url: Url,
        max_bytes: u64,
        kind: LimitedKind,
    ) -> Result<Vec<u8>, EngineError> {
        let mut request = self.client.get(url);
        for (name, value) in &self.headers {
            request = request.header(name.clone(), value.clone());
        }
        let response = request
            .send()
            .await
            .map_err(|err| EngineError::from(network_error(&err)))?;
        if let Some(error) = rate_limited(&response) {
            return Err(error);
        }
        let response = response
            .error_for_status()
            .map_err(|err| EngineError::from(network_error(&err)))?;

        if let Some(length) = response.content_length() {
            if length > max_bytes {
                return Err(kind.too_large(length, max_bytes));
            }
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|err| EngineError::from(network_error(&err)))?;
        if bytes.len() as u64 > max_bytes {
            return Err(kind.too_large(bytes.len() as u64, max_bytes));
        }
        Ok(bytes.to_vec())
    }

    /// Download a large object as parallel Range chunks, then concatenate.
    ///
    /// Returns `Ok(None)` when the server does not honor ranges or the data
    /// does not match the expected size, so the caller can fall back to the
    /// regular single-stream download. All partial chunks are cleaned up.
    async fn try_download_ranged(
        &self,
        url: &Url,
        dest: &Path,
        total: u64,
    ) -> Result<Option<DownloadReceipt>, EngineError> {
        use futures_util::StreamExt;

        if total < RANGE_CHUNK_THRESHOLD {
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

        let chunk_count = total.div_ceil(RANGE_CHUNK_SIZE) as usize;
        let outcomes: Vec<Result<ChunkOutcome, EngineError>> =
            futures_util::stream::iter(0..chunk_count)
                .map(|index| self.download_chunk(url, dest, index, total))
                .buffer_unordered(RANGE_CONCURRENCY)
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

        let mut output = tokio::fs::File::create(dest)
            .await
            .map_err(|err| EngineError::PermissionDenied(err.to_string()))?;
        let mut written = 0_u64;
        for index in 0..chunk_count {
            let path = chunk_path(dest, index);
            let mut part = tokio::fs::File::open(&path)
                .await
                .map_err(|err| EngineError::PermissionDenied(err.to_string()))?;
            written += tokio::io::copy(&mut part, &mut output)
                .await
                .map_err(|err| EngineError::PermissionDenied(err.to_string()))?;
            let _ = tokio::fs::remove_file(&path).await;
        }
        output
            .sync_all()
            .await
            .map_err(|err| EngineError::PermissionDenied(err.to_string()))?;
        drop(output);
        let _ = tokio::fs::remove_dir(&chunk_dir).await;

        if written != total {
            remove_chunks(dest).await;
            return Ok(None);
        }
        Ok(Some(DownloadReceipt {
            bytes_written: written,
            resumed: false,
        }))
    }

    async fn download_chunk(
        &self,
        url: &Url,
        dest: &Path,
        index: usize,
        total: u64,
    ) -> Result<ChunkOutcome, EngineError> {
        use futures_util::StreamExt;
        use tokio::io::AsyncWriteExt;

        let start = RANGE_CHUNK_SIZE.saturating_mul(index as u64);
        if start >= total {
            return Ok(ChunkOutcome::Unsupported);
        }
        let end = (start + RANGE_CHUNK_SIZE).min(total) - 1;
        let response = self
            .client
            .get(url.clone())
            .header(RANGE, format!("bytes={start}-{end}"))
            .send()
            .await
            .map_err(|err| EngineError::from(network_error(&err)))?;
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

    async fn try_download(
        &self,
        url: Url,
        dest: &Path,
        resume_from: u64,
        max_bytes: u64,
    ) -> Result<DownloadReceipt, EngineError> {
        use futures_util::StreamExt;
        use tokio::io::AsyncWriteExt;

        // Resume only when the bytes already on disk are exactly the prefix we
        // asked the server for; otherwise a stale partial file would corrupt
        // the result.
        let existing = resume_offset(dest).await;
        let resume_from = if existing == resume_from {
            resume_from
        } else {
            0
        };

        let mut request = self.client.get(url);
        if resume_from > 0 {
            request = request.header(RANGE, format!("bytes={resume_from}-"));
        }

        let response = request
            .send()
            .await
            .map_err(|err| EngineError::from(network_error(&err)))?;
        if let Some(error) = rate_limited(&response) {
            return Err(error);
        }
        let response = response
            .error_for_status()
            .map_err(|err| EngineError::from(network_error(&err)))?;

        let resumed = resume_from > 0 && response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|err| EngineError::PermissionDenied(err.to_string()))?;
        }
        let mut file = if resumed {
            tokio::fs::OpenOptions::new()
                .append(true)
                .open(dest)
                .await
                .map_err(|err| EngineError::PermissionDenied(err.to_string()))?
        } else {
            tokio::fs::File::create(dest)
                .await
                .map_err(|err| EngineError::PermissionDenied(err.to_string()))?
        };

        let mut written = 0_u64;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|err| EngineError::from(network_error(&err)))?;
            written += chunk.len() as u64;
            let total = if resumed {
                resume_from + written
            } else {
                written
            };
            if total > max_bytes {
                drop(file);
                let _ = tokio::fs::remove_file(dest).await;
                return Err(EngineError::PatchFailed(PatchError::TooLarge(format!(
                    "object exceeds the {max_bytes} byte limit"
                ))));
            }
            file.write_all(&chunk)
                .await
                .map_err(|err| EngineError::PermissionDenied(err.to_string()))?;
        }
        file.flush()
            .await
            .map_err(|err| EngineError::PermissionDenied(err.to_string()))?;

        Ok(DownloadReceipt {
            bytes_written: written,
            resumed,
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum LimitedKind {
    Manifest,
    Signature,
}

impl LimitedKind {
    fn too_large(self, size: u64, max: u64) -> EngineError {
        match self {
            Self::Manifest => {
                EngineError::ManifestTooLarge(format!("manifest is {size} bytes, limit is {max}"))
            }
            Self::Signature => EngineError::Signature(SignatureError::Invalid(format!(
                "signature is {size} bytes, limit is {max}"
            ))),
        }
    }
}

enum ChunkOutcome {
    Written,
    Unsupported,
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

async fn remove_chunks(dest: &Path) {
    let _ = tokio::fs::remove_dir_all(chunk_dir(dest)).await;
}

fn rate_limited(response: &reqwest::Response) -> Option<EngineError> {
    if response.status() != reqwest::StatusCode::TOO_MANY_REQUESTS {
        return None;
    }
    let retry_after_secs = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok());
    Some(NetworkError::RateLimited { retry_after_secs }.into())
}

fn network_error(err: &reqwest::Error) -> NetworkError {
    if err.is_timeout() {
        return NetworkError::Timeout(err.to_string());
    }
    if let Some(status) = err.status() {
        let code = status.as_u16();
        return if status.is_server_error() {
            NetworkError::Http5xx {
                status: code,
                detail: err.to_string(),
            }
        } else {
            NetworkError::Http4xx {
                status: code,
                detail: err.to_string(),
            }
        };
    }
    NetworkError::Connect(err.to_string())
}

fn is_expired_status(error: &EngineError) -> bool {
    matches!(
        error,
        EngineError::Network(NetworkError::Http4xx { status: 403, .. })
    )
}

fn needs_refresh(cached: &CachedUrl) -> bool {
    match cached.expires_at.duration_since(SystemTime::now()) {
        Ok(remaining) => remaining <= REFRESH_MARGIN,
        Err(_) => true,
    }
}

fn has_expired(cached: &CachedUrl) -> bool {
    SystemTime::now() >= cached.expires_at
}

fn build_headers(headers: Vec<(String, String)>) -> Result<HeaderMap, EngineError> {
    let mut map = HeaderMap::new();
    for (name, value) in headers {
        let name = HeaderName::from_bytes(name.trim().as_bytes())
            .map_err(|err| EngineError::ConfigInvalid(format!("invalid header name: {err}")))?;
        let value = HeaderValue::from_str(&value)
            .map_err(|err| EngineError::ConfigInvalid(format!("invalid header value: {err}")))?;
        map.insert(name, value);
    }
    Ok(map)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[async_trait]
impl Fetcher for HttpFetcher {
    async fn fetch_manifest(&self) -> Result<Vec<u8>, EngineError> {
        let key = self.manifest_key.clone();
        self.get_limited(&key, self.limits.manifest_max_bytes, LimitedKind::Manifest)
            .await
    }

    async fn fetch_signature(&self) -> Result<Vec<u8>, EngineError> {
        let key = format!("{}.minisig", self.manifest_key);
        self.get_limited(
            &key,
            self.limits.signature_max_bytes,
            LimitedKind::Signature,
        )
        .await
        .map_err(|error| match error {
            EngineError::Network(NetworkError::Http4xx { status: 404, .. }) => {
                EngineError::Signature(SignatureError::Missing)
            }
            other => other,
        })
    }

    async fn download(
        &self,
        key: &str,
        dest: &Path,
        resume_from: u64,
        max_bytes: u64,
    ) -> Result<DownloadReceipt, EngineError> {
        let max_bytes = max_bytes.min(self.limits.object_max_bytes);
        let mut resume = resume_from;
        let mut refreshes = 0;
        loop {
            let url = self.resolve(key).await?;
            if resume == 0 {
                match self.try_download_ranged(&url, dest, max_bytes).await {
                    Ok(Some(receipt)) => return Ok(receipt),
                    Ok(None) => {}
                    Err(error) if error.retryable() => {
                        tracing::debug!(key, %error, "ranged download failed; falling back to a single stream");
                    }
                    Err(error) => return Err(error),
                }
            }
            match self.try_download(url, dest, resume, max_bytes).await {
                Ok(receipt) => return Ok(receipt),
                Err(error) if self.signing.is_some() && is_expired_status(&error) => {
                    if refreshes == MAX_REFRESHES {
                        return Err(NetworkError::PresignExpired.into());
                    }
                    refreshes += 1;
                    self.invalidate(key);
                    // The failed request never touched the partial file.
                    resume = resume_offset(dest).await;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_headers_are_validated_and_stored() {
        let url: Url = "https://example.com/latest.json".parse().unwrap();
        let error = match HttpFetcher::new_with_options(
            url.clone(),
            None,
            FetchLimits::default(),
            None,
            false,
            None,
            vec![("bad header".into(), "x".into())],
        ) {
            Ok(_) => panic!("expected invalid header error"),
            Err(error) => error,
        };
        assert_eq!(error.code(), "E_CONFIG_INVALID");

        let fetcher = HttpFetcher::new_with_options(
            url,
            None,
            FetchLimits::default(),
            None,
            false,
            None,
            vec![("authorization".into(), "Bearer test".into())],
        )
        .unwrap();
        assert_eq!(fetcher.headers.get("authorization").unwrap(), "Bearer test");
    }
}
