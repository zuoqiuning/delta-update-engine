use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::EngineError;

/// Version of the persisted PREPARED plan. Files written before this field
/// existed default to 1, which matches the current layout.
pub const PREPARED_SCHEMA: u32 = 1;

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
    #[serde(default = "default_schema")]
    pub schema: u32,
    pub version: String,
    pub manifest_hash: String,
    pub staging_root: PathBuf,
    pub files: Vec<StagedFile>,
    pub removed: Vec<String>,
    pub all_files: Vec<String>,
}

fn default_schema() -> u32 {
    PREPARED_SCHEMA
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
        let plan: Self = serde_json::from_slice(&bytes)
            .map_err(|err| EngineError::Internal(format!("prepared plan: {err}")))?;
        if plan.schema > PREPARED_SCHEMA {
            return Err(EngineError::JournalFormatTooNew {
                client: u16::try_from(PREPARED_SCHEMA).unwrap_or(u16::MAX),
                actual: u16::try_from(plan.schema).unwrap_or(u16::MAX),
            });
        }
        Ok(plan)
    }
}
