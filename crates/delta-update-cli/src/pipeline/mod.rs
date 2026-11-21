//! Release pipeline. Stages are separate modules so each can be tested alone.
//!
//! Command entry points live in [`crate::commands`]; this module keeps the
//! reusable stages and re-exports those entry points so `pipeline::release`,
//! `pipeline::verify` and friends stay stable for callers and tests.

pub mod audit;
pub mod bundle;
pub mod keys;
pub mod manifest;
pub mod patches;
pub mod plan;
pub mod publish;
pub mod sbom;
pub mod scan;
pub mod self_test;
pub mod sign;

pub use crate::commands::doctor::{doctor, simulated_error};
pub use crate::commands::inspect::inspect_release;
pub use crate::commands::key_info::key_info;
pub use crate::commands::migrate::migrate;
pub use crate::commands::promote::promote;
pub use crate::commands::release::release;
pub use crate::commands::report::report_history;
pub use crate::commands::rollback::rollback;
pub use crate::commands::verify::verify;
pub use crate::commands::verify_key::verify_key;
