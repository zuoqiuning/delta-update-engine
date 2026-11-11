use std::collections::{BTreeMap, BTreeSet};

use crate::error::EngineError;

use super::model::{FileEntry, Manifest, Strategy};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFile {
    pub sha256: String,
    pub size: u64,
}

pub type LocalManifest = BTreeMap<String, LocalFile>;

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ModifiedFile {
    pub path: String,
    pub entry: FileEntry,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DiffResult {
    pub unchanged: Vec<String>,
    pub modified: Vec<ModifiedFile>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub untracked: Vec<String>,
    pub download_bytes: u64,
    pub total_bytes: u64,
}

/// Classify every remote entry against the local tree and the known-file set.
pub fn diff(
    local: &LocalManifest,
    remote: &Manifest,
    known: &BTreeSet<String>,
) -> Result<DiffResult, EngineError> {
    let mut unchanged = Vec::new();
    let mut modified = Vec::new();
    let mut added = Vec::new();
    let mut download_bytes = 0_u64;
    let mut total_bytes = 0_u64;

    for (path, entry) in &remote.files {
        total_bytes = total_bytes.saturating_add(entry.size);
        match local.get(path) {
            Some(local_file) if local_file.sha256.eq_ignore_ascii_case(&entry.sha256) => {
                unchanged.push(path.clone());
            }
            Some(_) => {
                require_full_key(path, entry)?;
                download_bytes = download_bytes.saturating_add(download_size(entry));
                modified.push(ModifiedFile {
                    path: path.clone(),
                    entry: entry.clone(),
                });
            }
            None => {
                require_full_key(path, entry)?;
                download_bytes = download_bytes.saturating_add(entry.size);
                added.push(path.clone());
            }
        }
    }

    let mut removed = Vec::new();
    let mut untracked = Vec::new();
    for path in local.keys() {
        if remote.files.contains_key(path) {
            continue;
        }
        if known.contains(path) {
            removed.push(path.clone());
        } else {
            untracked.push(path.clone());
        }
    }

    Ok(DiffResult {
        unchanged,
        modified,
        added,
        removed,
        untracked,
        download_bytes,
        total_bytes,
    })
}

fn download_size(entry: &FileEntry) -> u64 {
    match entry.strategy {
        Strategy::Full | Strategy::Unknown(_) => entry.size,
        _ => entry.delta_size.unwrap_or(entry.size),
    }
}

fn require_full_key(path: &str, entry: &FileEntry) -> Result<(), EngineError> {
    if entry.full_key.is_none() {
        return Err(EngineError::ManifestInvalid(format!(
            "{path}: changed entries require full_key"
        )));
    }
    Ok(())
}
