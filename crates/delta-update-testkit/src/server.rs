use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::response::Response;
use axum::Router;
use futures_util::{stream, StreamExt};

#[derive(Default)]
struct MockState {
    files: HashMap<String, Vec<u8>>,
    fail_5xx: Mutex<HashMap<String, u32>>,
    forbidden: Mutex<HashMap<String, u32>>,
    cut_at: Mutex<HashMap<String, usize>>,
    requests: Mutex<Vec<String>>,
    headers: Mutex<Vec<(String, HashMap<String, String>)>>,
    ranges: Mutex<Vec<(String, String)>>,
    range_supported: AtomicBool,
    signing: AtomicBool,
    presign_expired: Mutex<u32>,
    base: String,
    active: AtomicUsize,
    max_active: AtomicUsize,
    delay_ms: AtomicU64,
}

/// In-process HTTP fixture for check/prepare tests.
///
/// Serves a fixed key/value object map and can inject 5xx or 403 responses a
/// configurable number of times before succeeding. Range requests return 206
/// with `Content-Range`; responses can be delayed to observe concurrency, and
/// the server can act as a presigned-URL service.
pub struct MockServer {
    addr: SocketAddr,
    state: Arc<MockState>,
    task: tokio::task::JoinHandle<()>,
}

impl MockServer {
    pub async fn start<I, K, V>(files: I) -> std::io::Result<Self>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<Vec<u8>>,
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let state = Arc::new(MockState {
            files: files
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
            range_supported: AtomicBool::new(true),
            base: format!("http://{addr}"),
            ..MockState::default()
        });
        let app = Router::new().fallback(serve).with_state(Arc::clone(&state));
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self { addr, state, task })
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url(), path.trim_start_matches('/'))
    }

    /// Return `500` the next `count` times this path is requested.
    pub fn fail_next(&self, path: &str, count: u32) {
        lock(&self.state.fail_5xx).insert(normalize(path), count);
    }

    /// Return `403` the next `count` times this path is requested.
    pub fn forbid_next(&self, path: &str, count: u32) {
        lock(&self.state.forbidden).insert(normalize(path), count);
    }

    /// Paths requested so far, in request order. Used to prove that a delta
    /// update never falls back to full objects.
    pub fn requests(&self) -> Vec<String> {
        lock(&self.state.requests).clone()
    }

    /// Raw `Range` headers seen so far as `(path, header)` pairs.
    pub fn ranges(&self) -> Vec<(String, String)> {
        lock(&self.state.ranges).clone()
    }

    /// Toggle Range support for subsequent requests.
    pub fn set_range_supported(&self, supported: bool) {
        self.state
            .range_supported
            .store(supported, Ordering::Relaxed);
    }

    /// Serve at most `at` bytes of the whole-object response once, then let the
    /// client resume with a Range request.
    pub fn cut_next(&self, path: &str, at: usize) {
        lock(&self.state.cut_at).insert(normalize(path), at);
    }

    /// Accept `POST /any` as a presigned-URL service returning URLs for the
    /// requested keys.
    pub fn enable_signing(&self) {
        self.state.signing.store(true, Ordering::Relaxed);
    }

    /// Hand out this many signed URLs with `sig=expired` (403 on GET) before
    /// switching to valid ones.
    pub fn expire_presigned_next(&self, count: u32) {
        *lock(&self.state.presign_expired) = count;
    }

    /// Delay every response by this many milliseconds, making overlapping
    /// requests observable.
    pub fn set_delay(&self, millis: u64) {
        self.state.delay_ms.store(millis, Ordering::Relaxed);
    }

    /// Peak number of requests handled at the same time.
    pub fn max_concurrent(&self) -> usize {
        self.state.max_active.load(Ordering::SeqCst)
    }

    /// Last value of a header seen on a request for `path`, lowercase name.
    pub fn last_header(&self, path: &str, name: &str) -> Option<String> {
        let path = normalize(path);
        let name = name.to_ascii_lowercase();
        lock(&self.state.headers)
            .iter()
            .rev()
            .find(|(request_path, _)| *request_path == path)
            .and_then(|(_, headers)| headers.get(&name).cloned())
    }

    pub fn reset_max_concurrent(&self) {
        self.state.max_active.store(0, Ordering::SeqCst);
    }
}

struct ActiveGuard<'a>(&'a MockState);

impl<'a> ActiveGuard<'a> {
    fn new(state: &'a MockState) -> Self {
        let active = state.active.fetch_add(1, Ordering::SeqCst) + 1;
        state.max_active.fetch_max(active, Ordering::SeqCst);
        Self(state)
    }
}

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(State(state): State<Arc<MockState>>, request: Request) -> Response {
    let _active = ActiveGuard::new(&state);
    let path = normalize(request.uri().path());
    lock(&state.requests).push(path.clone());
    let captured: HashMap<String, String> = request
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect();
    lock(&state.headers).push((path.clone(), captured));
    if let Some(range) = request
        .headers()
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
    {
        lock(&state.ranges).push((path.clone(), range.to_string()));
    }
    let delay = state.delay_ms.load(Ordering::Relaxed);
    if delay > 0 {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    if request.method() == Method::POST && state.signing.load(Ordering::Relaxed) {
        return signing_response(&state, request).await;
    }
    if request
        .uri()
        .query()
        .is_some_and(|query| query.split('&').any(|part| part == "sig=expired"))
    {
        return response(StatusCode::FORBIDDEN, Vec::new(), None);
    }
    if take(&state.fail_5xx, &path) > 0 {
        return response(StatusCode::INTERNAL_SERVER_ERROR, Vec::new(), None);
    }
    if take(&state.forbidden, &path) > 0 {
        return response(StatusCode::FORBIDDEN, Vec::new(), None);
    }
    let Some(body) = state.files.get(&path).cloned() else {
        return response(StatusCode::NOT_FOUND, Vec::new(), None);
    };

    // Announce the full length but close the body early once, so the client
    // sees a retryable transport error and resumes with a Range request.
    if let Some(at) = take_cut(&state.cut_at, &path) {
        let prefix = body[..at.min(body.len())].to_vec();
        return truncated_response(prefix, body.len() as u64);
    }

    let range = if state.range_supported.load(Ordering::Relaxed) {
        request
            .headers()
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| parse_range(value, body.len()))
    } else {
        None
    };

    match range {
        Some((start, end)) => response(
            StatusCode::PARTIAL_CONTENT,
            body[start..=end].to_vec(),
            Some(format!("bytes {start}-{end}/{}", body.len())),
        ),
        None => response(StatusCode::OK, body, None),
    }
}

async fn signing_response(state: &MockState, request: Request) -> Response {
    let body = axum::body::to_bytes(request.into_body(), 64 * 1024)
        .await
        .unwrap_or_default();
    let keys: Vec<String> = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| value.get("keys")?.as_array().cloned())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let mut urls = serde_json::Map::new();
    for key in keys {
        let expired = {
            let mut remaining = lock(&state.presign_expired);
            if *remaining > 0 {
                *remaining -= 1;
                true
            } else {
                false
            }
        };
        let suffix = if expired { "expired" } else { "ok" };
        urls.insert(
            key.clone(),
            serde_json::Value::String(format!("{}/{}?sig={suffix}", state.base, key)),
        );
    }
    let payload = serde_json::json!({
        "urls": urls,
        "expires_at": "2100-01-01T00:00:00Z",
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload.to_string()))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn truncated_response(body: Vec<u8>, declared_len: u64) -> Response {
    let prefix = Bytes::from(body);
    let chunks = stream::once(async move { Ok(prefix) }).chain(stream::once(async move {
        // Give the client a chance to consume the prefix before the reset.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        Err(std::io::Error::other("cut"))
    }));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_LENGTH, declared_len)
        .body(Body::from_stream(chunks))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn response(status: StatusCode, body: Vec<u8>, content_range: Option<String>) -> Response {
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_LENGTH, body.len() as u64);
    if let Some(range) = content_range {
        if let Ok(value) = HeaderValue::from_str(&range) {
            builder = builder.header(header::CONTENT_RANGE, value);
        }
    }
    builder
        .body(Body::from(body))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

/// Parse `bytes=<start>-` and return an inclusive range clamped to the body.
fn parse_range(value: &str, len: usize) -> Option<(usize, usize)> {
    let start = value
        .strip_prefix("bytes=")?
        .split('-')
        .next()?
        .parse()
        .ok()?;
    if start >= len {
        return None;
    }
    Some((start, len - 1))
}

fn normalize(path: &str) -> String {
    path.trim_start_matches('/').to_string()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn take_cut(map: &Mutex<HashMap<String, usize>>, path: &str) -> Option<usize> {
    lock(map).remove(path)
}

fn take(map: &Mutex<HashMap<String, u32>>, path: &str) -> u32 {
    let mut map = lock(map);
    match map.get_mut(path) {
        Some(remaining) if *remaining > 0 => {
            *remaining -= 1;
            1
        }
        _ => 0,
    }
}
