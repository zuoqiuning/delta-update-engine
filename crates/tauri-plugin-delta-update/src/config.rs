use std::collections::BTreeMap;
use std::path::PathBuf;

use delta_update_engine::config::is_allowed_endpoint;
use delta_update_engine::EngineError;
use serde::{Deserialize, Serialize};
use url::Url;

pub const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct Config {
    pub config_version: u32,
    pub endpoint: String,
    pub signing_public_keys: Vec<String>,
    #[serde(default = "default_true")]
    pub auto_check: bool,
    #[serde(default = "default_true")]
    pub auto_apply: bool,
    #[serde(default = "default_true")]
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

impl Default for Config {
    fn default() -> Self {
        Self {
            config_version: 1,
            endpoint: String::new(),
            signing_public_keys: Vec::new(),
            auto_check: true,
            auto_apply: true,
            auto_confirm: true,
            confirm_delay_ms: default_confirm_delay(),
            check_interval_secs: None,
            channel: default_channel(),
            signing_endpoint: None,
            allowed_hosts: None,
            crash_loop_threshold: default_crash_loop_threshold(),
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

impl Config {
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.config_version < CONFIG_VERSION {
            return Err(EngineError::ConfigMigrationRequired {
                from: self.config_version,
                to: CONFIG_VERSION,
            });
        }
        if self.config_version > CONFIG_VERSION {
            return Err(EngineError::ConfigInvalid(format!(
                "config_version {} is newer than the supported version {CONFIG_VERSION}",
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
            delta_update_engine::crypto::validate_public_key(key)?;
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
            if !install.is_dir() {
                return Err(EngineError::ConfigInvalid(format!(
                    "install_dir does not exist: {}",
                    install.display()
                )));
            }
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
    2
}

fn default_max_concurrent() -> usize {
    4
}
