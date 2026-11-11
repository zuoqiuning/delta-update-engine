use std::path::PathBuf;
use std::sync::Arc;

use url::Url;

use crate::error::EngineError;
use crate::fetch::Fetcher;
use crate::limits::Limits;

/// Construction-time configuration. No network I/O happens here.
#[derive(Clone)]
pub struct EngineConfig {
    pub endpoint: Url,
    pub signing_public_keys: Vec<String>,
    pub channel: String,
    /// Version of the running binary, taken from the package metadata.
    pub current_version: String,
    pub install_root: PathBuf,
    pub staging_root: PathBuf,
    pub journal_root: PathBuf,
    pub limits: Limits,
    pub fetcher: Arc<dyn Fetcher>,
    /// Development escape hatch: allow plain HTTP only on loopback hosts.
    pub allow_insecure_http: bool,
    /// Number of unconfirmed starts before a committed update is rolled back.
    pub crash_loop_threshold: u32,
}

impl EngineConfig {
    /// Validate fields that must hold before any request is made.
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.signing_public_keys.is_empty() {
            return Err(EngineError::ConfigInvalid(
                "signing_public_keys must contain at least one key".into(),
            ));
        }
        if self.channel.trim().is_empty() || self.channel.chars().any(char::is_whitespace) {
            return Err(EngineError::ConfigInvalid("channel is invalid".into()));
        }
        semver::Version::parse(&self.current_version)
            .map_err(|_| EngineError::VersionInvalid(self.current_version.clone()))?;
        if !is_allowed_endpoint(&self.endpoint, self.allow_insecure_http) {
            return Err(EngineError::EndpointInsecure(self.endpoint.to_string()));
        }
        for (name, path) in [
            ("install_root", &self.install_root),
            ("staging_root", &self.staging_root),
            ("journal_root", &self.journal_root),
        ] {
            if !path.is_absolute() {
                return Err(EngineError::ConfigInvalid(format!(
                    "{name} must be an absolute path: {}",
                    path.display()
                )));
            }
        }
        for (name, path) in [
            ("staging_root", &self.staging_root),
            ("journal_root", &self.journal_root),
        ] {
            if path.starts_with(&self.install_root) {
                return Err(EngineError::ConfigInvalid(format!(
                    "{name} ({}) must not be inside install_root ({})",
                    path.display(),
                    self.install_root.display()
                )));
            }
        }
        if self.crash_loop_threshold == 0 {
            return Err(EngineError::ConfigInvalid(
                "crash_loop_threshold must be at least 1".into(),
            ));
        }
        Ok(())
    }
}

/// HTTPS is required everywhere; plain HTTP is allowed only for loopback
/// development endpoints and only when explicitly enabled.
pub fn is_allowed_endpoint(url: &Url, allow_insecure_http: bool) -> bool {
    match url.scheme() {
        "https" => true,
        "http" if allow_insecure_http => url.host_str().is_some_and(is_loopback_host),
        _ => false,
    }
}

pub(crate) fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

impl std::fmt::Debug for EngineConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineConfig")
            .field("endpoint", &self.endpoint)
            .field("channel", &self.channel)
            .field("current_version", &self.current_version)
            .field("install_root", &self.install_root)
            .field("staging_root", &self.staging_root)
            .field("journal_root", &self.journal_root)
            .field("limits", &self.limits)
            .field("allow_insecure_http", &self.allow_insecure_http)
            .field("crash_loop_threshold", &self.crash_loop_threshold)
            .finish_non_exhaustive()
    }
}
