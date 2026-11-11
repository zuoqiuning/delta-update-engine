use std::collections::HashSet;

use crate::error::EngineError;
use crate::limits::Limits;
use crate::path::validate_rel_path;

use super::model::Manifest;

pub const SUPPORTED_SCHEMA: u32 = 1;
pub const MAX_NOTES_BYTES: usize = 64 * 1024;

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

    let mut seen = HashSet::new();
    for (path, entry) in &manifest.files {
        validate_rel_path(path)?;
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
    }

    Ok(manifest)
}
