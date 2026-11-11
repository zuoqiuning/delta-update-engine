use std::collections::BTreeMap;

use anyhow::Context;
use serde::Serialize;

pub const SCHEMA: u32 = 1;
pub const MIN_CLIENT_SCHEMA: u32 = 1;

#[derive(Debug, Clone, Serialize)]
pub struct ManifestFile {
    pub sha256: String,
    pub size: u64,
    pub strategy: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_key: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutputManifest {
    pub schema: u32,
    pub version: String,
    pub channel: String,
    pub published_at: String,
    pub min_client_schema: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub mandatory: bool,
    pub files: BTreeMap<String, ManifestFile>,
    #[serde(skip_serializing_if = "is_false")]
    pub rollback: bool,
}

pub struct ManifestInput<'a> {
    pub version: &'a str,
    pub channel: &'a str,
    pub rollback: bool,
    pub notes: Option<&'a str>,
    pub mandatory: bool,
    pub files: BTreeMap<String, ManifestFile>,
}

/// Build deterministic manifest bytes. The signature covers exactly these
/// bytes, so file ordering must not depend on the filesystem walk order.
pub fn build(input: ManifestInput<'_>) -> anyhow::Result<Vec<u8>> {
    let manifest = OutputManifest {
        schema: SCHEMA,
        version: input.version.to_string(),
        channel: input.channel.to_string(),
        published_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        min_client_schema: MIN_CLIENT_SCHEMA,
        notes: input.notes.map(str::to_string),
        mandatory: input.mandatory,
        files: input.files,
        rollback: input.rollback,
    };
    serde_json::to_vec_pretty(&manifest).context("serialize manifest")
}

pub fn version_key(prefix: &str, version: &str) -> String {
    prefixed(prefix, &format!("manifests/{version}.json"))
}

pub fn latest_key(prefix: &str) -> String {
    prefixed(prefix, "manifests/latest.json")
}

pub fn signature_key(manifest_key: &str) -> String {
    format!("{manifest_key}.minisig")
}

pub fn prefixed(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}/{key}")
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_is_deterministic_and_omits_rollback_false() {
        let mut files = BTreeMap::new();
        files.insert(
            "b.txt".to_string(),
            ManifestFile {
                sha256: "b".repeat(64),
                size: 2,
                strategy: "full".into(),
                delta_key: None,
                delta_size: None,
                full_key: Some("artifacts/full/1.0.0/b.txt".into()),
            },
        );
        files.insert(
            "a.txt".to_string(),
            ManifestFile {
                sha256: "a".repeat(64),
                size: 1,
                strategy: "full".into(),
                delta_key: None,
                delta_size: None,
                full_key: None,
            },
        );
        let first = build(ManifestInput {
            version: "1.0.0",
            channel: "stable",
            rollback: false,
            notes: None,
            mandatory: false,
            files,
        })
        .unwrap();
        let text = String::from_utf8(first).unwrap();
        assert!(!text.contains("rollback"));
        assert!(!text.contains("mandatory"));
        assert!(text.find("a.txt").unwrap() < text.find("b.txt").unwrap());
    }
}
