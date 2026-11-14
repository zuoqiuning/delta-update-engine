use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
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

/// Serialized plugin configuration, kept in the engine so the Tauri plugin and
/// offline tooling (`delta-update doctor --check-config`) validate the exact
/// same fields without depending on each other.
pub const PLUGIN_CONFIG_VERSION: u32 = 1;

/// The `plugins.delta-update` object from `tauri.conf.json`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct PluginConfig {
    pub config_version: u32,
    pub endpoint: String,
    pub signing_public_keys: Vec<String>,
    #[serde(default = "default_true")]
    pub auto_check: bool,
    /// Apply a prepared update automatically when the app exits. Disabled by
    /// default: silent file replacement must be an explicit product decision.
    #[serde(default)]
    pub auto_apply: bool,
    /// Confirm the new version as healthy after the first window loads.
    /// Disabled by default: only the application knows when its core is usable.
    #[serde(default)]
    pub auto_confirm: bool,
    #[serde(default = "default_confirm_delay")]
    pub confirm_delay_ms: u64,
    /// Optional periodic check+prepare interval in seconds. Minimum 60.
    #[serde(default)]
    pub check_interval_secs: Option<u64>,
    #[serde(default = "default_channel")]
    pub channel: String,
    pub signing_endpoint: Option<String>,
    pub allowed_hosts: Option<Vec<String>>,
    #[serde(default = "default_crash_loop_threshold")]
    pub crash_loop_threshold: u8,
    /// A normal exit after this many seconds discounts one unconfirmed start.
    /// Crashes never reach the exit hook, so this only forgives "open, look,
    /// close" sessions. Set to 0 to discount any clean exit.
    #[serde(default = "default_exit_grace_period")]
    pub exit_grace_period_secs: u64,
    pub install_dir: Option<PathBuf>,
    pub staging_dir: Option<PathBuf>,
    pub journal_dir: Option<PathBuf>,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_downloads: usize,
    pub staging_size_limit: Option<u64>,
    pub max_patch_file_size: Option<u64>,
    pub patch_memory_factor: Option<f32>,
    pub manifest_max_bytes: Option<u64>,
    pub max_files: Option<usize>,
    #[serde(default)]
    pub allow_insecure_http: bool,
    /// Optional HTTP/SOCKS proxy for enterprise networks.
    pub proxy: Option<String>,
    /// Custom headers for manifest/signature requests only. They are never sent
    /// to object downloads, which may be hosted by a third party.
    pub headers: Option<BTreeMap<String, String>>,
    /// Unknown keys, collected so `validate` can warn about typos instead of
    /// silently ignoring them.
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            config_version: PLUGIN_CONFIG_VERSION,
            endpoint: String::new(),
            signing_public_keys: Vec::new(),
            auto_check: true,
            auto_apply: false,
            auto_confirm: false,
            confirm_delay_ms: default_confirm_delay(),
            check_interval_secs: None,
            channel: default_channel(),
            signing_endpoint: None,
            allowed_hosts: None,
            crash_loop_threshold: default_crash_loop_threshold(),
            exit_grace_period_secs: default_exit_grace_period(),
            install_dir: None,
            staging_dir: None,
            journal_dir: None,
            max_concurrent_downloads: default_max_concurrent(),
            staging_size_limit: None,
            max_patch_file_size: None,
            patch_memory_factor: None,
            manifest_max_bytes: None,
            max_files: None,
            allow_insecure_http: false,
            proxy: None,
            headers: None,
            extra: BTreeMap::new(),
        }
    }
}

impl PluginConfig {
    /// Parse and validate the `plugins.delta-update` value from `tauri.conf.json`.
    ///
    /// Validation is offline: `install_dir` is not required to exist, so CI can
    /// check a project on a machine that has not installed the app.
    pub fn from_json(value: &serde_json::Value) -> Result<Self, EngineError> {
        let config: Self = serde_json::from_value(value.clone())
            .map_err(|error| EngineError::ConfigInvalid(format!("config parse: {error}")))?;
        config.validate_offline()?;
        Ok(config)
    }

    /// Full runtime validation, including that `install_dir` exists.
    pub fn validate(&self) -> Result<(), EngineError> {
        self.validate_offline()?;
        if let Some(install) = &self.install_dir {
            if !install.is_dir() {
                return Err(EngineError::ConfigInvalid(format!(
                    "install_dir does not exist: {}",
                    install.display()
                )));
            }
        }
        Ok(())
    }

    /// Validation that does not touch the filesystem, so CI can check a config
    /// on a machine that has not installed the app.
    pub fn validate_offline(&self) -> Result<(), EngineError> {
        if self.config_version < PLUGIN_CONFIG_VERSION {
            return Err(EngineError::ConfigMigrationRequired {
                from: self.config_version,
                to: PLUGIN_CONFIG_VERSION,
            });
        }
        if self.config_version > PLUGIN_CONFIG_VERSION {
            return Err(EngineError::ConfigInvalid(format!(
                "config_version {} is newer than the supported version {PLUGIN_CONFIG_VERSION}",
                self.config_version
            )));
        }
        if self.endpoint.is_empty() {
            return Err(EngineError::ConfigInvalid("endpoint is required".into()));
        }
        let endpoint = parse_endpoint("endpoint", &self.endpoint)?;
        if !is_allowed_endpoint(&endpoint, self.allow_insecure_http) {
            return Err(EngineError::EndpointInsecure(endpoint.to_string()));
        }
        if let Some(value) = &self.signing_endpoint {
            let signing = parse_endpoint("signing_endpoint", value)?;
            if !is_allowed_endpoint(&signing, self.allow_insecure_http) {
                return Err(EngineError::EndpointInsecure(signing.to_string()));
            }
        } else if endpoint.query().is_some() {
            return Err(EngineError::ConfigInvalid(
                "a presigned endpoint requires signing_endpoint".into(),
            ));
        }
        if self.signing_public_keys.is_empty() {
            return Err(EngineError::ConfigInvalid(
                "signing_public_keys must contain at least one key".into(),
            ));
        }
        for key in &self.signing_public_keys {
            crate::crypto::validate_public_key(key)?;
        }
        for (name, path) in [
            ("install_dir", self.install_dir.as_ref()),
            ("staging_dir", self.staging_dir.as_ref()),
            ("journal_dir", self.journal_dir.as_ref()),
        ] {
            if let Some(path) = path {
                if !path.is_absolute() {
                    return Err(EngineError::ConfigInvalid(format!(
                        "{name} must be an absolute path: {}",
                        path.display()
                    )));
                }
            }
        }
        if let Some(install) = &self.install_dir {
            for (name, path) in [
                ("staging_dir", self.staging_dir.as_ref()),
                ("journal_dir", self.journal_dir.as_ref()),
            ] {
                if let Some(path) = path {
                    if path.starts_with(install) {
                        return Err(EngineError::ConfigInvalid(format!(
                            "{name} ({}) must not be inside install_dir ({})",
                            path.display(),
                            install.display()
                        )));
                    }
                }
            }
        }
        if let Some(hosts) = &self.allowed_hosts {
            if hosts.iter().any(|host| host.trim().is_empty()) {
                return Err(EngineError::ConfigInvalid(
                    "allowed_hosts must not contain empty entries".into(),
                ));
            }
        }
        if self.channel.trim().is_empty() || self.channel.chars().any(char::is_whitespace) {
            return Err(EngineError::ConfigInvalid("channel is invalid".into()));
        }
        if self.max_concurrent_downloads == 0 {
            return Err(EngineError::ConfigInvalid(
                "max_concurrent_downloads must be at least 1".into(),
            ));
        }
        if self.crash_loop_threshold == 0 {
            return Err(EngineError::ConfigInvalid(
                "crash_loop_threshold must be at least 1".into(),
            ));
        }
        if let Some(interval) = self.check_interval_secs {
            if interval < 60 {
                return Err(EngineError::ConfigInvalid(
                    "check_interval_secs must be at least 60".into(),
                ));
            }
        }
        if self
            .proxy
            .as_deref()
            .is_some_and(|proxy| proxy.trim().is_empty())
        {
            return Err(EngineError::ConfigInvalid("proxy must not be empty".into()));
        }
        for (name, value) in [
            ("staging_size_limit", self.staging_size_limit),
            ("max_patch_file_size", self.max_patch_file_size),
            ("manifest_max_bytes", self.manifest_max_bytes),
        ] {
            if value == Some(0) {
                return Err(EngineError::ConfigInvalid(format!(
                    "{name} must be positive"
                )));
            }
        }
        if self.max_files == Some(0) {
            return Err(EngineError::ConfigInvalid(
                "max_files must be positive".into(),
            ));
        }
        if let Some(factor) = self.patch_memory_factor {
            if !factor.is_finite() || factor < 1.0 {
                return Err(EngineError::ConfigInvalid(
                    "patch_memory_factor must be a finite value >= 1.0".into(),
                ));
            }
        }
        for key in self.extra.keys() {
            if key == "key_rotation" {
                tracing::warn!("key_rotation is not implemented and will be ignored");
            } else {
                tracing::warn!(%key, "unknown delta-update config field");
            }
        }
        Ok(())
    }
}

fn parse_endpoint(name: &str, value: &str) -> Result<Url, EngineError> {
    Url::parse(value)
        .map_err(|err| EngineError::ConfigInvalid(format!("{name} is not a valid URL: {err}")))
}

fn default_true() -> bool {
    true
}

fn default_confirm_delay() -> u64 {
    3_000
}

fn default_channel() -> String {
    "stable".to_string()
}

fn default_crash_loop_threshold() -> u8 {
    3
}

fn default_exit_grace_period() -> u64 {
    10
}

fn default_max_concurrent() -> usize {
    4
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
