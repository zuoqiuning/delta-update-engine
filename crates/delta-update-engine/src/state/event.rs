use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::error::ErrorPayload;

use super::machine::EngineState;

pub const EVENT_PROTOCOL_VERSION: u32 = 1;

/// Unit of `ProgressPayload::{completed, total}`. It depends on the state, so
/// the value is carried on the wire instead of being inferred by the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProgressUnit {
    Bytes,
    Files,
    Ops,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressPayload {
    pub total: u64,
    pub completed: u64,
    /// The file currently being downloaded/verified, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Meaning of `completed`/`total`; absent in older payloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<ProgressUnit>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatePayload {
    pub v: u32,
    pub state: String,
    pub version: Option<String>,
    pub progress: Option<ProgressPayload>,
    pub error: Option<ErrorPayload>,
    /// Set only in `UpdateRequired`: the oldest version the publisher supports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_supported_version: Option<String>,
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
                    unit: Some(ProgressUnit::Bytes),
                }),
                None,
            ),
            EngineState::Preparing { verified, total } => (
                None,
                Some(ProgressPayload {
                    total: u64::from(*total),
                    completed: u64::from(*verified),
                    file: None,
                    unit: Some(ProgressUnit::Files),
                }),
                None,
            ),
            EngineState::Committing { processed, total } => (
                None,
                Some(ProgressPayload {
                    total: u64::from(*total),
                    completed: u64::from(*processed),
                    file: None,
                    unit: Some(ProgressUnit::Ops),
                }),
                None,
            ),
            EngineState::Prepared { version }
            | EngineState::Cancelled { version }
            | EngineState::Committed { version }
            | EngineState::UpdateRequired { version, .. } => (Some(version.clone()), None, None),
            EngineState::RollingBack { to_version, .. }
            | EngineState::RolledBack { to_version, .. } => (Some(to_version.clone()), None, None),
            EngineState::Failed { error, .. } => (None, None, Some(error.clone())),
            EngineState::Idle | EngineState::Checking => (None, None, None),
        };

        let min_supported_version = match state {
            EngineState::UpdateRequired {
                min_supported_version,
                ..
            } => Some(min_supported_version.clone()),
            _ => None,
        };

        Self {
            v: EVENT_PROTOCOL_VERSION,
            state: state.name().to_string(),
            version,
            progress,
            error,
            min_supported_version,
            timestamp: now_rfc3339(),
        }
    }
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_payloads_carry_their_unit() {
        let downloading = StatePayload::from_state(&EngineState::Downloading {
            total: 10,
            completed: 4,
        });
        let progress = downloading.progress.as_ref().unwrap();
        assert_eq!(progress.unit, Some(ProgressUnit::Bytes));

        let preparing = StatePayload::from_state(&EngineState::Preparing {
            verified: 2,
            total: 3,
        });
        assert_eq!(preparing.progress.unwrap().unit, Some(ProgressUnit::Files));

        let committing = StatePayload::from_state(&EngineState::Committing {
            processed: 1,
            total: 8,
        });
        assert_eq!(committing.progress.unwrap().unit, Some(ProgressUnit::Ops));

        let json = serde_json::to_value(&downloading).unwrap();
        assert_eq!(json["progress"]["unit"], "bytes");
    }

    #[test]
    fn update_required_carries_the_minimum_version() {
        let payload = StatePayload::from_state(&EngineState::UpdateRequired {
            version: "2.0.0".into(),
            min_supported_version: "1.5.0".into(),
        });
        assert_eq!(payload.state, "UpdateRequired");
        assert_eq!(payload.version.as_deref(), Some("2.0.0"));
        assert_eq!(payload.min_supported_version.as_deref(), Some("1.5.0"));

        let idle = StatePayload::from_state(&EngineState::Idle);
        assert_eq!(idle.min_supported_version, None);
    }
}
