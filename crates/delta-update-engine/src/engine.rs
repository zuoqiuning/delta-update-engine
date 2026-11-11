use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use semver::Version;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::EngineConfig;
use crate::crypto::{sha256_bytes, sha256_file, verify_manifest};
use crate::error::{EngineError, SignatureError};
use crate::journal::state::{
    self, ApplyConfig, CommitStatus, APPLY_JSON, COMMIT_JOURNAL, COMMIT_STATUS, PREPARED_JSON,
    UPDATE_LOCK,
};
use crate::journal::{plan as journal_plan, UpdateLock};
use crate::manifest::diff::{diff, DiffResult, LocalFile, LocalManifest};
use crate::manifest::hash_cache::HashCache;
use crate::manifest::history::KnownFiles;
use crate::manifest::model::Manifest;
use crate::manifest::parse_manifest;
use crate::recovery::{self, RecoveryRoots};
use crate::rollback;
use crate::staging::{prepare as stage, verify_plan, PrepareInput, PreparedPlan};
use crate::version::{decide, VersionDecision};

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CheckOutcome {
    pub available: bool,
    pub version: Option<String>,
    pub current_version: String,
    /// Changed files including removals, for display.
    pub files_changed: u32,
    /// Files that will be materialized in staging (modified + added).
    pub files_staged: u32,
    pub download_bytes: u64,
    pub total_bytes: u64,
    /// Release notes from the manifest, if any.
    pub notes: Option<String>,
    /// The publisher marked this update as mandatory.
    pub mandatory: bool,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Progress {
    Downloading {
        file: String,
        total: u64,
        completed: u64,
    },
    Applying {
        file: String,
    },
    Verifying {
        file: String,
    },
    Done,
}

#[derive(Debug, Clone)]
pub struct PreparedUpdate {
    pub version: String,
    pub staging_root: PathBuf,
    pub manifest_hash: String,
}

#[derive(Debug, Clone)]
pub struct PendingCommit {
    pub commit_id: String,
    pub version: String,
    pub journal_path: PathBuf,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum StartupOutcome {
    Clean,
    Recovered {
        ops_completed: usize,
        version: String,
    },
    Committed {
        version: String,
    },
    RolledBack {
        to_version: String,
        reason: String,
    },
    ReusedPrepared {
        version: String,
    },
    NeedsAttention {
        error: EngineError,
    },
}

/// The manifest that passed signature verification in the last successful
/// check, plus the diff computed against it. Prepare consumes this snapshot so
/// callers can never substitute an unverified manifest.
#[derive(Clone)]
struct VerifiedUpdate {
    manifest: Manifest,
    manifest_hash: String,
    diff: DiffResult,
}

struct SessionState {
    verified: Option<VerifiedUpdate>,
    cancel: CancellationToken,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            verified: None,
            cancel: CancellationToken::new(),
        }
    }
}

/// Main engine handle. Holds configuration and no Tauri types.
pub struct Engine {
    config: EngineConfig,
    current_version: Version,
    session: Mutex<SessionState>,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Result<Self, EngineError> {
        config.validate()?;
        let current_version = Version::parse(&config.current_version)
            .map_err(|_| EngineError::VersionInvalid(config.current_version.clone()))?;
        Ok(Self {
            config,
            current_version,
            session: Mutex::new(SessionState::default()),
        })
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// Fetch, verify, parse, apply channel/version guards, then diff against
    /// the install root. The verified snapshot is cached for `prepare`.
    pub async fn check(&self) -> Result<CheckOutcome, EngineError> {
        let (raw_manifest, raw_signature) = tokio::try_join!(
            self.config.fetcher.fetch_manifest(),
            self.config.fetcher.fetch_signature()
        )?;
        let signature = std::str::from_utf8(&raw_signature).map_err(|_| {
            EngineError::Signature(SignatureError::Invalid(
                "signature is not valid UTF-8".into(),
            ))
        })?;
        verify_manifest(&raw_manifest, signature, &self.config.signing_public_keys)?;
        let manifest = parse_manifest(&raw_manifest, &self.config.limits)?;

        let channel = manifest.channel.as_deref().unwrap_or("stable");
        if channel != self.config.channel {
            self.clear_verified()?;
            return Ok(self.outcome(false, None, 0, 0, 0, 0));
        }
        if state::read_skipped_version(&self.config.journal_root).as_deref()
            == Some(manifest.version.as_str())
        {
            if !manifest.rollback {
                self.clear_verified()?;
                return Ok(self.outcome(false, None, 0, 0, 0, 0));
            }
            // An explicit rollback to this version clears the crash-loop skip.
            let _ = std::fs::remove_file(self.config.journal_root.join(state::SKIPPED_VERSION));
        }

        let current_version = self.current_version.to_string();
        let decision = decide(&manifest.version, &current_version, manifest.rollback)?;
        if decision == VersionDecision::NoUpdate {
            self.clear_verified()?;
            return Ok(self.outcome(false, None, 0, 0, 0, 0));
        }

        let diff = tokio::task::spawn_blocking({
            let install_root = self.config.install_root.clone();
            let journal_root = self.config.journal_root.clone();
            let manifest = manifest.clone();
            move || diff_install(&install_root, &journal_root, &manifest)
        })
        .await
        .map_err(|err| EngineError::Internal(format!("scan task: {err}")))??;
        let files_staged =
            u32::try_from(diff.modified.len() + diff.added.len()).unwrap_or(u32::MAX);
        let files_changed =
            files_staged.saturating_add(u32::try_from(diff.removed.len()).unwrap_or(u32::MAX));
        let mut outcome = self.outcome(
            true,
            Some(manifest.version.clone()),
            files_changed,
            files_staged,
            diff.download_bytes,
            diff.total_bytes,
        );
        outcome.notes = manifest.notes.clone();
        outcome.mandatory = manifest.mandatory;
        self.set_verified(VerifiedUpdate {
            manifest,
            manifest_hash: sha256_bytes(&raw_manifest),
            diff,
        })?;
        Ok(outcome)
    }

    /// Scan the install root and classify every manifest entry against it.
    ///
    /// `.old/` and `.trash/` are bookkeeping directories owned by the update
    /// process and never participate in a user-visible diff. File hashes are
    /// cached by size + mtime under the journal root so repeated checks of a
    /// large install do not re-hash unchanged files. `check` runs this on the
    /// blocking pool.
    pub fn diff(&self, local_root: &Path, remote: &Manifest) -> Result<DiffResult, EngineError> {
        diff_install(local_root, &self.config.journal_root, remote)
    }

    /// Prepare the version that passed `check`. Runs a check first when no
    /// verified manifest is cached, mirroring the documented command contract.
    pub async fn prepare(
        &self,
        progress: Box<dyn Fn(Progress) + Send + Sync>,
    ) -> Result<PreparedUpdate, EngineError> {
        let cancel = {
            let mut session = self
                .session
                .lock()
                .map_err(|_| EngineError::Internal("engine state poisoned".into()))?;
            session.cancel = CancellationToken::new();
            session.cancel.clone()
        };
        let verified = self.ensure_verified().await?;
        // Hold the cross-process lock across staging and PREPARED persistence so
        // two instances can never interleave writes to the same journal root.
        let _lock = UpdateLock::acquire(&self.config.journal_root.join(UPDATE_LOCK))?;
        let plan = stage(&PrepareInput {
            manifest: &verified.manifest,
            diff: &verified.diff,
            fetcher: self.config.fetcher.as_ref(),
            install_root: &self.config.install_root,
            staging_root: &self.config.staging_root,
            version: &verified.manifest.version,
            manifest_hash: &verified.manifest_hash,
            limits: &self.config.limits,
            cancel: &cancel,
            progress: progress.as_ref(),
        })
        .await?;
        // Hashing every staged file can take a while; keep it off the async
        // runtime so downloads and command handling stay responsive.
        let plan = tokio::task::spawn_blocking(move || {
            verify_plan(&plan)?;
            Ok::<PreparedPlan, EngineError>(plan)
        })
        .await
        .map_err(|err| EngineError::Internal(format!("verify task: {err}")))??;
        plan.save(&self.config.journal_root.join("prepared.json"))?;
        progress(Progress::Done);
        Ok(PreparedUpdate {
            version: plan.version,
            staging_root: plan.staging_root,
            manifest_hash: plan.manifest_hash,
        })
    }

    /// Cancel the prepare currently in flight. Temporary files are kept for
    /// resume by the next prepare attempt.
    pub fn cancel_prepare(&self) -> Result<(), EngineError> {
        self.session
            .lock()
            .map_err(|_| EngineError::Internal("engine state poisoned".into()))?
            .cancel
            .cancel();
        Ok(())
    }

    /// Persist the journal and apply config before the app exits. The
    /// replacement process is the only consumer of the result.
    pub fn begin_commit(&self, prepared: &PreparedUpdate) -> Result<PendingCommit, EngineError> {
        let _lock = UpdateLock::acquire(&self.config.journal_root.join(UPDATE_LOCK))?;
        let prepared_path = self.config.journal_root.join(PREPARED_JSON);
        if !prepared_path.exists() {
            return Err(EngineError::NotPrepared);
        }
        let plan = PreparedPlan::load(&prepared_path)?;
        if plan.version != prepared.version || plan.manifest_hash != prepared.manifest_hash {
            return Err(EngineError::NotPrepared);
        }
        verify_plan(&plan)?;

        let commit_id = Uuid::new_v4().to_string();
        let journal = journal_plan::build(
            &plan,
            &commit_id,
            &self.config.current_version,
            &self.config.install_root,
        )?;
        let apply_config = ApplyConfig {
            v: 1,
            install_root: self.config.install_root.clone(),
            staging_root: self.config.staging_root.clone(),
            journal_root: self.config.journal_root.clone(),
            version: plan.version.clone(),
            commit_id: commit_id.clone(),
            parent_pid: std::process::id(),
        };
        state::write_apply_config(&self.config.journal_root, &apply_config)?;
        let journal_path = self.config.journal_root.join(COMMIT_JOURNAL);
        journal.write_atomic(&journal_path)?;

        Ok(PendingCommit {
            commit_id,
            version: plan.version,
            journal_path,
        })
    }

    /// Withdraw a pending commit whose worker never started.
    ///
    /// `begin_commit` is the commit point, so the journal must only be removed
    /// when no worker consumed it. This is used when the replacement process
    /// could not be spawned (for example the user declined the UAC prompt):
    /// the next start then reuses the PREPARED plan instead of replaying the
    /// journal without the required privileges. A journal that already has a
    /// status is left for the normal recovery path.
    pub fn discard_pending_commit(&self) -> Result<(), EngineError> {
        let journal_root = &self.config.journal_root;
        let _lock = UpdateLock::acquire(&journal_root.join(UPDATE_LOCK))?;
        if state::read_commit_status(journal_root)?.is_some() {
            return Ok(());
        }
        let _ = std::fs::remove_file(journal_root.join(COMMIT_JOURNAL));
        let _ = std::fs::remove_file(journal_root.join(APPLY_JSON));
        Ok(())
    }

    /// Mark the running version healthy: reset crash-loop attempts and clean
    /// up backups, journals and the consumed PREPARED plan.
    ///
    /// Without a completed commit there is nothing to clean, so the counter is
    /// reset without taking the update lock. This keeps health confirmation
    /// working while a long prepare holds the lock for its whole download.
    pub fn confirm_healthy(&self) -> Result<(), EngineError> {
        let journal_root = &self.config.journal_root;
        if !matches!(
            state::read_commit_status(journal_root)?,
            Some(CommitStatus::Completed { .. })
        ) {
            state::reset_attempts(journal_root)?;
            return Ok(());
        }

        let _lock = UpdateLock::acquire(&journal_root.join(UPDATE_LOCK))?;
        state::reset_attempts(journal_root)?;
        let Some(CommitStatus::Completed { version, commit_id }) =
            state::read_commit_status(journal_root)?
        else {
            return Ok(());
        };

        let prepared_path = journal_root.join(PREPARED_JSON);
        if let Ok(plan) = PreparedPlan::load(&prepared_path) {
            if plan.version == version {
                let known = KnownFiles {
                    files: plan.all_files.into_iter().collect(),
                };
                known.save(&journal_root.join("known_files.json"))?;
            }
        }

        let _ = std::fs::remove_file(journal_root.join(COMMIT_JOURNAL));
        let _ = std::fs::remove_file(journal_root.join(COMMIT_STATUS));
        let _ = std::fs::remove_file(journal_root.join(APPLY_JSON));
        let _ = std::fs::remove_file(&prepared_path);
        HashCache::clear(journal_root);
        crate::fs::remove_dir_all_best_effort(&self.config.staging_root.join(&version));
        crate::fs::remove_dir_all_best_effort(&rollback::backup_dir(
            &self.config.install_root,
            &commit_id,
        ));
        crate::fs::remove_dir_all_best_effort(&rollback::trash_dir(
            &self.config.install_root,
            &commit_id,
        ));
        if state::read_skipped_version(journal_root).as_deref() != Some(version.as_str()) {
            let _ = std::fs::remove_file(journal_root.join(state::SKIPPED_VERSION));
        }
        Ok(())
    }

    /// Startup entry point: replay a pending journal, detect a crash loop and
    /// roll back, reuse a PREPARED plan, or clean up.
    ///
    /// Counts one startup attempt. Use [`Self::recover_after_elevation`] when
    /// an elevated worker already performed the recovery for this start.
    pub fn recover_and_cleanup(&self) -> Result<StartupOutcome, EngineError> {
        let attempts = state::increment_attempts(&self.config.journal_root)?;
        self.recover_with_attempts(attempts)
    }

    /// Inspect the result of an elevated recovery worker without counting a
    /// second startup attempt. The worker increments the counter once.
    pub fn recover_after_elevation(&self) -> Result<StartupOutcome, EngineError> {
        self.recover_with_attempts(state::read_attempts(&self.config.journal_root))
    }

    /// Whether recovering this startup needs write access to the install root.
    ///
    /// Only a pending journal, a failed commit, or a completed commit past the
    /// crash-loop threshold needs writes; a normal start after an update does
    /// not.
    pub fn recovery_needs_install_write(&self) -> Result<bool, EngineError> {
        recovery::needs_install_write(&self.config.journal_root, self.config.crash_loop_threshold)
    }

    fn recover_with_attempts(&self, attempts: u64) -> Result<StartupOutcome, EngineError> {
        recovery::recover(&RecoveryRoots {
            install_root: &self.config.install_root,
            staging_root: &self.config.staging_root,
            journal_root: &self.config.journal_root,
            crash_loop_threshold: self.config.crash_loop_threshold,
            attempts,
        })
    }

    /// Rebuild the persisted PREPARED update after a restart.
    pub fn resume_prepared(&self) -> Result<PreparedUpdate, EngineError> {
        let path = self.config.journal_root.join(PREPARED_JSON);
        if !path.exists() {
            return Err(EngineError::NotPrepared);
        }
        let plan = PreparedPlan::load(&path)?;
        verify_plan(&plan)?;
        Ok(PreparedUpdate {
            version: plan.version,
            staging_root: plan.staging_root,
            manifest_hash: plan.manifest_hash,
        })
    }

    async fn ensure_verified(&self) -> Result<VerifiedUpdate, EngineError> {
        if let Some(verified) = self.cached_verified()? {
            return Ok(verified);
        }
        self.check().await?;
        self.cached_verified()?.ok_or(EngineError::NotPrepared)
    }

    fn cached_verified(&self) -> Result<Option<VerifiedUpdate>, EngineError> {
        Ok(self
            .session
            .lock()
            .map_err(|_| EngineError::Internal("engine state poisoned".into()))?
            .verified
            .clone())
    }

    fn set_verified(&self, verified: VerifiedUpdate) -> Result<(), EngineError> {
        self.session
            .lock()
            .map_err(|_| EngineError::Internal("engine state poisoned".into()))?
            .verified = Some(verified);
        Ok(())
    }

    fn clear_verified(&self) -> Result<(), EngineError> {
        self.session
            .lock()
            .map_err(|_| EngineError::Internal("engine state poisoned".into()))?
            .verified = None;
        Ok(())
    }

    fn outcome(
        &self,
        available: bool,
        version: Option<String>,
        files_changed: u32,
        files_staged: u32,
        download_bytes: u64,
        total_bytes: u64,
    ) -> CheckOutcome {
        CheckOutcome {
            available,
            version,
            current_version: self.config.current_version.clone(),
            files_changed,
            files_staged,
            download_bytes,
            total_bytes,
            notes: None,
            mandatory: false,
        }
    }
}

fn diff_install(
    install_root: &Path,
    journal_root: &Path,
    remote: &Manifest,
) -> Result<DiffResult, EngineError> {
    let mut cache = HashCache::load(journal_root, install_root);
    let local = scan_local(install_root, &mut cache)?;
    let known = KnownFiles::load(&journal_root.join("known_files.json"))?;
    let result = diff(&local, remote, &known.files)?;
    cache.save(journal_root);
    Ok(result)
}

fn scan_local(root: &Path, cache: &mut HashCache) -> Result<LocalManifest, EngineError> {
    let mut files = LocalManifest::new();
    let mut present = BTreeSet::new();
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
        if matches!(relative.split('/').next(), Some(".old" | ".trash")) {
            continue;
        }
        let metadata = entry
            .metadata()
            .map_err(|err| io_error(err.into(), &relative))?;
        let size = metadata.len();
        let mtime = mtime_ns(&metadata);
        present.insert(relative.clone());
        let sha256 = match mtime.and_then(|mtime| cache.lookup(&relative, size, mtime)) {
            Some(sha256) => sha256.to_string(),
            None => {
                let sha256 = sha256_file(entry.path()).map_err(|err| io_error(err, &relative))?;
                if let Some(mtime) = mtime {
                    cache.insert(&relative, size, mtime, sha256.clone());
                }
                sha256
            }
        };
        files.insert(relative, LocalFile { sha256, size });
    }
    cache.retain(&present);
    Ok(files)
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
        let first = scan_local(root, &mut cache).unwrap();
        assert_eq!(first["app.exe"].sha256, crate::crypto::sha256_bytes(b"v1"));

        // Same size and mtime must reuse the cached hash verbatim.
        let metadata = std::fs::metadata(root.join("app.exe")).unwrap();
        cache.insert(
            "app.exe",
            metadata.len(),
            mtime_ns(&metadata).unwrap(),
            "deadbeef".into(),
        );
        assert_eq!(
            scan_local(root, &mut cache).unwrap()["app.exe"].sha256,
            "deadbeef"
        );

        // A size change invalidates the entry and re-hashes the file.
        std::fs::write(root.join("app.exe"), b"v2-longer").unwrap();
        assert_eq!(
            scan_local(root, &mut cache).unwrap()["app.exe"].sha256,
            crate::crypto::sha256_bytes(b"v2-longer")
        );
    }
}
