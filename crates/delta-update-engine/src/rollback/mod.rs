//! Rollback executes the inverse of a commit journal.

use std::path::Path;

use crate::error::EngineError;
use crate::journal::state::{
    self, ApplyConfig, COMMIT_JOURNAL, COMMIT_STATUS, PREPARED_JSON, ROLLBACK_JOURNAL,
};
use crate::journal::{replay, Journal, JournalOutcome, ReplayContext};

pub mod inverse;

/// Roll back a completed commit by generating and replaying an inverse journal.
pub fn rollback(config: &ApplyConfig, commit: &Journal) -> Result<(), EngineError> {
    let ops = inverse::inverse(&config.install_root, commit)?;
    let journal = Journal::new(
        &commit.commit_id,
        &commit.from_version,
        &commit.version,
        ops,
    );
    let rollback_path = config.journal_root.join(ROLLBACK_JOURNAL);
    journal.write_atomic(&rollback_path)?;

    let staging_root = config.staging_root.join(&commit.version);
    let context = ReplayContext {
        install_root: &config.install_root,
        staging_root: &staging_root,
        journal_root: &config.journal_root,
        commit_id: &commit.commit_id,
    };
    let outcome = replay(&context, &journal)?;
    if let JournalOutcome::ReplayFailed { op_index, reason } = outcome {
        return Err(EngineError::JournalReplayFailed(format!(
            "rollback op {op_index}: {reason}"
        )));
    }

    state::write_skipped_version(&config.journal_root, &commit.version)?;
    state::reset_attempts(&config.journal_root)?;
    let _ = std::fs::remove_file(&rollback_path);
    let _ = std::fs::remove_file(config.journal_root.join(COMMIT_JOURNAL));
    let _ = std::fs::remove_file(config.journal_root.join(COMMIT_STATUS));
    let _ = std::fs::remove_file(config.journal_root.join(PREPARED_JSON));
    crate::fs::remove_dir_all_best_effort(&config.staging_root.join(&commit.version));
    crate::fs::remove_dir_all_best_effort(&backup_dir(&config.install_root, &commit.commit_id));
    crate::fs::remove_dir_all_best_effort(&trash_dir(&config.install_root, &commit.commit_id));
    Ok(())
}

pub fn backup_dir(install_root: &Path, commit_id: &str) -> std::path::PathBuf {
    install_root.join(".old").join(commit_id)
}

pub fn trash_dir(install_root: &Path, commit_id: &str) -> std::path::PathBuf {
    install_root.join(".trash").join(commit_id)
}
