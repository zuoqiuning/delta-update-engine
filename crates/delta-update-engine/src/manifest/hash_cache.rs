//! Best-effort cache of install-tree file hashes.
//!
//! `check` hashes every file under the install root, which dominates the scan
//! for large installs. Entries are reused only when path, size and mtime all
//! match. Any load/save failure degrades to hashing the full tree.
//!
//! A stale entry cannot make the updater accept a tampered file: `prepare`
//! re-hashes every unchanged file before the commit point, so a local file
//! that was modified without changing its size and mtime makes the update fail
//! with `E_PATCH_SOURCE_MISMATCH` rather than being silently replaced.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

const CACHE_FILE: &str = "hash_cache.json";
const CACHE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct CacheEntry {
    pub size: u64,
    pub mtime_ns: u64,
    pub sha256: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct HashCache {
    v: u32,
    root: String,
    #[serde(default)]
    files: BTreeMap<String, CacheEntry>,
    #[serde(skip)]
    dirty: bool,
}

impl HashCache {
    /// Load the cache for `install_root`, or start empty when it is missing,
    /// corrupt, or belongs to a different install root.
    pub fn load(journal_root: &Path, install_root: &Path) -> Self {
        let install_root =
            dunce::canonicalize(install_root).unwrap_or_else(|_| install_root.into());
        let root = install_root.to_string_lossy().into_owned();
        let empty = || Self {
            v: CACHE_VERSION,
            root: root.clone(),
            files: BTreeMap::new(),
            dirty: false,
        };
        let Ok(bytes) = std::fs::read(journal_root.join(CACHE_FILE)) else {
            return empty();
        };
        match serde_json::from_slice::<Self>(&bytes) {
            Ok(mut cache) if cache.v == CACHE_VERSION && cache.root == root => {
                cache.dirty = false;
                cache
            }
            Ok(_) => empty(),
            Err(err) => {
                tracing::debug!(%err, "hash cache is unreadable; hashing the full tree");
                empty()
            }
        }
    }

    pub fn lookup(&self, path: &str, size: u64, mtime_ns: u64) -> Option<&str> {
        self.files
            .get(path)
            .filter(|entry| entry.size == size && entry.mtime_ns == mtime_ns)
            .map(|entry| entry.sha256.as_str())
    }

    pub fn insert(&mut self, path: &str, size: u64, mtime_ns: u64, sha256: String) {
        self.files.insert(
            path.to_string(),
            CacheEntry {
                size,
                mtime_ns,
                sha256,
            },
        );
        self.dirty = true;
    }

    /// Drop entries for files that no longer exist so the cache does not grow
    /// without bound across updates.
    pub fn retain(&mut self, present: &BTreeSet<String>) {
        let before = self.files.len();
        self.files.retain(|path, _| present.contains(path));
        self.dirty |= self.files.len() != before;
    }

    /// Persist the cache. Failures are logged and ignored: a missing cache only
    /// costs a full re-hash on the next check.
    pub fn save(&self, journal_root: &Path) {
        if !self.dirty {
            return;
        }
        let Ok(bytes) = serde_json::to_vec(self) else {
            return;
        };
        if let Err(err) = crate::fs::atomic::write_atomic(&journal_root.join(CACHE_FILE), &bytes) {
            tracing::warn!(%err, "failed to write hash cache");
        }
    }

    /// Drop the cache after a successful update so the next check re-hashes the
    /// replaced tree. Best-effort, like every other cache operation.
    pub fn clear(journal_root: &Path) {
        let _ = std::fs::remove_file(journal_root.join(CACHE_FILE));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_matches_size_and_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("journal");
        let install = dir.path().join("install");
        let mut cache = HashCache::load(&journal, &install);
        cache.insert("a.dll", 3, 42, "aa".into());
        cache.save(&journal);

        let cache = HashCache::load(&journal, &install);
        assert_eq!(cache.lookup("a.dll", 3, 42), Some("aa"));
        assert_eq!(cache.lookup("a.dll", 3, 43), None);
        assert_eq!(cache.lookup("a.dll", 4, 42), None);
        assert_eq!(cache.lookup("b.dll", 3, 42), None);
    }

    #[test]
    fn retain_prunes_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("journal");
        let install = dir.path().join("install");
        let mut cache = HashCache::load(&journal, &install);
        cache.insert("a.dll", 3, 42, "aa".into());
        cache.insert("b.dll", 3, 42, "bb".into());

        cache.retain(&BTreeSet::from(["b.dll".to_string()]));
        cache.save(&journal);

        let cache = HashCache::load(&journal, &install);
        assert_eq!(cache.lookup("a.dll", 3, 42), None);
        assert_eq!(cache.lookup("b.dll", 3, 42), Some("bb"));
    }

    #[test]
    fn other_root_or_corrupt_cache_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("journal");
        let mut cache = HashCache::load(&journal, &dir.path().join("install-a"));
        cache.insert("a.dll", 3, 42, "aa".into());
        cache.save(&journal);

        let other = HashCache::load(&journal, &dir.path().join("install-b"));
        assert_eq!(other.lookup("a.dll", 3, 42), None);

        std::fs::write(journal.join(CACHE_FILE), b"{not json").unwrap();
        let corrupt = HashCache::load(&journal, &dir.path().join("install-a"));
        assert_eq!(corrupt.lookup("a.dll", 3, 42), None);
    }
}
