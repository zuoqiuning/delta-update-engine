use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use reqwest::header::HeaderMap;
use serde::Deserialize;
use url::Url;

use crate::config::is_allowed_endpoint;
use crate::error::{EngineError, NetworkError};

/// Requested signed-URL lifetime. The service may return a shorter one.
pub const SIGNED_URL_TTL_SECONDS: u64 = 900;

/// Refresh a signed URL this long before it actually expires.
pub const REFRESH_MARGIN: Duration = Duration::from_secs(60);

/// A signing response only contains a small key→URL map. Anything larger is a
/// broken or hostile service, not a real release, so the body is capped before
/// JSON parsing.
pub const MAX_SIGNING_RESPONSE_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone)]
pub struct ResolvedUrl {
    pub url: Url,
    pub expires_at: SystemTime,
}

/// Presigned-URL service. It only returns URLs; verification always happens in
/// the client, so a compromised service cannot inject unverified bytes.
#[async_trait]
pub trait SignedUrlProvider: Send + Sync {
    async fn resolve(&self, key: &str, ttl_seconds: u64) -> Result<ResolvedUrl, EngineError>;
}

#[derive(Debug, Deserialize)]
struct SigningResponse {
    urls: HashMap<String, String>,
    expires_at: String,
}

#[derive(Debug, Clone)]
pub struct HttpSigningService {
    client: reqwest::Client,
    endpoint: Url,
    allow_insecure_http: bool,
    /// Sent only to the signing service, never to object hosts. Enterprise
    /// deployments often put this service behind their own auth proxy.
    headers: HeaderMap,
}

impl HttpSigningService {
    pub fn new(
        client: reqwest::Client,
        endpoint: Url,
        allow_insecure_http: bool,
    ) -> Result<Self, EngineError> {
        Self::new_with_headers(client, endpoint, allow_insecure_http, HeaderMap::new())
    }

    pub fn new_with_headers(
        client: reqwest::Client,
        endpoint: Url,
        allow_insecure_http: bool,
        headers: HeaderMap,
    ) -> Result<Self, EngineError> {
        if !is_allowed_endpoint(&endpoint, allow_insecure_http) {
            return Err(EngineError::EndpointInsecure(endpoint.to_string()));
        }
        Ok(Self {
            client,
            endpoint,
            allow_insecure_http,
            headers,
        })
    }
}

#[async_trait]
impl SignedUrlProvider for HttpSigningService {
    async fn resolve(&self, key: &str, ttl_seconds: u64) -> Result<ResolvedUrl, EngineError> {
        let body = serde_json::json!({
            "keys": [key],
            "ttl_seconds": ttl_seconds.min(SIGNED_URL_TTL_SECONDS),
        });
        let mut request = self.client.post(self.endpoint.clone()).json(&body);
        for (name, value) in &self.headers {
            request = request.header(name.clone(), value.clone());
        }
        let response = request
            .send()
            .await
            .map_err(|err| signing_service(err.to_string()))?;
        if !response.status().is_success() {
            return Err(signing_service(format!(
                "signing service returned {}",
                response.status()
            )));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_SIGNING_RESPONSE_BYTES)
        {
            return Err(signing_service(format!(
                "signing response exceeds the {MAX_SIGNING_RESPONSE_BYTES} byte limit"
            )));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|err| signing_service(format!("read signing response: {err}")))?;
        if bytes.len() as u64 > MAX_SIGNING_RESPONSE_BYTES {
            return Err(signing_service(format!(
                "signing response exceeds the {MAX_SIGNING_RESPONSE_BYTES} byte limit"
            )));
        }
        let payload: SigningResponse = serde_json::from_slice(&bytes)
            .map_err(|err| signing_service(format!("invalid signing response: {err}")))?;
        let raw = payload
            .urls
            .get(key)
            .ok_or_else(|| signing_service(format!("signing response is missing {key}")))?;
        let url =
            Url::parse(raw).map_err(|err| signing_service(format!("invalid signed URL: {err}")))?;
        if !is_allowed_endpoint(&url, self.allow_insecure_http) {
            return Err(signing_service(
                "signed URL is not allowed by the transport policy".into(),
            ));
        }
        Ok(ResolvedUrl {
            url,
            expires_at: parse_expiry(&payload.expires_at)?,
        })
    }
}

fn parse_expiry(value: &str) -> Result<SystemTime, EngineError> {
    let parsed = time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .map_err(|err| signing_service(format!("invalid expires_at: {err}")))?;
    let seconds = u64::try_from(parsed.unix_timestamp()).unwrap_or(0);
    Ok(UNIX_EPOCH + Duration::from_secs(seconds))
}

fn signing_service(message: String) -> EngineError {
    EngineError::Network(NetworkError::SigningService(message))
}
