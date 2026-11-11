use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::EngineError;

pub const APPLY_JSON: &str = "apply.json";
pub const COMMIT_STATUS: &str = "commit.status";
pub const PREPARED_JSON: &str = "prepared.json";
pub const ATTEMPTS: &str = "attempts";
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

pub fn write_commit_status(journal_root: &Path, status: &CommitStatus) -> Result<(), EngineError> {
    write_json(&journal_root.join(COMMIT_STATUS), status)
}

pub fn read_commit_status(journal_root: &Path) -> Result<Option<CommitStatus>, EngineError> {
    read_json(&journal_root.join(COMMIT_STATUS))
}

pub fn increment_attempts(journal_root: &Path) -> Result<u64, EngineError> {
    let attempts = read_attempts(journal_root).saturating_add(1);
    write_text(&journal_root.join(ATTEMPTS), &attempts.to_string())?;
    Ok(attempts)
}

pub fn read_attempts(journal_root: &Path) -> u64 {
    std::fs::read_to_string(journal_root.join(ATTEMPTS))
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
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
