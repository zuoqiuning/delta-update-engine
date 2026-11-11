use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::Deserialize;
use url::Url;

use crate::config::is_allowed_endpoint;
use crate::error::{EngineError, NetworkError};

/// Requested signed-URL lifetime. The service may return a shorter one.
pub const SIGNED_URL_TTL_SECONDS: u64 = 900;

/// Refresh a signed URL this long before it actually expires.
pub const REFRESH_MARGIN: Duration = Duration::from_secs(60);

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
}

impl HttpSigningService {
    pub fn new(
        client: reqwest::Client,
        endpoint: Url,
        allow_insecure_http: bool,
    ) -> Result<Self, EngineError> {
        if !is_allowed_endpoint(&endpoint, allow_insecure_http) {
            return Err(EngineError::EndpointInsecure(endpoint.to_string()));
        }
        Ok(Self {
            client,
            endpoint,
            allow_insecure_http,
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
        let response = self
            .client
            .post(self.endpoint.clone())
            .json(&body)
            .send()
            .await
            .map_err(|err| signing_service(err.to_string()))?;
        if !response.status().is_success() {
            return Err(signing_service(format!(
                "signing service returned {}",
                response.status()
            )));
        }
        let payload: SigningResponse = response
            .json()
            .await
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
