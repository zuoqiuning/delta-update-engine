//! Small helpers shared by the fetch paths: error classification, signed URL
//! freshness and header validation.

use std::sync::{Mutex, MutexGuard};
use std::time::SystemTime;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

use crate::error::{EngineError, NetworkError};
use crate::fetch::signing::REFRESH_MARGIN;

use super::CachedUrl;

pub(super) fn rate_limited(response: &reqwest::Response) -> Option<EngineError> {
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

pub(super) fn network_error(err: &reqwest::Error) -> NetworkError {
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
    NetworkError::Connect(describe_connect_failure(err))
}

/// `reqwest` collapses DNS, TLS, proxy and connection-refused failures into one
/// type. Keep the stable `E_NETWORK_CONNECT` code, but prefix the message with
/// the layer that failed so support can tell them apart.
fn describe_connect_failure(err: &reqwest::Error) -> String {
    let mut chain = vec![err.to_string()];
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        chain.push(cause.to_string());
        source = cause.source();
    }
    let text = chain.join(" ");
    let lower = text.to_lowercase();
    if lower.contains("dns")
        || lower.contains("name or service")
        || lower.contains("failed to lookup address")
    {
        format!("DNS resolution failed: {text}")
    } else if lower.contains("tls") || lower.contains("certificate") || lower.contains("handshake")
    {
        format!("TLS handshake failed: {text}")
    } else if lower.contains("proxy") {
        format!("proxy connection failed: {text}")
    } else {
        text
    }
}

pub(super) fn is_expired_status(error: &EngineError) -> bool {
    matches!(
        error,
        EngineError::Network(NetworkError::Http4xx { status: 403, .. })
    )
}

pub(super) fn needs_refresh(cached: &CachedUrl) -> bool {
    match cached.expires_at.duration_since(SystemTime::now()) {
        Ok(remaining) => remaining <= REFRESH_MARGIN,
        Err(_) => true,
    }
}

pub(super) fn has_expired(cached: &CachedUrl) -> bool {
    SystemTime::now() >= cached.expires_at
}

pub(super) fn build_headers(headers: Vec<(String, String)>) -> Result<HeaderMap, EngineError> {
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

pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
