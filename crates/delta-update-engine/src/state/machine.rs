use crate::error::{EngineError, ErrorPayload};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateEvent {
    pub state: EngineState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EngineState {
    Idle,
    /// The running version is below the publisher's `min_supported_version`.
    /// The engine never blocks startup; the application decides how strict to
    /// be. `version` is the release the manifest offers, if any.
    UpdateRequired {
        version: String,
        min_supported_version: String,
    },
    Checking,
    Downloading {
        total: u64,
        completed: u64,
    },
    Preparing {
        verified: u32,
        total: u32,
    },
    Prepared {
        version: String,
    },
    Cancelled {
        version: String,
    },
    Committing {
        processed: u32,
        total: u32,
    },
    Committed {
        version: String,
    },
    RollingBack {
        to_version: String,
        reason: String,
    },
    RolledBack {
        to_version: String,
        reason: String,
    },
    Failed {
        category: String,
        error: ErrorPayload,
    },
}

impl EngineState {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::UpdateRequired { .. } => "UpdateRequired",
            Self::Checking => "Checking",
            Self::Downloading { .. } => "Downloading",
            Self::Preparing { .. } => "Preparing",
            Self::Prepared { .. } => "Prepared",
            Self::Cancelled { .. } => "Cancelled",
            Self::Committing { .. } => "Committing",
            Self::Committed { .. } => "Committed",
            Self::RollingBack { .. } => "RollingBack",
            Self::RolledBack { .. } => "RolledBack",
            Self::Failed { .. } => "Failed",
        }
    }

    pub fn is_busy(&self) -> bool {
        matches!(
            self,
            Self::Checking
                | Self::Downloading { .. }
                | Self::Preparing { .. }
                | Self::Committing { .. }
                | Self::RollingBack { .. }
        )
    }

    /// The transition table from the protocol document. Recovery paths such as
    /// `Idle -> Committed` and `Idle -> RollingBack` are explicitly legal.
    pub fn can_transition_to(&self, next: &Self) -> bool {
        use EngineState::*;
        matches!(
            (self, next),
            (Idle, Checking)
                | (Idle, UpdateRequired { .. })
                | (Idle, Committed { .. })
                | (Idle, RollingBack { .. })
                | (UpdateRequired { .. }, Checking)
                | (UpdateRequired { .. }, Prepared { .. })
                | (UpdateRequired { .. }, Cancelled { .. })
                | (UpdateRequired { .. }, Failed { .. })
                | (UpdateRequired { .. }, Idle)
                | (Checking, UpdateRequired { .. })
                | (Checking, Downloading { .. })
                | (Checking, Failed { .. })
                | (Downloading { .. }, Preparing { .. })
                | (Downloading { .. }, Failed { .. })
                | (Preparing { .. }, Prepared { .. })
                | (Preparing { .. }, Cancelled { .. })
                | (Preparing { .. }, Failed { .. })
                | (Cancelled { .. }, Checking)
                | (Prepared { .. }, Committing { .. })
                | (Committing { .. }, Committed { .. })
                | (Committing { .. }, Failed { .. })
                | (RollingBack { .. }, RolledBack { .. })
                | (RollingBack { .. }, Failed { .. })
                | (Idle, RolledBack { .. })
                | (RolledBack { .. }, Checking)
                | (RolledBack { .. }, Idle)
                | (Committed { .. }, Idle)
                | (Failed { .. }, Checking)
                | (Failed { .. }, Idle)
        )
    }

    pub fn transition_to(self, next: Self) -> Result<Self, EngineError> {
        if self.can_transition_to(&next) {
            Ok(next)
        } else {
            Err(EngineError::InvalidTransition {
                from: self.name().to_string(),
                to: next.name().to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed() -> EngineState {
        EngineState::Failed {
            category: "network".into(),
            error: EngineError::Cancelled.to_payload(),
        }
    }

    #[test]
    fn accepts_documented_chain() {
        let states = [
            EngineState::Idle,
            EngineState::Checking,
            EngineState::Downloading {
                total: 10,
                completed: 0,
            },
            EngineState::Preparing {
                verified: 0,
                total: 10,
            },
            EngineState::Prepared {
                version: "2.1.0".into(),
            },
            EngineState::Committing {
                processed: 0,
                total: 10,
            },
            EngineState::Committed {
                version: "2.1.0".into(),
            },
            EngineState::Idle,
        ];
        for pair in states.windows(2) {
            assert!(
                pair[0].can_transition_to(&pair[1]),
                "{} -> {}",
                pair[0].name(),
                pair[1].name()
            );
        }
    }

    #[test]
    fn accepts_recovery_paths_and_cancel() {
        assert!(
            EngineState::Idle.can_transition_to(&EngineState::Committed {
                version: "2.1.0".into()
            })
        );
        assert!(
            EngineState::Idle.can_transition_to(&EngineState::RollingBack {
                to_version: "2.0.0".into(),
                reason: "crash loop".into(),
            })
        );
        assert!(EngineState::Preparing {
            verified: 1,
            total: 10
        }
        .can_transition_to(&EngineState::Cancelled {
            version: "2.1.0".into()
        }));
        assert!(failed().can_transition_to(&EngineState::Checking));
    }

    #[test]
    fn rejects_skipped_states() {
        let error = EngineState::Idle
            .transition_to(EngineState::Prepared {
                version: "2.1.0".into(),
            })
            .unwrap_err();
        assert_eq!(error.code(), "E_INVALID_TRANSITION");
    }

    #[test]
    fn update_required_is_a_resting_state() {
        let required = EngineState::UpdateRequired {
            version: "2.0.0".into(),
            min_supported_version: "1.5.0".into(),
        };
        assert_eq!(required.name(), "UpdateRequired");
        assert!(!required.is_busy());
        assert!(EngineState::Idle.can_transition_to(&required));
        assert!(required.can_transition_to(&EngineState::Checking));
        assert!(required.can_transition_to(&EngineState::Idle));
    }
}
