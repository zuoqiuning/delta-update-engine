use std::collections::HashSet;

use crate::crypto::is_key_id;
use crate::error::EngineError;
use crate::limits::Limits;
use crate::path::{validate_install_path, validate_rel_path};

use super::model::Manifest;

pub const SUPPORTED_SCHEMA: u32 = 1;
pub const MAX_NOTES_BYTES: usize = 64 * 1024;
/// Bounds the extra signature fetches a check can trigger.
pub const MAX_SIGNING_KEY_IDS: usize = 8;
/// Bounds the number of baseline patches one file may list.
pub const MAX_DELTAS_PER_FILE: usize = 16;

/// Parse and validate a manifest from its raw bytes.
///
/// Signature verification must happen before this function is called.
pub fn parse_manifest(raw: &[u8], limits: &Limits) -> Result<Manifest, EngineError> {
    if raw.len() as u64 > limits.manifest_max_bytes {
        return Err(EngineError::ManifestTooLarge(format!(
            "{} bytes exceeds the {} byte limit",
            raw.len(),
            limits.manifest_max_bytes
        )));
    }

    let manifest: Manifest = serde_json::from_slice(raw)
        .map_err(|err| EngineError::ManifestInvalid(format!("invalid JSON: {err}")))?;

    if manifest.schema > SUPPORTED_SCHEMA {
        return Err(EngineError::ManifestSchemaTooNew {
            client: SUPPORTED_SCHEMA,
            required: manifest.schema,
        });
    }
    if manifest.min_client_schema > SUPPORTED_SCHEMA {
        return Err(EngineError::ManifestSchemaTooNew {
            client: SUPPORTED_SCHEMA,
            required: manifest.min_client_schema,
        });
    }
    if semver::Version::parse(&manifest.version).is_err() {
        return Err(EngineError::VersionInvalid(manifest.version.clone()));
    }
    if let Some(min) = &manifest.min_supported_version {
        let min_version = semver::Version::parse(min).map_err(|_| {
            EngineError::ManifestInvalid(format!("min_supported_version {min} is not SemVer"))
        })?;
        let version = semver::Version::parse(&manifest.version)
            .map_err(|_| EngineError::VersionInvalid(manifest.version.clone()))?;
        if min_version > version {
            return Err(EngineError::ManifestInvalid(
                "min_supported_version must not exceed version".into(),
            ));
        }
    }
    if manifest
        .notes
        .as_ref()
        .is_some_and(|notes| notes.len() > MAX_NOTES_BYTES)
    {
        return Err(EngineError::ManifestInvalid(format!(
            "release notes exceed the {MAX_NOTES_BYTES} byte limit"
        )));
    }
    if manifest.files.len() > limits.max_files {
        return Err(EngineError::ManifestTooLarge(format!(
            "{} files exceeds the {} file limit",
            manifest.files.len(),
            limits.max_files
        )));
    }
    if manifest.signing_key_ids.len() > MAX_SIGNING_KEY_IDS {
        return Err(EngineError::ManifestInvalid(format!(
            "signing_key_ids exceeds the {MAX_SIGNING_KEY_IDS} entry limit"
        )));
    }
    let mut seen_ids = HashSet::new();
    for key_id in &manifest.signing_key_ids {
        if !is_key_id(key_id) {
            return Err(EngineError::ManifestInvalid(format!(
                "signing_key_ids entry is not a 16-char lowercase hex id: {key_id}"
            )));
        }
        if !seen_ids.insert(key_id) {
            return Err(EngineError::ManifestInvalid(format!(
                "duplicate signing_key_ids entry: {key_id}"
            )));
        }
    }

    let mut seen = HashSet::new();
    for (path, entry) in &manifest.files {
        validate_install_path(path)?;
        let key = path.to_ascii_lowercase();
        if !seen.insert(key) {
            return Err(EngineError::PathCollision(path.clone()));
        }

        if entry.sha256.len() != 64
            || !entry
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(EngineError::ManifestInvalid(format!(
                "{path}: sha256 must be 64 lowercase hex characters"
            )));
        }
        if entry.size > limits.max_full_bytes {
            return Err(EngineError::ManifestInvalid(format!(
                "{path}: file exceeds the full object limit"
            )));
        }
        if let Some(key) = &entry.delta_key {
            validate_rel_path(key)?;
        }
        if let Some(key) = &entry.full_key {
            validate_rel_path(key)?;
        }
        if entry.strategy.is_full() {
            if entry.delta_key.is_some() || entry.delta_size.is_some() {
                return Err(EngineError::ManifestInvalid(format!(
                    "{path}: full strategy must not carry delta_key or delta_size"
                )));
            }
        } else {
            if entry.delta_key.is_none() || entry.delta_size.is_none() {
                return Err(EngineError::ManifestInvalid(format!(
                    "{path}: patch strategy requires delta_key and delta_size"
                )));
            }
            if entry.delta_size.unwrap_or(0) > limits.max_patch_bytes {
                return Err(EngineError::ManifestInvalid(format!(
                    "{path}: patch exceeds the patch object limit"
                )));
            }
        }

        if entry.deltas.len() > MAX_DELTAS_PER_FILE {
            return Err(EngineError::ManifestInvalid(format!(
                "{path}: more than {MAX_DELTAS_PER_FILE} baseline patches"
            )));
        }
        let mut baselines = HashSet::new();
        for delta in &entry.deltas {
            semver::Version::parse(&delta.from_version).map_err(|_| {
                EngineError::ManifestInvalid(format!(
                    "{path}: delta from_version is not SemVer: {}",
                    delta.from_version
                ))
            })?;
            if !baselines.insert(delta.from_version.as_str()) {
                return Err(EngineError::ManifestInvalid(format!(
                    "{path}: duplicate delta for version {}",
                    delta.from_version
                )));
            }
            validate_rel_path(&delta.delta_key)?;
            if delta.strategy.is_full() {
                return Err(EngineError::ManifestInvalid(format!(
                    "{path}: a delta entry cannot use the full strategy"
                )));
            }
            if delta.delta_size == 0 || delta.delta_size > limits.max_patch_bytes {
                return Err(EngineError::ManifestInvalid(format!(
                    "{path}: delta for {} exceeds the patch object limit",
                    delta.from_version
                )));
            }
        }
    }

    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn with_key_ids(ids: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schema": 1,
            "version": "2.0.0",
            "min_client_schema": 1,
            "files": {
                "a.txt": { "sha256": "a".repeat(64), "size": 1, "strategy": "full" }
            },
            "signing_key_ids": ids,
        }))
        .unwrap()
    }

    fn with_deltas(deltas: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schema": 1,
            "version": "2.1.0",
            "min_client_schema": 1,
            "files": {
                "a.txt": {
                    "sha256": "a".repeat(64),
                    "size": 10,
                    "strategy": "full",
                    "full_key": "blobs/aaa",
                    "deltas": deltas
                }
            }
        }))
        .unwrap()
    }

    fn delta(from: &str, key: &str, size: u64, strategy: &str) -> serde_json::Value {
        json!({
            "from_version": from,
            "strategy": strategy,
            "delta_key": key,
            "delta_size": size,
        })
    }

    #[test]
    fn accepts_several_baselines_per_file() {
        let raw = with_deltas(json!([
            delta(
                "1.0.0",
                "artifacts/1.0.0-to-2.1.0/a.txt.tdp",
                5,
                "qbsdiff+zstd"
            ),
            delta(
                "2.0.0",
                "artifacts/2.0.0-to-2.1.0/a.txt.tdp",
                3,
                "qbsdiff+zstd"
            ),
        ]));
        let parsed = parse_manifest(&raw, &Limits::default()).unwrap();
        assert_eq!(parsed.files["a.txt"].deltas.len(), 2);
    }

    #[test]
    fn rejects_malformed_baselines() {
        for deltas in [
            json!([delta("nope", "artifacts/x/a.tdp", 5, "qbsdiff+zstd")]),
            json!([
                delta("1.0.0", "artifacts/1.0.0-to-2.1.0/a.tdp", 5, "qbsdiff+zstd"),
                delta("1.0.0", "artifacts/1.0.0-to-2.1.0/b.tdp", 4, "qbsdiff+zstd"),
            ]),
            json!([delta("1.0.0", "artifacts/1.0.0-to-2.1.0/a.tdp", 5, "full")]),
            json!([delta(
                "1.0.0",
                "artifacts/1.0.0-to-2.1.0/a.tdp",
                0,
                "qbsdiff+zstd"
            )]),
            json!([delta("1.0.0", "../escape.tdp", 5, "qbsdiff+zstd")]),
        ] {
            let error = parse_manifest(&with_deltas(deltas), &Limits::default()).unwrap_err();
            assert!(
                matches!(
                    error.code(),
                    "E_MANIFEST_PARSE" | "E_PATH_INVALID" | "E_PATH_ESCAPE"
                ),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn accepts_lowercase_hex_key_ids() {
        let parsed = parse_manifest(
            &with_key_ids(json!(["0123456789abcdef"])),
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(parsed.signing_key_ids, ["0123456789abcdef"]);
    }

    #[test]
    fn rejects_malformed_or_duplicate_key_ids() {
        for ids in [
            json!(["ABCDEF0123456789"]),
            json!(["short"]),
            json!(["0123456789abcdef", "0123456789abcdef"]),
        ] {
            let error = parse_manifest(&with_key_ids(ids), &Limits::default()).unwrap_err();
            assert_eq!(error.code(), "E_MANIFEST_PARSE");
        }
    }
}
