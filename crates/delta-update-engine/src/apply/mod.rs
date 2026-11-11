//! Commit execution. Called by the replacement process, never by the UI.

pub mod applier;
pub mod writer;

pub use applier::{run_applier, run_applier_from_args, run_recovery};
pub use writer::{apply_commit, DirectWriter, PrivilegedWriter};
