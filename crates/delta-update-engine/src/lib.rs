//! File-level delta update engine.
//!
//! The engine is intentionally free of Tauri types. It can be used from a
//! plugin, a CLI, or an integration test with two directory trees.

#![forbid(unsafe_code)]

pub mod apply;
pub mod config;
pub mod crypto;
pub mod engine;
pub mod error;
pub mod fetch;
pub mod fs;
pub mod journal;
pub mod limits;
pub mod manifest;
pub mod patch;
pub mod path;
mod recovery;
pub mod rollback;
pub mod staging;
pub mod state;
pub mod version;

pub use apply::{run_applier, run_applier_from_args, run_recovery, DirectWriter, PrivilegedWriter};
pub use config::EngineConfig;
pub use engine::{CheckOutcome, Engine, PendingCommit, PreparedUpdate, Progress, StartupOutcome};
pub use error::{
    EngineError, ErrorAction, ErrorCategory, ErrorPayload, NetworkError, PatchError, SignatureError,
};
pub use journal::{ApplyConfig, CommitStatus, FileOp, Journal, JournalOutcome};
pub use limits::Limits;
pub use manifest::model::{FileEntry, Manifest, Strategy};
pub use state::{EngineState, ProgressPayload, StateEvent, StatePayload};
