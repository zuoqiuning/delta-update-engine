use std::path::{Path, PathBuf};

use crate::error::EngineError;

/// Resolve the install root.
///
/// The running executable is the authoritative source. A configured override
/// is accepted only when it is absolute, exists, and contains the executable.
pub fn resolve_install_root(
    configured: Option<&Path>,
    current_exe: &Path,
) -> Result<PathBuf, EngineError> {
    let exe = dunce::canonicalize(current_exe).map_err(|err| {
        EngineError::ConfigInvalid(format!("cannot resolve current executable: {err}"))
    })?;
    let exe_parent = exe.parent().ok_or_else(|| {
        EngineError::ConfigInvalid("current executable has no parent directory".into())
    })?;

    if let Some(root) = configured {
        if !root.is_absolute() {
            return Err(EngineError::ConfigInvalid(
                "install_dir must be an absolute path".into(),
            ));
        }
        let root = dunce::canonicalize(root).map_err(|err| {
            EngineError::ConfigInvalid(format!("install_dir does not exist: {err}"))
        })?;
        if !exe.starts_with(&root) {
            return Err(EngineError::ConfigInvalid(
                "install_dir does not contain the running executable".into(),
            ));
        }
        return Ok(root);
    }

    Ok(exe_parent.to_path_buf())
}
