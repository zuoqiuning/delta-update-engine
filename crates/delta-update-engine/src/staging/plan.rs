use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::EngineError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedFile {
    pub path: String,
    pub staging_path: PathBuf,
    pub sha256: String,
    pub size: u64,
}

/// Persisted PREPARED plan. It contains only changed files because unchanged
/// files are verified in place and never copied into staging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedPlan {
    pub version: String,
    pub manifest_hash: String,
    pub staging_root: PathBuf,
    pub files: Vec<StagedFile>,
    pub removed: Vec<String>,
    pub all_files: Vec<String>,
}

impl PreparedPlan {
    pub fn save(&self, path: &Path) -> Result<(), EngineError> {
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|err| EngineError::Internal(format!("prepared plan: {err}")))?;
        crate::fs::atomic::write_atomic(path, &bytes)
            .map_err(|err| EngineError::Internal(format!("prepared plan: {err}")))
    }

    pub fn load(path: &Path) -> Result<Self, EngineError> {
        let bytes = std::fs::read(path)
            .map_err(|err| EngineError::Internal(format!("prepared plan: {err}")))?;
        serde_json::from_slice(&bytes)
            .map_err(|err| EngineError::Internal(format!("prepared plan: {err}")))
    }
}
