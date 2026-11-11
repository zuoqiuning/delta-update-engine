use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use anyhow::{bail, Context};

use crate::ignore::DeltaIgnore;

#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub sha256: String,
    pub size: u64,
}

pub type ScanResult = BTreeMap<String, ScannedFile>;

/// Walk a build output directory and hash every file that should be published.
pub fn scan(root: &Path, ignore: &DeltaIgnore) -> anyhow::Result<ScanResult> {
    let mut files = BTreeMap::new();
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
        let sha256 = delta_update_engine::crypto::sha256_file(path).context("hash file")?;
        files.insert(rel, ScannedFile { sha256, size });
    }
    Ok(files)
}
