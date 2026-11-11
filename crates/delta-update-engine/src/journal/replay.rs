use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::crypto::sha256_file;
use crate::error::EngineError;
use crate::fs::{FileSystem, RealFs};
use crate::path::validate_rel_path;

use super::format::{FileOp, Journal, JOURNAL_MAX_OPS};

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum JournalOutcome {
    NoJournal,
    Replayed { ops_completed: usize },
    Corrupt { reason: String },
    ReplayFailed { op_index: usize, reason: String },
    AlreadyCompleted,
}

pub struct ReplayContext<'a> {
    pub install_root: &'a Path,
    pub staging_root: &'a Path,
    pub journal_root: &'a Path,
    pub commit_id: &'a str,
}

/// Replay a journal forward, idempotently.
pub fn replay(
    context: &ReplayContext<'_>,
    journal: &Journal,
) -> Result<JournalOutcome, EngineError> {
    replay_with_fs(context, journal, &RealFs)
}

/// Replay against an injected filesystem.
pub(crate) fn replay_with_fs(
    context: &ReplayContext<'_>,
    journal: &Journal,
    fs: &dyn FileSystem,
) -> Result<JournalOutcome, EngineError> {
    if journal.ops.len() > JOURNAL_MAX_OPS {
        return Err(EngineError::JournalCorrupt(
            "journal exceeds the operation limit".into(),
        ));
    }
    if journal.commit_id != context.commit_id {
        return Err(EngineError::JournalCorrupt(
            "journal commit_id does not match the commit context".into(),
        ));
    }

    let mut completed = 0;
    for (index, op) in journal.ops.iter().enumerate() {
        match is_done(context, op) {
            Ok(true) => {
                completed += 1;
                continue;
            }
            Ok(false) => {}
            Err(reason) => {
                return Ok(JournalOutcome::ReplayFailed {
                    op_index: index,
                    reason,
                })
            }
        }
        if let Err(reason) = execute(context, op, fs) {
            return Ok(JournalOutcome::ReplayFailed {
                op_index: index,
                reason,
            });
        }
        completed += 1;
    }
    Ok(JournalOutcome::Replayed {
        ops_completed: completed,
    })
}

fn is_done(context: &ReplayContext<'_>, op: &FileOp) -> Result<bool, String> {
    match op {
        // A backup is done once the backup exists; the source path may have
        // been repopulated by a later Place. A missing source also means there
        // was nothing to back up.
        FileOp::Backup { src, dst } => Ok(resolve(context.install_root, dst)?.exists()
            || !resolve(context.install_root, src)?.exists()),
        FileOp::Place {
            dst, sha256, size, ..
        }
        | FileOp::Restore {
            dst, sha256, size, ..
        } => {
            let dest = resolve(context.install_root, dst)?;
            if !dest.exists() {
                return Ok(false);
            }
            hash_matches(&dest, sha256, *size)
        }
        // Same reasoning as Backup: a later op may have repopulated src.
        FileOp::Trash { src, trash_dst } => {
            Ok(trash_root(context).join(validate(trash_dst)?).exists()
                || !resolve(context.install_root, src)?.exists())
        }
        FileOp::RemoveTrash { trash_path } => {
            Ok(!trash_root(context).join(validate(trash_path)?).exists())
        }
    }
}

fn execute(context: &ReplayContext<'_>, op: &FileOp, fs: &dyn FileSystem) -> Result<(), String> {
    match op {
        FileOp::Backup { src, dst } => {
            let source = resolve(context.install_root, src)?;
            let dest = resolve(context.install_root, dst)?;
            if !source.exists() {
                return Err(format!("backup source is missing: {src}"));
            }
            move_path(fs, &source, &dest, true)
        }
        FileOp::Place {
            staging_src,
            dst,
            sha256,
            size,
        } => {
            let source = resolve(context.staging_root, staging_src)?;
            let dest = resolve(context.install_root, dst)?;
            if !source.exists() {
                return Err(format!("staged source is missing: {staging_src}"));
            }
            if !hash_matches(&source, sha256, *size)? {
                return Err(format!("staged source hash mismatch: {staging_src}"));
            }
            move_path(fs, &source, &dest, true)?;
            if !hash_matches(&dest, sha256, *size)? {
                let _ = fs.remove(&dest);
                return Err(format!("target hash mismatch after place: {dst}"));
            }
            Ok(())
        }
        FileOp::Trash { src, trash_dst } => {
            let source = resolve(context.install_root, src)?;
            let dest = trash_root(context).join(validate(trash_dst)?);
            if !source.exists() {
                return Err(format!("trash source is missing: {src}"));
            }
            move_path(fs, &source, &dest, true)
        }
        FileOp::Restore {
            backup_src,
            dst,
            sha256,
            size,
        } => {
            let source = backup_source(context, backup_src)?;
            let dest = resolve(context.install_root, dst)?;
            if !source.exists() {
                return Err(format!("backup source is missing: {backup_src}"));
            }
            if !hash_matches(&source, sha256, *size)? {
                return Err(format!("backup source hash mismatch: {backup_src}"));
            }
            move_path(fs, &source, &dest, true)?;
            if !hash_matches(&dest, sha256, *size)? {
                let _ = fs.remove(&dest);
                return Err(format!("target hash mismatch after restore: {dst}"));
            }
            Ok(())
        }
        FileOp::RemoveTrash { trash_path } => {
            let path = trash_root(context).join(validate(trash_path)?);
            if path.exists() {
                fs.remove(&path)
                    .map_err(|err| format!("remove {}: {err}", path.display()))?;
            }
            Ok(())
        }
    }
}

fn backup_source(context: &ReplayContext<'_>, value: &str) -> Result<PathBuf, String> {
    if let Some(rest) = value.strip_prefix("old/") {
        let rest = validate(rest)?;
        return Ok(context
            .install_root
            .join(".old")
            .join(context.commit_id)
            .join(rest));
    }
    if let Some(rest) = value.strip_prefix("trash/") {
        let rest = validate(rest)?;
        return Ok(trash_root(context).join(rest));
    }
    Err(format!("invalid backup source prefix: {value}"))
}

fn trash_root(context: &ReplayContext<'_>) -> PathBuf {
    context.install_root.join(".trash").join(context.commit_id)
}

fn resolve(root: &Path, value: &str) -> Result<PathBuf, String> {
    Ok(root.join(validate(value)?))
}

fn validate(value: &str) -> Result<PathBuf, String> {
    validate_rel_path(value).map_err(|err| err.to_string())?;
    Ok(PathBuf::from(value))
}

fn hash_matches(path: &Path, expected_sha256: &str, expected_size: u64) -> Result<bool, String> {
    let metadata = std::fs::metadata(path).map_err(|err| format!("{}: {err}", path.display()))?;
    if metadata.len() != expected_size {
        return Ok(false);
    }
    let actual = sha256_file(path).map_err(|err| format!("{}: {err}", path.display()))?;
    Ok(actual == expected_sha256)
}

/// Move a file, falling back to copy + fsync + atomic rename across volumes.
fn move_path(
    fs: &dyn FileSystem,
    source: &Path,
    dest: &Path,
    overwrite: bool,
) -> Result<(), String> {
    if dest.exists() {
        if !overwrite {
            return Err(format!("destination already exists: {}", dest.display()));
        }
        fs.remove(dest)
            .map_err(|err| format!("remove {}: {err}", dest.display()))?;
    }
    if let Some(parent) = dest.parent() {
        fs.create_dir_all(parent)
            .map_err(|err| format!("create {}: {err}", parent.display()))?;
    }

    match fs.rename(source, dest) {
        Ok(()) => {
            sync_parent(fs, dest);
            return Ok(());
        }
        // Only a cross-volume move justifies the copy fallback. Retrying on
        // permission or disk-full errors would copy large files for nothing
        // and hide the real cause.
        Err(err) if err.kind() == std::io::ErrorKind::CrossesDevices => {}
        Err(err) => {
            return Err(format!(
                "rename {} -> {}: {err}",
                source.display(),
                dest.display()
            ))
        }
    }

    let name = dest
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("object");
    let temp = dest.with_file_name(format!(".{name}.tmp-{}", Uuid::new_v4()));
    std::fs::copy(source, &temp).map_err(|err| format!("copy {}: {err}", source.display()))?;
    if let Ok(file) = std::fs::File::open(&temp) {
        let _ = file.sync_all();
    }
    fs.rename(&temp, dest)
        .map_err(|err| format!("rename {}: {err}", temp.display()))?;
    fs.remove(source)
        .map_err(|err| format!("remove {}: {err}", source.display()))?;
    sync_parent(fs, dest);
    Ok(())
}

fn sync_parent(fs: &dyn FileSystem, path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = fs.sync_dir(parent);
    }
}
