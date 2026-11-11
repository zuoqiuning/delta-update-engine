use crate::crypto::sha256_file;
use crate::error::{EngineError, PatchError};

use super::plan::PreparedPlan;

/// Re-verify a persisted PREPARED plan before reusing it.
pub fn verify_plan(plan: &PreparedPlan) -> Result<(), EngineError> {
    for file in &plan.files {
        let metadata = std::fs::metadata(&file.staging_path)
            .map_err(|err| EngineError::Internal(format!("{}: {err}", file.path)))?;
        if metadata.len() != file.size {
            return Err(PatchError::TargetHashMismatch(format!(
                "{}: staged size {} does not match {}",
                file.path,
                metadata.len(),
                file.size
            ))
            .into());
        }
        let actual = sha256_file(&file.staging_path)
            .map_err(|err| EngineError::Internal(format!("{}: {err}", file.path)))?;
        if actual != file.sha256 {
            return Err(PatchError::TargetHashMismatch(format!(
                "{}: staged hash does not match the manifest",
                file.path
            ))
            .into());
        }
    }
    Ok(())
}
