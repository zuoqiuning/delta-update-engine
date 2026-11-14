use std::path::{Path, PathBuf};

use crate::error::EngineError;

use super::rel_path::{validate, validate_install, RelPath};

pub fn validate_rel_path(value: &str) -> Result<RelPath, EngineError> {
    validate(value).map(RelPath::from_validated)
}

/// Validate a path that lives inside the installation root. Unlike object
/// keys, install paths must not collide with the engine's `.old`/`.trash`
/// bookkeeping directories.
pub fn validate_install_path(value: &str) -> Result<RelPath, EngineError> {
    validate_install(value).map(RelPath::from_validated)
}

/// Join a validated relative path to a root, rejecting any escape.
pub fn resolve_within(root: &Path, rel: &RelPath) -> Result<PathBuf, EngineError> {
    let joined = root.join(rel.to_path_buf());
    if !joined.starts_with(root) {
        return Err(EngineError::PathEscape(format!(
            "{rel} escapes {}",
            root.display()
        )));
    }
    Ok(joined)
}
