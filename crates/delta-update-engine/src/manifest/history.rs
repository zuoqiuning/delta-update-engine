use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::EngineError;

/// The set of files that were official in any previously accepted manifest.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct KnownFiles {
    pub files: BTreeSet<String>,
}

impl KnownFiles {
    pub fn load(path: &Path) -> Result<Self, EngineError> {
        match std::fs::read(path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(known) => Ok(known),
                Err(err) => {
                    // Corrupt history must not block updates; it only means
                    // removed files are treated as untracked (never deleted).
                    tracing::warn!(%err, "known_files.json is corrupt; using empty history");
                    Ok(Self::default())
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(EngineError::ManifestInvalid(format!("known_files: {err}"))),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), EngineError> {
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|err| EngineError::Internal(format!("known_files: {err}")))?;
        crate::fs::atomic::write_atomic(path, &bytes)
            .map_err(|err| EngineError::Internal(format!("known_files: {err}")))
    }

    pub fn contains(&self, path: &str) -> bool {
        self.files.contains(path)
    }

    pub fn extend(&mut self, manifest: &super::model::Manifest) {
        self.files.extend(manifest.files.keys().cloned());
    }
}
