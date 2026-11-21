//! Local install scan and diff for `check`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::crypto::sha256_file;
use crate::error::EngineError;
use crate::manifest::diff::{diff, DiffResult, LocalFile, LocalManifest};
use crate::manifest::hash_cache::HashCache;
use crate::manifest::history::KnownFiles;
use crate::manifest::model::Manifest;
use crate::staging::VerifiedInventory;

pub(crate) fn diff_install(
    install_root: &Path,
    journal_root: &Path,
    remote: &Manifest,
    current_version: &str,
) -> Result<(DiffResult, u32, VerifiedInventory), EngineError> {
    let mut cache = HashCache::load(journal_root, install_root);
    let (local, placeholders_skipped, mtimes) = scan_local(install_root, &mut cache)?;
    let known = KnownFiles::load(&journal_root.join("known_files.json"))?;
    let result = diff(&local, remote, &known.files, current_version)?;
    cache.save(journal_root);

    // Only files that the commit will replace or remove need their old hash
    // for rollback. Saving just those keeps the session copy small.
    let inventory = VerifiedInventory::default();
    for path in result
        .modified
        .iter()
        .map(|file| &file.path)
        .chain(result.removed.iter())
    {
        let (Some(mtime_ns), Some(file)) = (mtimes.get(path), local.get(path)) else {
            continue;
        };
        inventory.record_hashed(
            install_root.join(path),
            file.size,
            *mtime_ns,
            file.sha256.clone(),
        );
    }
    Ok((result, placeholders_skipped, inventory))
}

/// One file found while walking the install root.
struct ScanEntry {
    relative: String,
    path: PathBuf,
    size: u64,
    mtime: Option<u64>,
    /// Hash reused from the size+mtime cache, if any.
    cached: Option<String>,
}

/// Scan the install root. Returns the local manifest and the number of cloud
/// placeholder files that were intentionally left out of the diff.
///
/// The walk stays serial (it is cheap and keeps diagnostics deterministic);
/// hashing runs on a bounded pool because it dominates the scan of a large
/// install and each file is independent.
fn scan_local(
    root: &Path,
    cache: &mut HashCache,
) -> Result<(LocalManifest, u32, BTreeMap<String, u64>), EngineError> {
    use rayon::prelude::*;

    let mut entries: Vec<ScanEntry> = Vec::new();
    let mut present = BTreeSet::new();
    let mut placeholders_skipped = 0_u32;
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry
            .map_err(|err| EngineError::Internal(format!("scan {}: {err}", root.display())))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|err| EngineError::Internal(format!("scan {}: {err}", root.display())))?
            .to_string_lossy()
            .replace('\\', "/");
        if relative.split('/').next().is_some_and(|segment| {
            segment.eq_ignore_ascii_case(".old") || segment.eq_ignore_ascii_case(".trash")
        }) {
            continue;
        }
        let metadata = entry
            .metadata()
            .map_err(|err| io_error(err.into(), &relative))?;
        if crate::fs::is_cloud_placeholder(&metadata) {
            // Hashing would hydrate the placeholder and stall `check` for
            // seconds per file. Leave it out of the diff and warn: if the
            // manifest still lists it, it is materialized from its
            // content-addressed blob like any missing file.
            tracing::warn!(
                file = %relative,
                "skipping cloud placeholder (OneDrive/Files On-Demand); download it locally to include it in the diff"
            );
            placeholders_skipped = placeholders_skipped.saturating_add(1);
            continue;
        }
        let size = metadata.len();
        let mtime = mtime_ns(&metadata);
        present.insert(relative.clone());
        let cached = mtime
            .and_then(|mtime| cache.lookup(&relative, size, mtime))
            .map(str::to_string);
        entries.push(ScanEntry {
            relative,
            path: entry.path().to_path_buf(),
            size,
            mtime,
            cached,
        });
    }

    let threads = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
        .min(8);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|err| EngineError::Internal(format!("hash pool: {err}")))?;
    let hashed: Vec<Result<(usize, String), EngineError>> = pool.install(|| {
        entries
            .par_iter()
            .enumerate()
            .filter(|(_, entry)| entry.cached.is_none())
            .map(|(index, entry)| {
                let sha256 =
                    sha256_file(&entry.path).map_err(|err| io_error(err, &entry.relative))?;
                Ok((index, sha256))
            })
            .collect()
    });
    let mut resolved: Vec<Option<String>> =
        entries.iter().map(|entry| entry.cached.clone()).collect();
    for result in hashed {
        let (index, sha256) = result?;
        resolved[index] = Some(sha256);
    }

    let mut files = LocalManifest::new();
    let mut mtimes = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        let sha256 = resolved[index].take().ok_or_else(|| {
            EngineError::Internal(format!("{}: hash was not computed", entry.relative))
        })?;
        if let Some(mtime) = entry.mtime {
            mtimes.insert(entry.relative.clone(), mtime);
            if entry.cached.is_none() {
                cache.insert(&entry.relative, entry.size, mtime, sha256.clone());
            }
        }
        files.insert(
            entry.relative.clone(),
            LocalFile {
                sha256,
                size: entry.size,
            },
        );
    }
    cache.retain(&present);
    Ok((files, placeholders_skipped, mtimes))
}

/// Modification time as nanoseconds. `None` when the filesystem cannot report
/// it, in which case the file is always re-hashed rather than cached.
fn mtime_ns(metadata: &std::fs::Metadata) -> Option<u64> {
    let elapsed = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some(elapsed.as_nanos().min(u128::from(u64::MAX)) as u64)
}

fn io_error(err: std::io::Error, path: &str) -> EngineError {
    if err.kind() == std::io::ErrorKind::PermissionDenied {
        EngineError::PermissionDenied(format!("{path}: {err}"))
    } else {
        EngineError::Internal(format!("{path}: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_local_reuses_cache_and_invalidates_on_change() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("app.exe"), b"v1").unwrap();

        let mut cache = HashCache::load(&root.join("journal"), root);
        let (first, skipped, mtimes) = scan_local(root, &mut cache).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(first["app.exe"].sha256, crate::crypto::sha256_bytes(b"v1"));
        assert!(mtimes.contains_key("app.exe"));

        // Same size and mtime must reuse the cached hash verbatim.
        let metadata = std::fs::metadata(root.join("app.exe")).unwrap();
        cache.insert(
            "app.exe",
            metadata.len(),
            mtime_ns(&metadata).unwrap(),
            "deadbeef".into(),
        );
        assert_eq!(
            scan_local(root, &mut cache).unwrap().0["app.exe"].sha256,
            "deadbeef"
        );

        // A size change invalidates the entry and re-hashes the file.
        std::fs::write(root.join("app.exe"), b"v2-longer").unwrap();
        assert_eq!(
            scan_local(root, &mut cache).unwrap().0["app.exe"].sha256,
            crate::crypto::sha256_bytes(b"v2-longer")
        );
    }
}
