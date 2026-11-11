//! Patch container format and apply pipeline.

pub mod apply;
pub mod container;
pub mod provider;

pub use apply::apply_patch;
pub use container::{PatchFormat, PatchHeader, PATCH_MAGIC};
pub use provider::DiffProvider;
