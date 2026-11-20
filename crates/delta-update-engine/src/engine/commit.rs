//! Commit hand-off, skip markers and health confirmation.

use semver::Version;
use uuid::Uuid;

use crate::crypto::{sha256_bytes, sha256_file};
use crate::error::EngineError;
use crate::history;
use crate::journal::plan as journal_plan;
use crate::journal::state::{
    self, ApplyConfig, CommitStatus, APPLY_JSON, COMMIT_JOURNAL, COMMIT_META, COMMIT_STATUS,
    PREPARED_JSON, UPDATE_LOCK,
};
use crate::journal::UpdateLock;
use crate::manifest::hash_cache::HashCache;
use crate::manifest::history::KnownFiles;
use crate::rollback;
use crate::staging::{verify_plan, verify_plan_with, PreparedPlan};

use super::{acquire_update_lock_retrying, Engine, PendingCommit, PreparedUpdate};

impl Engine {
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
        // A plan produced by this process was already hashed file by file during
        // prepare; a plan resumed by another process has no inventory and is
        // verified in full.
        match self.staged_inventory() {
            Some(inventory) => verify_plan_with(&plan, &inventory)?,
            None => verify_plan(&plan)?,
        }

        // Capture the old bytes before the worker moves them. Rollback verifies
        // Restore against this snapshot instead of trusting the backup file's
        // current hash (which may have been corrupted or tampered with).
        //
        // `check` already hashed the same files in this process. Reuse that
        // hash while size and mtime still match: an edited file changes mtime,
        // so it is hashed again. A plan resumed by another process has no
        // inventory and takes the slow path.
        let scan = self.scan_inventory();
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
            if let Some(sha256) = scan
                .as_deref()
                .and_then(|scan| scan.sha256_if_fresh(&file, &metadata))
            {
                old_hashes.insert(
                    path.clone(),
                    state::OldFileHash {
                        sha256,
                        size: metadata.len(),
                    },
                );
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
                        schema: state::COMMIT_META_SCHEMA,
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

    /// Skip `version` so `check` stops offering it.
    ///
    /// The marker survives restarts and is cleared by a newer release or by
    /// [`Self::clear_skipped_version`]. The UI uses this for "later": the next
    /// check reports no update instead of nagging on every launch.
    pub fn skip_version(&self, version: &str) -> Result<(), EngineError> {
        Version::parse(version).map_err(|_| EngineError::VersionInvalid(version.to_string()))?;
        state::write_skipped_version(&self.config.journal_root, version)
    }

    /// The version skipped by the user or recorded by a rollback, if any.
    pub fn skipped_version(&self) -> Option<String> {
        state::read_skipped_version(&self.config.journal_root)
    }

    /// Mark the running version healthy: reset crash-loop attempts and clean
    /// up backups, journals and the consumed PREPARED plan.
    ///
    /// Without a completed commit there is nothing to clean. The counter is
    /// reset without the update lock unless a commit journal exists (a worker may
    /// be counting this start); that keeps health confirmation working while a
    /// long prepare holds the lock for its whole download.
    pub fn confirm_healthy(&self) -> Result<(), EngineError> {
        let journal_root = &self.config.journal_root;
        if !matches!(
            state::read_commit_status(journal_root)?,
            Some(CommitStatus::Completed { .. })
        ) {
            // A worker may be incrementing the counter for this very start. Take
            // the lock only when a commit journal exists; otherwise stay
            // lock-free so a long prepare cannot block health confirmation.
            if journal_root.join(COMMIT_JOURNAL).exists() {
                let _lock = acquire_update_lock_retrying(journal_root)?;
                state::reset_attempts(journal_root)?;
            } else {
                state::reset_attempts(journal_root)?;
            }
            return Ok(());
        }

        let _lock = acquire_update_lock_retrying(journal_root)?;
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
        // The plan is gone; drop the staging hashes with it.
        if let Some(inventory) = self.staged_inventory() {
            inventory.clear();
        }
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
        // The install tree is now the new version, so the last check snapshot
        // describes a tree that no longer exists. Drop it; the next prepare
        // checks again instead of staging against stale diffs.
        self.clear_verified()?;
        Ok(())
    }
}
