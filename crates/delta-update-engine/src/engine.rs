use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use semver::Version;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::EngineConfig;
use crate::crypto::{sha256_bytes, sha256_file, verify_manifest};
use crate::error::{EngineError, SignatureError};
use crate::history;
use crate::journal::state::{
    self, ApplyConfig, CommitStatus, APPLY_JSON, COMMIT_JOURNAL, COMMIT_META, COMMIT_STATUS,
    PREPARED_JSON, UPDATE_LOCK,
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
    /// The running version is below the manifest's `min_supported_version`.
    pub update_required: bool,
    /// The publisher's minimum supported version, when the manifest sets one.
    pub min_supported_version: Option<String>,
    /// Files skipped because they are cloud placeholders (OneDrive/Files
    /// On-Demand). They are not part of the diff, so if the manifest lists them
    /// they are downloaded in full. Surfaced so UIs can explain the size.
    pub cloud_placeholders_skipped: u32,
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
    raw_manifest: Vec<u8>,
    signature: String,
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
    /// Test/legacy escape hatch. Not part of `EngineConfig`: production code
    /// cannot reach it accidentally, only through [`Engine::new_legacy`].
    allow_unsigned_journal_recovery: bool,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Result<Self, EngineError> {
        Self::build(config, false)
    }

    /// Test/legacy constructor that permits replaying a journal without a
    /// signed-manifest binding. `#[doc(hidden)]`: it is not a supported
    /// production option, and the plugin/CLI never call it.
    #[doc(hidden)]
    pub fn new_legacy(
        config: EngineConfig,
        allow_unsigned_journal_recovery: bool,
    ) -> Result<Self, EngineError> {
        Self::build(config, allow_unsigned_journal_recovery)
    }

    fn build(config: EngineConfig, allow_unsigned: bool) -> Result<Self, EngineError> {
        config.validate()?;
        let current_version = Version::parse(&config.current_version)
            .map_err(|_| EngineError::VersionInvalid(config.current_version.clone()))?;
        if allow_unsigned {
            tracing::warn!(
                "allow_unsigned_journal_recovery is enabled: recovery may execute a journal that is not bound to a signed manifest"
            );
        }
        Ok(Self {
            config,
            current_version,
            session: Mutex::new(SessionState::default()),
            allow_unsigned_journal_recovery: allow_unsigned,
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
        let signature = std::str::from_utf8(&raw_signature)
            .map_err(|_| {
                EngineError::Signature(SignatureError::Invalid(
                    "signature is not valid UTF-8".into(),
                ))
            })?
            .to_string();
        verify_manifest(&raw_manifest, &signature, &self.config.signing_public_keys)?;
        let manifest = parse_manifest(&raw_manifest, &self.config.limits)?;
        let channel = manifest.channel.as_deref().unwrap_or("stable");
        if channel != self.config.channel {
            self.clear_verified()?;
            return Ok(self.outcome(false, None, 0, 0, 0, 0));
        }
        // Only a manifest for this client's channel can require an update.
        let min_supported_version = manifest.min_supported_version.clone();
        let update_required = manifest.is_update_required(&self.current_version);
        let with_support = |mut outcome: CheckOutcome| {
            outcome.update_required = update_required;
            outcome.min_supported_version = min_supported_version.clone();
            outcome
        };

        if state::read_skipped_version(&self.config.journal_root).as_deref()
            == Some(manifest.version.as_str())
        {
            if !manifest.rollback {
                self.clear_verified()?;
                return Ok(with_support(self.outcome(false, None, 0, 0, 0, 0)));
            }
            // An explicit rollback to this version clears the crash-loop skip.
            let _ = std::fs::remove_file(self.config.journal_root.join(state::SKIPPED_VERSION));
        }

        let current_version = self.current_version.to_string();
        let decision = decide(&manifest.version, &current_version, manifest.rollback)?;
        if decision == VersionDecision::NoUpdate {
            self.clear_verified()?;
            return Ok(with_support(self.outcome(false, None, 0, 0, 0, 0)));
        }

        let (diff, placeholders_skipped) = tokio::task::spawn_blocking({
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
        outcome.cloud_placeholders_skipped = placeholders_skipped;
        self.set_verified(VerifiedUpdate {
            manifest,
            manifest_hash: sha256_bytes(&raw_manifest),
            raw_manifest,
            signature,
            diff,
        })?;
        Ok(with_support(outcome))
    }

    /// Scan the install root and classify every manifest entry against it.
    ///
    /// `.old/` and `.trash/` are bookkeeping directories owned by the update
    /// process and never participate in a user-visible diff. File hashes are
    /// cached by size + mtime under the journal root so repeated checks of a
    /// large install do not re-hash unchanged files. `check` runs this on the
    /// blocking pool.
    pub fn diff(&self, local_root: &Path, remote: &Manifest) -> Result<DiffResult, EngineError> {
        diff_install(local_root, &self.config.journal_root, remote).map(|(diff, _)| diff)
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
            journal_root: &self.config.journal_root,
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
        // A second instance must not overwrite the pending commit intent of
        // the first one. The worker consumes the journal after this lock is
        // released, so the existence check is the cross-process hand-off.
        if self.config.journal_root.join(COMMIT_JOURNAL).exists() {
            return Err(EngineError::UpdateLocked);
        }
        let prepared_path = self.config.journal_root.join(PREPARED_JSON);
        if !prepared_path.exists() {
            return Err(EngineError::NotPrepared);
        }
        let plan = PreparedPlan::load(&prepared_path)?;
        if plan.version != prepared.version || plan.manifest_hash != prepared.manifest_hash {
            return Err(EngineError::NotPrepared);
        }
        verify_plan(&plan)?;

        // Capture the old bytes before the worker moves them. Rollback verifies
        // Restore against this snapshot instead of trusting the backup file's
        // current hash (which may have been corrupted or tampered with).
        let mut old_hashes = state::OldHashes::new();
        for path in plan
            .files
            .iter()
            .map(|file| &file.path)
            .chain(plan.removed.iter())
        {
            let file = self.config.install_root.join(path);
            let metadata = match std::fs::metadata(&file) {
                Ok(metadata) => metadata,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    return Err(EngineError::Internal(format!(
                        "old hash {path}: cannot read metadata: {err}"
                    )))
                }
            };
            if !metadata.is_file() {
                continue;
            }
            let sha256 = sha256_file(&file)
                .map_err(|err| EngineError::Internal(format!("old hash {path}: {err}")))?;
            old_hashes.insert(
                path.clone(),
                state::OldFileHash {
                    sha256,
                    size: metadata.len(),
                },
            );
        }
        state::write_old_hashes(&self.config.journal_root, &old_hashes)?;

        let commit_id = Uuid::new_v4().to_string();
        match state::read_verified_manifest(&self.config.journal_root)? {
            Some((raw, _signature)) => {
                if sha256_bytes(&raw) != plan.manifest_hash {
                    return Err(EngineError::JournalCorrupt(
                        "verified manifest does not match the PREPARED plan".into(),
                    ));
                }
                state::write_commit_meta(
                    &self.config.journal_root,
                    &state::CommitMeta {
                        manifest_hash: plan.manifest_hash.clone(),
                        version: plan.version.clone(),
                        from_version: self.config.current_version.clone(),
                        install_root: self.config.install_root.clone(),
                        commit_id: commit_id.clone(),
                    },
                )?;
            }
            None if self.allow_unsigned_journal_recovery => {
                // Test/legacy mode: the journal cannot be bound to a signed
                // manifest, and recovery will only run it when the same
                // explicit flag is set.
            }
            None => {
                return Err(EngineError::NotPrepared);
            }
        }
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
        history::mark_started(
            &self.config.journal_root,
            &plan.version,
            &self.config.current_version,
        );

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
        let _ = std::fs::remove_file(journal_root.join(COMMIT_META));
        state::remove_old_hashes(journal_root);
        history::clear_started(journal_root);
        Ok(())
    }

    /// Clear the rollback skip marker so the same version can be retried.
    ///
    /// `check` skips a version that was rolled back unless the manifest carries
    /// `rollback: true`. Publishers sometimes fix and re-release the same
    /// version, so this is the manual escape hatch.
    pub fn clear_skipped_version(&self) -> Result<(), EngineError> {
        state::clear_skipped_version(&self.config.journal_root);
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
        state::promote_verified_to_installed(journal_root)?;

        let _ = std::fs::remove_file(journal_root.join(COMMIT_JOURNAL));
        let _ = std::fs::remove_file(journal_root.join(COMMIT_STATUS));
        let _ = std::fs::remove_file(journal_root.join(COMMIT_META));
        let _ = std::fs::remove_file(journal_root.join(APPLY_JSON));
        let _ = std::fs::remove_file(&prepared_path);
        state::remove_old_hashes(journal_root);
        HashCache::clear(journal_root);
        crate::fs::remove_dir_all_best_effort(&self.config.staging_root.join(&version));
        for (label, path) in [
            (
                "backup",
                rollback::backup_dir(&self.config.install_root, &commit_id),
            ),
            (
                "trash",
                rollback::trash_dir(&self.config.install_root, &commit_id),
            ),
        ] {
            if !crate::fs::remove_dir_all_best_effort(&path) {
                tracing::warn!(
                    kind = label,
                    path = %path.display(),
                    "cleanup left files behind (locked or still in use); they are harmless but occupy disk"
                );
            }
        }
        if state::read_skipped_version(journal_root).as_deref() != Some(version.as_str()) {
            let _ = std::fs::remove_file(journal_root.join(state::SKIPPED_VERSION));
        }
        history::record_completed(journal_root, &version, None);
        Ok(())
    }

    /// Startup entry point: replay a pending journal, detect a crash loop and
    /// roll back, reuse a PREPARED plan, or clean up.
    ///
    /// Counts one startup attempt. Use [`Self::recover_after_elevation`] when
    /// an elevated worker already performed the recovery for this start.
    pub fn recover_and_cleanup(&self) -> Result<StartupOutcome, EngineError> {
        let attempts =
            state::increment_attempts(&self.config.journal_root, self.config.crash_loop_threshold)?;
        let outcome = self.recover_with_attempts(attempts)?;
        self.record_startup_outcome(&outcome);
        Ok(outcome)
    }

    /// Read the newest persisted update outcomes for the host application to
    /// display or forward to its own monitoring. Nothing is uploaded here.
    pub fn update_history(&self, limit: usize) -> Vec<crate::history::UpdateRecord> {
        history::read(&self.config.journal_root, limit)
    }

    fn record_startup_outcome(&self, outcome: &StartupOutcome) {
        match outcome {
            StartupOutcome::RolledBack { reason, .. } => {
                history::record_rolled_back(&self.config.journal_root, reason);
            }
            StartupOutcome::NeedsAttention { error } => {
                history::record_failed(&self.config.journal_root, error.code());
            }
            _ => {}
        }
    }

    /// A normal shutdown of a committed but unconfirmed version proves the
    /// process did not crash. Discount the startup attempt so "open, look,
    /// close" does not accumulate into a rollback; real crashes never reach the
    /// exit hook and keep counting.
    ///
    /// Callers gate this on a grace period and on the update awaiting
    /// confirmation. They must also ensure the commit really reached
    /// `CommitStatus::Completed`: this method deliberately no-ops otherwise, so
    /// a future refactor that splits `begin_commit` from the worker spawn must
    /// not call it between those steps.
    pub fn discount_clean_exit(&self) -> Result<(), EngineError> {
        let journal_root = &self.config.journal_root;
        if !matches!(
            state::read_commit_status(journal_root)?,
            Some(CommitStatus::Completed { .. })
        ) {
            return Ok(());
        }
        let _lock = UpdateLock::acquire(&journal_root.join(UPDATE_LOCK))?;
        let attempts = state::read_attempts(journal_root);
        if attempts == 0 {
            return Ok(());
        }
        state::write_attempts(journal_root, attempts - 1, self.config.crash_loop_threshold)?;
        tracing::info!(
            remaining = attempts - 1,
            "clean exit discounted one unconfirmed start"
        );
        Ok(())
    }

    /// Inspect the result of an elevated recovery worker without counting a
    /// second startup attempt. The worker increments the counter once.
    pub fn recover_after_elevation(&self) -> Result<StartupOutcome, EngineError> {
        let outcome =
            self.recover_with_attempts(state::read_attempts(&self.config.journal_root))?;
        self.record_startup_outcome(&outcome);
        Ok(outcome)
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
            trusted_keys: &self.config.signing_public_keys,
            accept_unsigned_journal: self.allow_unsigned_journal_recovery,
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
        state::write_verified_manifest(
            &self.config.journal_root,
            &verified.raw_manifest,
            &verified.signature,
        )?;
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
            update_required: false,
            min_supported_version: None,
            cloud_placeholders_skipped: 0,
        }
    }
}

fn diff_install(
    install_root: &Path,
    journal_root: &Path,
    remote: &Manifest,
) -> Result<(DiffResult, u32), EngineError> {
    let mut cache = HashCache::load(journal_root, install_root);
    let (local, placeholders_skipped) = scan_local(install_root, &mut cache)?;
    let known = KnownFiles::load(&journal_root.join("known_files.json"))?;
    let result = diff(&local, remote, &known.files)?;
    cache.save(journal_root);
    Ok((result, placeholders_skipped))
}

/// Scan the install root. Returns the local manifest and the number of cloud
/// placeholder files that were intentionally left out of the diff.
fn scan_local(root: &Path, cache: &mut HashCache) -> Result<(LocalManifest, u32), EngineError> {
    let mut files = LocalManifest::new();
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
    Ok((files, placeholders_skipped))
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
        let (first, skipped) = scan_local(root, &mut cache).unwrap();
        assert_eq!(skipped, 0);
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
