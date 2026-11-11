//! Startup recovery shared by the engine and the elevated recovery worker.
//!
//! The engine calls this with a fresh startup attempt counter. A per-machine
//! install cannot write its own root, so the plugin may first run the same
//! recovery in an elevated worker and then inspect the result with
//! `Engine::recover_after_elevation`, which reuses the counter instead of
//! counting a second start.

use std::path::Path;

use crate::engine::StartupOutcome;
use crate::error::EngineError;
use crate::journal::state::{
    self, ApplyConfig, CommitStatus, COMMIT_JOURNAL, PREPARED_JSON, ROLLBACK_JOURNAL, UPDATE_LOCK,
};
use crate::journal::{replay, Journal, JournalOutcome, ReplayContext, UpdateLock};
use crate::rollback;
use crate::staging::{verify_plan, PreparedPlan};

/// Roots and attempt counter for one startup recovery pass.
pub(crate) struct RecoveryRoots<'a> {
    pub install_root: &'a Path,
    pub staging_root: &'a Path,
    pub journal_root: &'a Path,
    pub crash_loop_threshold: u32,
    /// Number of unconfirmed starts including this one.
    pub attempts: u64,
}

/// Whether startup must write to the install root.
///
/// A completed update that is merely waiting for its health confirmation is
/// only rolled back once the attempt counter exceeds the threshold, so it must
/// not trigger a UAC prompt on every normal start. Call this before the
/// attempt counter is incremented.
pub(crate) fn needs_install_write(
    journal_root: &Path,
    crash_loop_threshold: u32,
) -> Result<bool, EngineError> {
    if journal_root.join(ROLLBACK_JOURNAL).exists() {
        return Ok(true);
    }
    if !journal_root.join(COMMIT_JOURNAL).exists() {
        return Ok(false);
    }
    match state::read_commit_status(journal_root)? {
        Some(CommitStatus::Completed { .. }) => Ok(state::read_attempts(journal_root)
            .saturating_add(1)
            > u64::from(crash_loop_threshold)),
        // Missing or failed status means the journal may still need a retry.
        _ => Ok(true),
    }
}

/// Replay a pending journal, roll back a crash loop, or reuse a PREPARED plan.
pub(crate) fn recover(roots: &RecoveryRoots<'_>) -> Result<StartupOutcome, EngineError> {
    let journal_root = roots.journal_root;
    let _lock = UpdateLock::acquire(&journal_root.join(UPDATE_LOCK))?;
    let attempts = roots.attempts;

    let rollback_path = journal_root.join(ROLLBACK_JOURNAL);
    if rollback_path.exists() {
        let journal = match read_journal(&rollback_path) {
            Ok(journal) => journal,
            Err(error) => return Ok(StartupOutcome::NeedsAttention { error }),
        };
        let staging_root = roots.staging_root.join(&journal.version);
        let context = ReplayContext {
            install_root: roots.install_root,
            staging_root: &staging_root,
            journal_root,
            commit_id: &journal.commit_id,
        };
        match replay(&context, &journal) {
            Ok(JournalOutcome::Replayed { .. } | JournalOutcome::AlreadyCompleted) => {
                state::reset_attempts(journal_root)?;
                let _ = std::fs::remove_file(&rollback_path);
                return Ok(StartupOutcome::RolledBack {
                    to_version: journal.version,
                    reason: "resumed rollback".into(),
                });
            }
            Ok(JournalOutcome::ReplayFailed { op_index, reason }) => {
                return Ok(StartupOutcome::NeedsAttention {
                    error: EngineError::JournalReplayFailed(format!(
                        "rollback op {op_index}: {reason}"
                    )),
                });
            }
            Ok(_) => {}
            Err(error) => return Ok(StartupOutcome::NeedsAttention { error }),
        }
    }

    let commit_path = journal_root.join(COMMIT_JOURNAL);
    if commit_path.exists() {
        let journal = match read_journal(&commit_path) {
            Ok(journal) => journal,
            Err(error) => return Ok(StartupOutcome::NeedsAttention { error }),
        };
        let status = state::read_commit_status(journal_root)?;
        // A failed worker did not finish the journal. Retry forward first
        // (the cause may have been transient: a temporary lock or a full
        // disk); a persistent failure is rolled back below.
        let interrupted = matches!(status, Some(CommitStatus::Failed { .. }));
        match status {
            Some(CommitStatus::Completed { .. }) => {
                if attempts > u64::from(roots.crash_loop_threshold) {
                    let config = apply_config(roots, &journal);
                    if let Err(error) = rollback::rollback(&config, &journal) {
                        return Ok(StartupOutcome::NeedsAttention { error });
                    }
                    return Ok(StartupOutcome::RolledBack {
                        to_version: journal.from_version,
                        reason: format!("{attempts} unconfirmed starts"),
                    });
                }
                return Ok(StartupOutcome::Committed {
                    version: journal.version,
                });
            }
            // Interrupted and never-started commits share the same retry.
            Some(CommitStatus::Failed { .. }) | None => {}
        }

        let staging_root = roots.staging_root.join(&journal.version);
        let context = ReplayContext {
            install_root: roots.install_root,
            staging_root: &staging_root,
            journal_root,
            commit_id: &journal.commit_id,
        };
        match replay(&context, &journal) {
            Ok(JournalOutcome::Replayed { ops_completed }) => {
                state::write_commit_status(
                    journal_root,
                    &CommitStatus::Completed {
                        version: journal.version.clone(),
                        commit_id: journal.commit_id.clone(),
                    },
                )?;
                Ok(StartupOutcome::Recovered {
                    ops_completed,
                    version: journal.version,
                })
            }
            Ok(JournalOutcome::ReplayFailed { op_index, reason }) => {
                if interrupted {
                    let config = apply_config(roots, &journal);
                    if let Err(error) = rollback::rollback(&config, &journal) {
                        return Ok(StartupOutcome::NeedsAttention { error });
                    }
                    return Ok(StartupOutcome::RolledBack {
                        to_version: journal.from_version,
                        reason: format!("commit failed at op {op_index}"),
                    });
                }
                state::write_commit_status(
                    journal_root,
                    &CommitStatus::Failed {
                        op_index: op_index as u32,
                        message: reason.clone(),
                    },
                )?;
                Ok(StartupOutcome::NeedsAttention {
                    error: EngineError::JournalReplayFailed(format!("op {op_index}: {reason}")),
                })
            }
            Ok(_) => Ok(StartupOutcome::Clean),
            Err(error) => Ok(StartupOutcome::NeedsAttention { error }),
        }
    } else {
        state::reset_attempts(journal_root)?;
        // Confirmation removes the journal first. If cleanup was
        // interrupted after that unlink, the completed status is the only
        // remaining evidence that the files are already in place.
        if let Some(CommitStatus::Completed { version, .. }) =
            state::read_commit_status(journal_root)?
        {
            return Ok(StartupOutcome::Committed { version });
        }
        let prepared_path = journal_root.join(PREPARED_JSON);
        if prepared_path.exists() {
            let plan = PreparedPlan::load(&prepared_path)?;
            verify_plan(&plan)?;
            return Ok(StartupOutcome::ReusedPrepared {
                version: plan.version,
            });
        }
        Ok(StartupOutcome::Clean)
    }
}

/// Elevated worker entry: recover the journal described by `apply.json`.
///
/// The worker process is a startup attempt of its own, so it increments the
/// counter exactly once. The parent then inspects the result without counting
/// again.
pub(crate) fn recover_worker(
    config: &ApplyConfig,
    journal_root: &Path,
    crash_loop_threshold: u32,
) -> Result<(), EngineError> {
    let attempts = state::increment_attempts(journal_root)?;
    match recover(&RecoveryRoots {
        install_root: &config.install_root,
        staging_root: &config.staging_root,
        journal_root,
        crash_loop_threshold,
        attempts,
    })? {
        StartupOutcome::NeedsAttention { error } => Err(error),
        _ => Ok(()),
    }
}

fn apply_config(roots: &RecoveryRoots<'_>, journal: &Journal) -> ApplyConfig {
    ApplyConfig {
        v: 1,
        install_root: roots.install_root.to_path_buf(),
        staging_root: roots.staging_root.to_path_buf(),
        journal_root: roots.journal_root.to_path_buf(),
        version: journal.version.clone(),
        commit_id: journal.commit_id.clone(),
        parent_pid: std::process::id(),
    }
}

fn read_journal(path: &Path) -> Result<Journal, EngineError> {
    let bytes = std::fs::read(path)
        .map_err(|err| EngineError::JournalCorrupt(format!("read {}: {err}", path.display())))?;
    Journal::decode(&bytes)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::journal::state::COMMIT_STATUS;

    #[test]
    fn completed_update_only_needs_writes_past_the_threshold() {
        let dir = tempdir().unwrap();
        let journal = dir.path().join("journal");
        std::fs::create_dir_all(&journal).unwrap();

        assert!(!needs_install_write(&journal, 2).unwrap());

        std::fs::write(journal.join(COMMIT_JOURNAL), b"journal").unwrap();
        std::fs::write(
            journal.join(COMMIT_STATUS),
            br#"{"state":"completed","version":"2.0.0","commit_id":"id"}"#,
        )
        .unwrap();
        assert!(!needs_install_write(&journal, 2).unwrap());

        std::fs::write(journal.join(state::ATTEMPTS), "2").unwrap();
        assert!(needs_install_write(&journal, 2).unwrap());

        std::fs::write(journal.join(ROLLBACK_JOURNAL), b"journal").unwrap();
        std::fs::write(journal.join(state::ATTEMPTS), "0").unwrap();
        assert!(needs_install_write(&journal, 2).unwrap());
    }

    #[test]
    fn only_state_paths_are_touched_without_a_journal() {
        let dir = tempdir().unwrap();
        let roots = RecoveryRoots {
            install_root: dir.path(),
            staging_root: dir.path(),
            journal_root: dir.path(),
            crash_loop_threshold: 2,
            attempts: 1,
        };
        assert!(matches!(recover(&roots).unwrap(), StartupOutcome::Clean));
        assert_eq!(state::read_attempts(dir.path()), 0);
    }
}
