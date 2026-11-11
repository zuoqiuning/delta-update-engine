use std::path::Path;

use crate::error::EngineError;
use crate::fs::{FileSystem, RealFs};

use crate::journal::state::{self, ApplyConfig, CommitStatus};
use crate::journal::{
    replay_with_fs, Journal, JournalOutcome, ReplayContext, UpdateLock, COMMIT_JOURNAL,
};

/// Executes a journal from disk. The worker never receives an in-memory op
/// list, so a compromised supervisor cannot change the intent after the
/// commit point.
pub trait PrivilegedWriter: Send + Sync {
    fn apply_journal(
        &self,
        config: &ApplyConfig,
        journal: &Path,
    ) -> Result<CommitStatus, EngineError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct DirectWriter;

impl DirectWriter {
    /// Execute the journal against an injected filesystem.
    pub fn apply_journal_with_fs(
        &self,
        config: &ApplyConfig,
        journal_path: &Path,
        fs: &dyn FileSystem,
    ) -> Result<CommitStatus, EngineError> {
        let _lock = UpdateLock::acquire(&config.journal_root.join(state::UPDATE_LOCK))?;
        let bytes = std::fs::read(journal_path)
            .map_err(|err| EngineError::JournalCorrupt(format!("read journal: {err}")))?;
        let journal = Journal::decode(&bytes)?;
        if journal.commit_id != config.commit_id {
            return Err(EngineError::JournalCorrupt(
                "commit_id does not match apply.json".into(),
            ));
        }

        let staging_root = config.staging_root.join(&journal.version);
        let context = ReplayContext {
            install_root: &config.install_root,
            staging_root: &staging_root,
            journal_root: &config.journal_root,
            commit_id: &journal.commit_id,
        };

        let status = match replay_with_fs(&context, &journal, fs)? {
            JournalOutcome::Replayed { .. } | JournalOutcome::AlreadyCompleted => {
                CommitStatus::Completed {
                    version: journal.version.clone(),
                    commit_id: journal.commit_id.clone(),
                }
            }
            JournalOutcome::ReplayFailed { op_index, reason } => {
                let status = CommitStatus::Failed {
                    op_index: op_index as u32,
                    message: reason.clone(),
                };
                state::write_commit_status(&config.journal_root, &status)?;
                return Err(EngineError::JournalReplayFailed(format!(
                    "op {op_index}: {reason}"
                )));
            }
            JournalOutcome::NoJournal => {
                return Err(EngineError::JournalCorrupt("journal is empty".into()))
            }
            JournalOutcome::Corrupt { reason } => return Err(EngineError::JournalCorrupt(reason)),
        };

        state::write_commit_status(&config.journal_root, &status)?;
        Ok(status)
    }
}

impl PrivilegedWriter for DirectWriter {
    fn apply_journal(
        &self,
        config: &ApplyConfig,
        journal_path: &Path,
    ) -> Result<CommitStatus, EngineError> {
        self.apply_journal_with_fs(config, journal_path, &RealFs)
    }
}

/// Execute the journal that `begin_commit` persisted.
pub fn apply_commit(journal_root: &Path, commit_id: &str) -> Result<CommitStatus, EngineError> {
    let config = state::read_apply_config(journal_root)?.ok_or(EngineError::NotPrepared)?;
    if config.commit_id != commit_id {
        return Err(EngineError::JournalCorrupt(
            "commit_id does not match apply.json".into(),
        ));
    }
    DirectWriter.apply_journal(&config, &journal_root.join(COMMIT_JOURNAL))
}
