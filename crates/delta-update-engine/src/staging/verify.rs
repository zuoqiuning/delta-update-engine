use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::crypto::sha256_file;
use crate::error::{EngineError, PatchError};

use super::plan::{PreparedPlan, StagedFile};

/// Re-verify a persisted PREPARED plan before reusing it.
pub fn verify_plan(plan: &PreparedPlan) -> Result<(), EngineError> {
    verify(plan, None)
}

/// Re-verify a plan, reusing hashes this process already computed.
///
/// `prepare` hashes every staged file right after writing it, so verifying the
/// same plan again in `begin_commit` would read every byte twice for no new
/// information. Pass the inventory of the prepare that produced the plan to
/// skip those hashes; anything the inventory does not know is still hashed.
pub fn verify_plan_with(
    plan: &PreparedPlan,
    inventory: &VerifiedInventory,
) -> Result<(), EngineError> {
    verify(plan, Some(inventory))
}

fn verify(plan: &PreparedPlan, inventory: Option<&VerifiedInventory>) -> Result<(), EngineError> {
    for file in &plan.files {
        let metadata = std::fs::metadata(&file.staging_path)
            .map_err(|err| EngineError::Internal(format!("{}: {err}", file.path)))?;
        if metadata.len() != file.size {
            return Err(PatchError::TargetHashMismatch(format!(
                "{}: staged size {} does not match {}",
                file.path,
                metadata.len(),
                file.size
            ))
            .into());
        }
        let Some(inventory) = inventory else {
            verify_hash(file)?;
            continue;
        };
        if inventory.is_fresh(&file.staging_path, &metadata) {
            continue;
        }
        verify_hash(file)?;
        inventory.record(&file.staging_path);
    }
    Ok(())
}

fn verify_hash(file: &StagedFile) -> Result<(), EngineError> {
    let actual = sha256_file(&file.staging_path)
        .map_err(|err| EngineError::Internal(format!("{}: {err}", file.path)))?;
    if actual != file.sha256 {
        return Err(PatchError::TargetHashMismatch(format!(
            "{}: staged hash does not match the manifest",
            file.path
        ))
        .into());
    }
    Ok(())
}

/// In-process record of files whose content this process hashed.
///
/// Two callers share it:
///
/// * `check` records the install files the diff will replace or remove, so
///   `begin_commit` can reuse their hashes for the rollback snapshot;
/// * `prepare` records every staged file, so `verify_plan` in `prepare` and in
///   `begin_commit` do not read staging twice.
///
/// A record is reused only while size and mtime still match the values
/// captured at hash time; any other file is hashed in full. The record never
/// leaves the process, so a restarted process (`resume_prepared`, crash
/// recovery) verifies every byte. Journal replay also hashes the staged source
/// before placing it, which keeps a file modified after the last check from
/// reaching the install root.
#[derive(Debug, Default)]
pub struct VerifiedInventory {
    stamps: Mutex<BTreeMap<PathBuf, Stamp>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    size: u64,
    mtime_ns: u64,
    /// Known when the caller hashed the install file itself. Staged files are
    /// recorded right after a hash too, but the caller already has the digest,
    /// so it is left out to keep `prepare` allocations small.
    sha256: Option<String>,
}

impl VerifiedInventory {
    /// Record that `path` was just verified. Filesystems that cannot report
    /// mtime are not recorded; the cost is one extra hash.
    pub(crate) fn record(&self, path: &Path) {
        let Some(metadata) = std::fs::metadata(path).ok() else {
            return;
        };
        let Some(mtime_ns) = mtime_ns(&metadata) else {
            return;
        };
        self.insert(
            path.to_path_buf(),
            Stamp {
                size: metadata.len(),
                mtime_ns,
                sha256: None,
            },
        );
    }

    /// Record a hash that the caller already computed, e.g. from the `check`
    /// scan, so later phases can reuse it without touching the file.
    pub(crate) fn record_hashed(&self, path: PathBuf, size: u64, mtime_ns: u64, sha256: String) {
        self.insert(
            path,
            Stamp {
                size,
                mtime_ns,
                sha256: Some(sha256),
            },
        );
    }

    /// Hash recorded for `path` if its size and mtime still match.
    pub(crate) fn sha256_if_fresh(
        &self,
        path: &Path,
        metadata: &std::fs::Metadata,
    ) -> Option<String> {
        let mtime_ns = mtime_ns(metadata)?;
        let stamps = self.lock();
        stamps
            .get(path)
            .filter(|stamp| stamp.size == metadata.len() && stamp.mtime_ns == mtime_ns)
            .and_then(|stamp| stamp.sha256.clone())
    }

    fn insert(&self, path: PathBuf, stamp: Stamp) {
        self.lock().insert(path, stamp);
    }

    fn is_fresh(&self, path: &Path, metadata: &std::fs::Metadata) -> bool {
        let Some(mtime_ns) = mtime_ns(metadata) else {
            return false;
        };
        self.lock()
            .get(path)
            .is_some_and(|stamp| stamp.size == metadata.len() && stamp.mtime_ns == mtime_ns)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<PathBuf, Stamp>> {
        self.stamps
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Forget every record, e.g. after the PREPARED plan was consumed.
    pub fn clear(&self) {
        self.lock().clear();
    }
}

/// Modification time as nanoseconds. `None` when the filesystem cannot report
/// it, in which case the file is always hashed rather than recorded.
fn mtime_ns(metadata: &std::fs::Metadata) -> Option<u64> {
    let elapsed = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some(elapsed.as_nanos().min(u128::from(u64::MAX)) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::sha256_bytes;

    fn plan_for(path: &Path, content: &[u8]) -> PreparedPlan {
        PreparedPlan {
            schema: crate::staging::plan::PREPARED_SCHEMA,
            version: "2.0.0".into(),
            manifest_hash: "hash".into(),
            staging_root: path.parent().unwrap().to_path_buf(),
            files: vec![StagedFile {
                path: "App.exe".into(),
                staging_path: path.to_path_buf(),
                sha256: sha256_bytes(content),
                size: content.len() as u64,
            }],
            removed: Vec::new(),
            all_files: vec!["App.exe".into()],
        }
    }

    fn staged(dir: &Path) -> PathBuf {
        let path = dir.join("files/App.exe");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"good").unwrap();
        path
    }

    /// Restore the mtime after an overwrite so the test can prove the size and
    /// mtime check is what gates the hash.
    fn overwrite_preserving_mtime(path: &Path, content: &[u8]) {
        let mtime = std::fs::metadata(path).unwrap().modified().unwrap();
        std::fs::write(path, content).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
    }

    #[test]
    fn recorded_file_is_not_hashed_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = staged(dir.path());
        let plan = plan_for(&path, b"good");
        let inventory = VerifiedInventory::default();
        verify_plan_with(&plan, &inventory).unwrap();

        overwrite_preserving_mtime(&path, b"evil");
        // Same size and mtime: the inventory trusts its record, like the
        // install-tree hash cache does. Replay hashes the source again.
        verify_plan_with(&plan, &inventory).unwrap();
        // A fresh process (or an explicit full verify) always hashes.
        assert!(verify_plan(&plan).is_err());
    }

    #[test]
    fn changed_mtime_or_size_falls_back_to_hashing() {
        let dir = tempfile::tempdir().unwrap();
        let path = staged(dir.path());
        let plan = plan_for(&path, b"good");
        let inventory = VerifiedInventory::default();
        verify_plan_with(&plan, &inventory).unwrap();

        // Same size, older mtime: the record is stale and the hash decides.
        std::fs::write(&path, b"evil").unwrap();
        let earlier = std::fs::metadata(&path).unwrap().modified().unwrap()
            - std::time::Duration::from_secs(1);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(earlier)
            .unwrap();
        let error = verify_plan_with(&plan, &inventory).unwrap_err();
        assert_eq!(error.code(), "E_PATCH_TARGET_HASH_MISMATCH");

        // A size change is rejected before any hash runs.
        std::fs::write(&path, b"longer").unwrap();
        let error = verify_plan_with(&plan, &inventory).unwrap_err();
        assert!(
            error.to_string().contains("staged size"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn clear_and_missing_files_are_safe() {
        let dir = tempfile::tempdir().unwrap();
        let path = staged(dir.path());
        let plan = plan_for(&path, b"good");
        let inventory = VerifiedInventory::default();
        inventory.record(&dir.path().join("missing"));
        verify_plan_with(&plan, &inventory).unwrap();

        inventory.clear();
        std::fs::remove_file(&path).unwrap();
        assert!(verify_plan_with(&plan, &inventory).is_err());
    }

    #[test]
    fn size_mismatch_is_reported_before_any_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = staged(dir.path());
        let plan = plan_for(&path, b"good!");
        let inventory = VerifiedInventory::default();
        inventory.record(&path);
        let error = verify_plan_with(&plan, &inventory).unwrap_err();
        assert!(
            error.to_string().contains("staged size 4 does not match 5"),
            "unexpected error: {error}"
        );
    }
}
