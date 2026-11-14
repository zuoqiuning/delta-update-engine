use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::EngineError;

pub const APPLY_JSON: &str = "apply.json";
pub const COMMIT_STATUS: &str = "commit.status";
pub const COMMIT_META: &str = "commit.meta.json";
pub const PREPARED_JSON: &str = "prepared.json";
pub const OLD_HASHES: &str = "old_hashes.json";
pub const VERIFIED_MANIFEST: &str = "verified_manifest.json";
pub const VERIFIED_SIGNATURE: &str = "verified_manifest.json.minisig";
pub const INSTALLED_MANIFEST: &str = "installed_manifest.json";
pub const INSTALLED_SIGNATURE: &str = "installed_manifest.json.minisig";
pub const ATTEMPTS: &str = "attempts";
/// Threshold recorded at the last startup so offline diagnostics can say how
/// close this machine is to a rollback without reading `tauri.conf.json`.
pub const CRASH_LOOP_THRESHOLD: &str = "crash_loop_threshold";
pub const SKIPPED_VERSION: &str = "skipped_version";
pub const UPDATE_LOCK: &str = "update.lock";
pub const COMMIT_JOURNAL: &str = "commit.journal";
pub const ROLLBACK_JOURNAL: &str = "rollback.journal";

/// Everything the replacement process needs, written before the journal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyConfig {
    pub v: u32,
    pub install_root: PathBuf,
    pub staging_root: PathBuf,
    pub journal_root: PathBuf,
    pub version: String,
    pub commit_id: String,
    pub parent_pid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CommitStatus {
    Completed { version: String, commit_id: String },
    Failed { op_index: u32, message: String },
}

pub fn write_apply_config(journal_root: &Path, config: &ApplyConfig) -> Result<(), EngineError> {
    write_json(&journal_root.join(APPLY_JSON), config)
}

pub fn read_apply_config(journal_root: &Path) -> Result<Option<ApplyConfig>, EngineError> {
    read_json(&journal_root.join(APPLY_JSON))
}

/// Metadata binding a commit journal to a signed manifest. The worker verifies
/// the detached signature with its embedded public keys before executing any
/// operation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct CommitMeta {
    pub manifest_hash: String,
    pub version: String,
    pub from_version: String,
    pub install_root: PathBuf,
    pub commit_id: String,
}

pub fn write_commit_meta(journal_root: &Path, meta: &CommitMeta) -> Result<(), EngineError> {
    write_json(&journal_root.join(COMMIT_META), meta)
}

pub fn read_commit_meta(journal_root: &Path) -> Result<Option<CommitMeta>, EngineError> {
    read_json(&journal_root.join(COMMIT_META))
}

pub fn write_verified_manifest(
    journal_root: &Path,
    raw: &[u8],
    signature: &str,
) -> Result<(), EngineError> {
    crate::fs::atomic::write_atomic(&journal_root.join(VERIFIED_MANIFEST), raw)
        .map_err(|err| EngineError::Internal(format!("verified manifest: {err}")))?;
    crate::fs::atomic::write_atomic(&journal_root.join(VERIFIED_SIGNATURE), signature.as_bytes())
        .map_err(|err| EngineError::Internal(format!("verified signature: {err}")))
}

pub fn read_verified_manifest(
    journal_root: &Path,
) -> Result<Option<(Vec<u8>, String)>, EngineError> {
    read_manifest_pair(
        &journal_root.join(VERIFIED_MANIFEST),
        &journal_root.join(VERIFIED_SIGNATURE),
    )
}

pub fn read_installed_manifest(
    journal_root: &Path,
) -> Result<Option<(Vec<u8>, String)>, EngineError> {
    read_manifest_pair(
        &journal_root.join(INSTALLED_MANIFEST),
        &journal_root.join(INSTALLED_SIGNATURE),
    )
}

/// Keep the manifest of the version that was just confirmed healthy. This is
/// the trusted old manifest used to validate a later rollback.
pub fn promote_verified_to_installed(journal_root: &Path) -> Result<(), EngineError> {
    let Some((raw, signature)) = read_verified_manifest(journal_root)? else {
        return Ok(());
    };
    crate::fs::atomic::write_atomic(&journal_root.join(INSTALLED_MANIFEST), &raw)
        .map_err(|err| EngineError::Internal(format!("installed manifest: {err}")))?;
    crate::fs::atomic::write_atomic(
        &journal_root.join(INSTALLED_SIGNATURE),
        signature.as_bytes(),
    )
    .map_err(|err| EngineError::Internal(format!("installed signature: {err}")))?;
    let _ = std::fs::remove_file(journal_root.join(VERIFIED_MANIFEST));
    let _ = std::fs::remove_file(journal_root.join(VERIFIED_SIGNATURE));
    Ok(())
}

fn read_manifest_pair(
    manifest: &Path,
    signature: &Path,
) -> Result<Option<(Vec<u8>, String)>, EngineError> {
    let raw = match std::fs::read(manifest) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(EngineError::Internal(format!(
                "read {}: {err}",
                manifest.display()
            )))
        }
    };
    let signature = std::fs::read_to_string(signature)
        .map_err(|err| EngineError::Internal(format!("read {}: {err}", signature.display())))?;
    Ok(Some((raw, signature)))
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct OldFileHash {
    pub sha256: String,
    pub size: u64,
}

/// Hashes of the files that will be replaced or removed by this commit. They
/// are captured before the journal is written and used by rollback to verify
/// that the backup still contains the original bytes. Without this snapshot,
/// inverse generation would hash the backup itself and accept a corrupt backup.
pub type OldHashes = BTreeMap<String, OldFileHash>;

pub fn write_old_hashes(journal_root: &Path, hashes: &OldHashes) -> Result<(), EngineError> {
    write_json(&journal_root.join(OLD_HASHES), hashes)
}

pub fn read_old_hashes(journal_root: &Path) -> Result<OldHashes, EngineError> {
    Ok(read_json(&journal_root.join(OLD_HASHES))?.unwrap_or_default())
}

pub fn remove_old_hashes(journal_root: &Path) {
    let _ = std::fs::remove_file(journal_root.join(OLD_HASHES));
}

pub fn write_commit_status(journal_root: &Path, status: &CommitStatus) -> Result<(), EngineError> {
    write_json(&journal_root.join(COMMIT_STATUS), status)
}

pub fn read_commit_status(journal_root: &Path) -> Result<Option<CommitStatus>, EngineError> {
    read_json(&journal_root.join(COMMIT_STATUS))
}

pub fn increment_attempts(journal_root: &Path, threshold: u32) -> Result<u64, EngineError> {
    let attempts = read_attempts(journal_root).saturating_add(1);
    write_attempts(journal_root, attempts, threshold)?;
    Ok(attempts)
}

pub fn read_attempts(journal_root: &Path) -> u64 {
    std::fs::read_to_string(journal_root.join(ATTEMPTS))
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
}

pub fn write_attempts(
    journal_root: &Path,
    attempts: u64,
    threshold: u32,
) -> Result<(), EngineError> {
    write_text(&journal_root.join(ATTEMPTS), &attempts.to_string())?;
    write_text(
        &journal_root.join(CRASH_LOOP_THRESHOLD),
        &threshold.to_string(),
    )
}

/// Threshold recorded by the last startup, if this machine has started before.
pub fn read_crash_loop_threshold(journal_root: &Path) -> Option<u32> {
    std::fs::read_to_string(journal_root.join(CRASH_LOOP_THRESHOLD))
        .ok()
        .and_then(|value| value.trim().parse().ok())
}

pub fn reset_attempts(journal_root: &Path) -> Result<(), EngineError> {
    write_text(&journal_root.join(ATTEMPTS), "0")
}

pub fn write_skipped_version(journal_root: &Path, version: &str) -> Result<(), EngineError> {
    write_text(&journal_root.join(SKIPPED_VERSION), version)
}

pub fn read_skipped_version(journal_root: &Path) -> Option<String> {
    std::fs::read_to_string(journal_root.join(SKIPPED_VERSION))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Remove the rollback skip marker so the same version can be attempted again
/// (for example after the publisher re-released that version).
pub fn clear_skipped_version(journal_root: &Path) {
    let _ = std::fs::remove_file(journal_root.join(SKIPPED_VERSION));
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), EngineError> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|err| EngineError::Internal(format!("state file: {err}")))?;
    write_bytes(path, &bytes)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>, EngineError> {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(value) => Ok(Some(value)),
            Err(err) => {
                tracing::warn!(%err, path = %path.display(), "ignoring malformed state file");
                Ok(None)
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(EngineError::Internal(format!(
            "read {}: {err}",
            path.display()
        ))),
    }
}

fn write_text(path: &Path, value: &str) -> Result<(), EngineError> {
    write_bytes(path, value.as_bytes())
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), EngineError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| EngineError::Internal(format!("create {}: {err}", parent.display())))?;
    }
    crate::fs::atomic::write_atomic(path, bytes)
        .map_err(|err| EngineError::Internal(format!("write {}: {err}", path.display())))?;
    if let Some(parent) = path.parent() {
        let _ = crate::fs::atomic::sync_dir(parent);
    }
    Ok(())
}
