use std::path::Path;

use crate::error::EngineError;
use crate::path::validate_rel_path;

use super::format::{FileOp, Journal, JOURNAL_MAX_OPS};
use crate::staging::PreparedPlan;

/// Build the forward commit journal from a verified PREPARED plan.
///
/// Existing files get a `Backup` before their `Place`; removed files are
/// moved to `.trash/<commit_id>/`. `RemoveTrash` is deliberately absent until
/// `confirm_healthy`.
pub fn build(
    plan: &PreparedPlan,
    commit_id: &str,
    from_version: &str,
    install_root: &Path,
) -> Result<Journal, EngineError> {
    let mut ops = Vec::with_capacity(plan.files.len() * 2 + plan.removed.len());
    for file in &plan.files {
        validate_rel_path(&file.path)?;
        if install_root.join(&file.path).exists() {
            ops.push(FileOp::Backup {
                src: file.path.clone(),
                dst: format!(".old/{commit_id}/{}", file.path),
            });
        }
        ops.push(FileOp::Place {
            staging_src: format!("files/{}", file.path),
            dst: file.path.clone(),
            sha256: file.sha256.clone(),
            size: file.size,
        });
    }
    for path in &plan.removed {
        validate_rel_path(path)?;
        ops.push(FileOp::Trash {
            src: path.clone(),
            trash_dst: path.clone(),
        });
    }
    if ops.len() > JOURNAL_MAX_OPS {
        return Err(EngineError::JournalCorrupt(
            "commit journal exceeds the operation limit".into(),
        ));
    }
    Ok(Journal::new(
        commit_id,
        plan.version.clone(),
        from_version,
        ops,
    ))
}
