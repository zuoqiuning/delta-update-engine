use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use anyhow::{bail, Context};
use rayon::prelude::*;

use crate::ignore::DeltaIgnore;

#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub sha256: String,
    pub size: u64,
}

pub type ScanResult = BTreeMap<String, ScannedFile>;

/// Walk a build output directory and hash every file that should be published.
pub fn scan(root: &Path, ignore: &DeltaIgnore) -> anyhow::Result<ScanResult> {
    scan_with_jobs(root, ignore, default_jobs())
}

/// Same as [`scan`] with an explicit hashing parallelism.
///
/// The walk stays serial (it is cheap and keeps collision errors
/// deterministic); hashing is CPU/IO bound and dominates large builds, so it
/// runs on a bounded pool. `release` passes `--jobs` here so scanning and patch
/// generation never oversubscribe the machine together.
pub fn scan_with_jobs(
    root: &Path,
    ignore: &DeltaIgnore,
    jobs: usize,
) -> anyhow::Result<ScanResult> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry.context("walk build output")?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .context("strip build root")?
            .to_string_lossy()
            .replace('\\', "/");
        if ignore.is_ignored(&rel) {
            continue;
        }
        if !seen.insert(rel.to_ascii_lowercase()) {
            bail!("path collision (case-insensitive): {rel}");
        }
        let size = entry.metadata().context("read metadata")?.len();
        entries.push((rel, path.to_path_buf(), size));
    }

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs.max(1))
        .build()
        .context("build scan thread pool")?;
    let hashed: Vec<anyhow::Result<(String, ScannedFile)>> = pool.install(|| {
        entries
            .into_par_iter()
            .map(|(rel, path, size)| {
                let sha256 = delta_update_engine::crypto::sha256_file(&path)
                    .with_context(|| format!("hash {}", path.display()))?;
                Ok((rel, ScannedFile { sha256, size }))
            })
            .collect()
    });

    let mut files = BTreeMap::new();
    for entry in hashed {
        let (rel, file) = entry?;
        files.insert(rel, file);
    }
    Ok(files)
}

/// Half the cores, capped, matching the patch pool default.
fn default_jobs() -> usize {
    (std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
        / 2)
    .clamp(1, 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_files_and_scopes_paths_to_the_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        std::fs::write(dir.path().join("bin/app.exe"), b"payload").unwrap();
        std::fs::write(dir.path().join("readme.txt"), b"text").unwrap();

        let files = scan_with_jobs(dir.path(), &DeltaIgnore::default(), 2).unwrap();
        assert_eq!(
            files.keys().collect::<Vec<_>>(),
            ["bin/app.exe", "readme.txt"]
        );
        assert_eq!(files["bin/app.exe"].size, 7);
        assert_eq!(
            files["bin/app.exe"].sha256,
            delta_update_engine::crypto::sha256_bytes(b"payload")
        );
    }
}
