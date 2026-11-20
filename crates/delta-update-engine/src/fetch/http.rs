use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use reqwest::header::{HeaderMap, ETAG, IF_NONE_MATCH, RANGE};
use url::Url;

use crate::config::is_allowed_endpoint;
use crate::error::{EngineError, NetworkError, PatchError, SignatureError};

use super::resume::resume_offset;
use super::signing::{HttpSigningService, ResolvedUrl, SignedUrlProvider, SIGNED_URL_TTL_SECONDS};
use super::{DownloadReceipt, Fetcher};

const MAX_RETRIES: u32 = 3;
const MAX_REFRESHES: u32 = 3;
const RETRY_BASE_DELAY_MS: u64 = 200;

mod ranged;
mod support;

use ranged::remove_chunks;
use support::{
    build_headers, has_expired, is_expired_status, lock, needs_refresh, network_error, rate_limited,
};

/// Live byte progress for one object. `Some` only when the caller asked for it.
type ProgressSink<'a> = Option<&'a (dyn Fn(u64) + Send + Sync)>;

#[derive(Debug, Clone, Copy)]
pub struct FetchLimits {
    pub manifest_max_bytes: u64,
    pub signature_max_bytes: u64,
    pub object_max_bytes: u64,
    /// Objects at least this large are fetched as parallel Range chunks.
    pub range_chunk_threshold: u64,
    pub range_chunk_size: u64,
    pub range_concurrency: usize,
    /// Upper bound on concurrent HTTP requests in this fetcher (file-level and
    /// Range-level downloads combined). Keeps a large update from opening
    /// `files × ranges` sockets at once.
    pub request_concurrency: usize,
}

impl Default for FetchLimits {
    fn default() -> Self {
        Self {
            manifest_max_bytes: 8 * 1024 * 1024,
            signature_max_bytes: 64 * 1024,
            object_max_bytes: 4 * 1024 * 1024 * 1024,
            range_chunk_threshold: 8 * 1024 * 1024,
            range_chunk_size: 8 * 1024 * 1024,
            range_concurrency: 4,
            request_concurrency: 8,
        }
    }
}

#[derive(Debug, Clone)]
struct CachedUrl {
    url: Url,
    expires_at: SystemTime,
}

/// Last manifest body and its `ETag`, so a `check` against an unchanged
/// channel pointer costs one 304 instead of the whole document.
#[derive(Debug, Clone)]
struct CachedBody {
    etag: String,
    body: Vec<u8>,
}

pub struct HttpFetcher {
    client: reqwest::Client,
    endpoint: Url,
    manifest_key: String,
    signing: Option<Arc<dyn SignedUrlProvider>>,
    signing_endpoint: Option<Url>,
    cache: Mutex<HashMap<String, CachedUrl>>,
    limits: FetchLimits,
    allowed_hosts: Option<Vec<String>>,
    allow_insecure_http: bool,
    /// Applied only to manifest/signature requests, never to object downloads.
    headers: HeaderMap,
    /// Bounds concurrent requests; see [`FetchLimits::request_concurrency`].
    permits: Arc<tokio::sync::Semaphore>,
    manifest_cache: Mutex<Option<CachedBody>>,
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
        let redirect_hosts = allowed_hosts.clone();
        builder = builder.redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 5 {
                return attempt.error(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "too many redirects",
                ));
            }
            if !is_allowed_endpoint(attempt.url(), allow_insecure_http) {
                return attempt.error(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "redirect target is not an allowed HTTPS endpoint",
                ));
            }
            if let Some(hosts) = &redirect_hosts {
                let host = attempt.url().host_str().unwrap_or_default().to_string();
                if !hosts
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(&host))
                {
                    return attempt.error(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!("redirect host is not allowed: {host}"),
                    ));
                }
            }
            attempt.follow()
        }));
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
        let signing_endpoint_url = signing_endpoint.clone();
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
            signing_endpoint: signing_endpoint_url,
            cache: Mutex::new(HashMap::new()),
            // `Copy` limits are captured by value; the semaphore is the only
            // shared resource.
            permits: Arc::new(tokio::sync::Semaphore::new(
                limits.request_concurrency.max(1),
            )),
            manifest_cache: Mutex::new(None),
            limits,
            allowed_hosts,
            allow_insecure_http,
            headers,
        })
    }

    /// Send extra headers with signed-URL requests only.
    ///
    /// Manifest/signature headers are deliberately not reused here: the signing
    /// service can live on a different host, and forwarding an Authorization
    /// header meant for the object store would leak it. A deployment that puts
    /// its signing service behind an auth proxy configures `signing_headers`.
    pub fn with_signing_headers(
        mut self,
        headers: Vec<(String, String)>,
    ) -> Result<Self, EngineError> {
        let headers = build_headers(headers)?;
        if let Some(endpoint) = self.signing_endpoint.clone() {
            self.signing = Some(Arc::new(HttpSigningService::new_with_headers(
                self.client.clone(),
                endpoint,
                self.allow_insecure_http,
                headers,
            )?));
        }
        Ok(self)
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
        cache: Option<&Mutex<Option<CachedBody>>>,
    ) -> Result<Vec<u8>, EngineError> {
        let mut attempt = 0;
        let mut refreshes = 0;
        loop {
            let url = self.resolve(key).await?;
            let cached = cache.and_then(|slot| lock(slot).clone());
            match self
                .try_get_limited(url, max_bytes, kind, cached.as_ref())
                .await
            {
                Ok((bytes, etag)) => {
                    // Only a body with an entity tag can be revalidated; a
                    // server without ETag support simply keeps no cache.
                    if let (Some(slot), Some(etag)) = (cache, etag) {
                        *lock(slot) = Some(CachedBody {
                            etag,
                            body: bytes.clone(),
                        });
                    }
                    return Ok(bytes);
                }
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
        cached: Option<&CachedBody>,
    ) -> Result<(Vec<u8>, Option<String>), EngineError> {
        let _permit = self.permit().await?;
        let mut request = self.client.get(url);
        for (name, value) in &self.headers {
            request = request.header(name.clone(), value.clone());
        }
        if let Some(cached) = cached {
            request = request.header(IF_NONE_MATCH, &cached.etag);
        }
        let response = request
            .send()
            .await
            .map_err(|err| EngineError::from(network_error(&err)))?;
        self.check_host(response.url())?;
        if response.status() == reqwest::StatusCode::NOT_MODIFIED {
            return match cached {
                Some(cached) => Ok((cached.body.clone(), Some(cached.etag.clone()))),
                None => Err(NetworkError::Http4xx {
                    status: 304,
                    detail: "server answered 304 without a cached copy".into(),
                }
                .into()),
            };
        }
        let etag = response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
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
        Ok((bytes.to_vec(), etag))
    }

    async fn permit(&self) -> Result<tokio::sync::SemaphorePermit<'_>, EngineError> {
        self.permits
            .acquire()
            .await
            .map_err(|_| EngineError::Internal("request semaphore is closed".into()))
    }

    /// Single-stream download with resume support (fallback when Range chunks
    /// are unsupported or the object is too small to benefit).
    async fn try_download(
        &self,
        url: Url,
        dest: &Path,
        resume_from: u64,
        max_bytes: u64,
        progress: ProgressSink<'_>,
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

        let _permit = self.permit().await?;
        let response = request
            .send()
            .await
            .map_err(|err| EngineError::from(network_error(&err)))?;
        self.check_host(response.url())?;
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
            if let Some(sink) = progress {
                sink(total);
            }
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

#[async_trait]
impl Fetcher for HttpFetcher {
    async fn fetch_manifest(&self) -> Result<Vec<u8>, EngineError> {
        let key = self.manifest_key.clone();
        self.get_limited(
            &key,
            self.limits.manifest_max_bytes,
            LimitedKind::Manifest,
            Some(&self.manifest_cache),
        )
        .await
    }

    async fn fetch_signature(&self) -> Result<Vec<u8>, EngineError> {
        let key = format!("{}.minisig", self.manifest_key);
        self.get_limited(
            &key,
            self.limits.signature_max_bytes,
            LimitedKind::Signature,
            None,
        )
        .await
        .map_err(|error| match error {
            EngineError::Network(NetworkError::Http4xx { status: 404, .. }) => {
                EngineError::Signature(SignatureError::Missing)
            }
            other => other,
        })
    }

    async fn fetch_attachment(&self, key_id: &str) -> Result<Option<Vec<u8>>, EngineError> {
        if !crate::crypto::is_key_id(key_id) {
            return Ok(None);
        }
        let key = format!("{}.minisig.{key_id}", self.manifest_key);
        match self
            .get_limited(
                &key,
                self.limits.signature_max_bytes,
                LimitedKind::Signature,
                None,
            )
            .await
        {
            Ok(bytes) => Ok(Some(bytes)),
            // A declared attachment may not be uploaded yet; that must not turn
            // into a failure when the primary signature could still pass.
            Err(EngineError::Network(NetworkError::Http4xx { status: 404, .. }))
            | Err(EngineError::Signature(SignatureError::Missing)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn fetch_envelope(&self) -> Result<Option<Vec<u8>>, EngineError> {
        // Only the channel pointer has an envelope; a versioned manifest URL is
        // used by tooling and never gets one.
        let key = if self.manifest_key == "latest.json" {
            "latest.envelope.json".to_string()
        } else if let Some((dir, _)) = self.manifest_key.rsplit_once('/') {
            if !self.manifest_key.ends_with("/latest.json") {
                return Ok(None);
            }
            format!("{dir}/latest.envelope.json")
        } else {
            return Ok(None);
        };
        match self
            .get_limited(
                &key,
                self.limits.manifest_max_bytes,
                LimitedKind::Manifest,
                None,
            )
            .await
        {
            Ok(bytes) => Ok(Some(bytes)),
            Err(EngineError::Network(NetworkError::Http4xx { status: 404, .. }))
            | Err(EngineError::Signature(SignatureError::Missing)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn download(
        &self,
        key: &str,
        dest: &Path,
        resume_from: u64,
        max_bytes: u64,
    ) -> Result<DownloadReceipt, EngineError> {
        self.download_inner(key, dest, resume_from, max_bytes, None)
            .await
    }

    async fn download_with_progress(
        &self,
        key: &str,
        dest: &Path,
        resume_from: u64,
        max_bytes: u64,
        on_progress: &(dyn Fn(u64) + Send + Sync),
    ) -> Result<DownloadReceipt, EngineError> {
        self.download_inner(key, dest, resume_from, max_bytes, Some(on_progress))
            .await
    }
}

impl HttpFetcher {
    async fn download_inner(
        &self,
        key: &str,
        dest: &Path,
        resume_from: u64,
        max_bytes: u64,
        progress: ProgressSink<'_>,
    ) -> Result<DownloadReceipt, EngineError> {
        let max_bytes = max_bytes.min(self.limits.object_max_bytes);
        // A crash during a ranged download can leave a stale chunk directory or
        // merge temp file behind. Clear them before any download strategy runs.
        remove_chunks(dest).await;
        let mut resume = resume_from;
        let mut refreshes = 0;
        loop {
            let url = self.resolve(key).await?;
            if resume == 0 {
                match self
                    .try_download_ranged(&url, dest, max_bytes, progress)
                    .await
                {
                    Ok(Some(receipt)) => return Ok(receipt),
                    Ok(None) => {}
                    Err(error) if error.retryable() => {
                        tracing::debug!(key, %error, "ranged download failed; falling back to a single stream");
                    }
                    Err(error) if self.signing.is_some() && is_expired_status(&error) => {
                        tracing::warn!(key, %error, "signed URL expired during ranged download; retrying as a single stream");
                        self.invalidate(key);
                    }
                    Err(error) => return Err(error),
                }
            }
            match self
                .try_download(url, dest, resume, max_bytes, progress)
                .await
            {
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
