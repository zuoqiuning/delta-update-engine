//! Write-ahead journal for multi-file replacement.

pub mod format;
pub mod lock;
pub mod plan;
pub mod replay;
pub mod state;

pub use format::{FileOp, Journal, JOURNAL_MAX_OPS};
pub use lock::UpdateLock;
pub(crate) use replay::replay_with_fs;
pub use replay::{replay, JournalOutcome, ReplayContext};
pub use state::{
    ApplyConfig, CommitStatus, APPLY_JSON, ATTEMPTS, COMMIT_JOURNAL, COMMIT_STATUS, PREPARED_JSON,
    ROLLBACK_JOURNAL, SKIPPED_VERSION, UPDATE_LOCK,
};
