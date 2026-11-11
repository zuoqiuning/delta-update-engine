//! Prepare-side staging: materialize changed files and verify every target.

pub mod plan;
pub mod prepare;
pub mod verify;

pub use plan::{PreparedPlan, StagedFile};
pub(crate) use prepare::{prepare, PrepareInput};
pub use verify::verify_plan;
