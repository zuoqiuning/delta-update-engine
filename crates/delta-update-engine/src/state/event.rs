use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::error::ErrorPayload;

use super::machine::EngineState;

pub const EVENT_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressPayload {
    pub total: u64,
    pub completed: u64,
    /// The file currently being downloaded/verified, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatePayload {
    pub v: u32,
    pub state: String,
    pub version: Option<String>,
    pub progress: Option<ProgressPayload>,
    pub error: Option<ErrorPayload>,
    pub timestamp: String,
}

impl StatePayload {
    /// Build the wire payload from an engine state snapshot.
    pub fn from_state(state: &EngineState) -> Self {
        let (version, progress, error) = match state {
            EngineState::Downloading { total, completed } => (
                None,
                Some(ProgressPayload {
                    total: *total,
                    completed: *completed,
                    file: None,
                }),
                None,
            ),
            EngineState::Preparing { verified, total } => (
                None,
                Some(ProgressPayload {
                    total: u64::from(*total),
                    completed: u64::from(*verified),
                    file: None,
                }),
                None,
            ),
            EngineState::Committing { processed, total } => (
                None,
                Some(ProgressPayload {
                    total: u64::from(*total),
                    completed: u64::from(*processed),
                    file: None,
                }),
                None,
            ),
            EngineState::Prepared { version }
            | EngineState::Cancelled { version }
            | EngineState::Committed { version } => (Some(version.clone()), None, None),
            EngineState::RollingBack { to_version, .. }
            | EngineState::RolledBack { to_version, .. } => (Some(to_version.clone()), None, None),
            EngineState::Failed { error, .. } => (None, None, Some(error.clone())),
            EngineState::Idle | EngineState::Checking => (None, None, None),
        };

        Self {
            v: EVENT_PROTOCOL_VERSION,
            state: state.name().to_string(),
            version,
            progress,
            error,
            timestamp: now_rfc3339(),
        }
    }
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}
