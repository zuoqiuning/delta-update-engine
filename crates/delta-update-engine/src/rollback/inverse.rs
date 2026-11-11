use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::crypto::sha256_file;
use crate::error::EngineError;
use crate::journal::{FileOp, Journal};

/// Build the inverse operation list for a commit journal.
///
/// Backups that were never created (crash before the op) are skipped: the old
/// file is still in place, so there is nothing to restore. New files without a
/// backup are trashed.
pub fn inverse(install_root: &Path, commit: &Journal) -> Result<Vec<FileOp>, EngineError> {
    let backups: HashSet<&str> = commit
        .ops
        .iter()
        .filter_map(|op| match op {
            FileOp::Backup { src, .. } => Some(src.as_str()),
            _ => None,
        })
        .collect();

    let mut ops = Vec::new();
    for op in &commit.ops {
        match op {
            FileOp::Place { dst, .. } => {
                if backups.contains(dst.as_str()) {
                    let backup = install_root.join(".old").join(&commit.commit_id).join(dst);
                    let Some((sha256, size)) = hash_if_exists(&backup)? else {
                        continue;
                    };
                    ops.push(FileOp::Restore {
                        backup_src: format!("old/{dst}"),
                        dst: dst.clone(),
                        sha256,
                        size,
                    });
                } else {
                    ops.push(FileOp::Trash {
                        src: dst.clone(),
                        trash_dst: dst.clone(),
                    });
                }
            }
            FileOp::Trash { src, trash_dst } => {
                let trashed = trash_path(install_root, &commit.commit_id, trash_dst);
                let Some((sha256, size)) = hash_if_exists(&trashed)? else {
                    continue;
                };
                ops.push(FileOp::Restore {
                    backup_src: format!("trash/{trash_dst}"),
                    dst: src.clone(),
                    sha256,
                    size,
                });
            }
            FileOp::Backup { .. } => {}
            FileOp::Restore { .. } => {
                return Err(EngineError::RollbackFailed(
                    "commit journal contains unexpected Restore ops".into(),
                ))
            }
            FileOp::RemoveTrash { .. } => {
                return Err(EngineError::RollbackFailed(
                    "RemoveTrash must not appear before confirm_healthy".into(),
                ))
            }
        }
    }
    Ok(ops)
}

fn trash_path(root: &Path, commit_id: &str, relative: &str) -> PathBuf {
    root.join(".trash").join(commit_id).join(relative)
}

fn hash_if_exists(path: &Path) -> Result<Option<(String, u64)>, EngineError> {
    match std::fs::metadata(path) {
        Ok(metadata) => {
            let sha256 = sha256_file(path)
                .map_err(|err| EngineError::RollbackFailed(format!("{}: {err}", path.display())))?;
            Ok(Some((sha256, metadata.len())))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(EngineError::RollbackFailed(format!(
            "{}: {err}",
            path.display()
        ))),
    }
}
