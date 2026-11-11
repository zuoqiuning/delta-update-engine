use std::path::Path;

use crate::error::EngineError;
use crate::manifest::model::Strategy;

/// Build-side diff provider. Implementations live behind features so the
/// runtime dependency surface stays small.
pub trait DiffProvider: Send + Sync {
    fn strategy(&self) -> Strategy;
    fn diff(&self, old: &Path, new: &Path, output: &Path) -> Result<u64, EngineError>;
}
