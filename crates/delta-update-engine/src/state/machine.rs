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
    ///
    /// `Checking` is transient: after a check the plugin returns to the stable
    /// state it came from, so every `X -> Checking -> X` pair below is legal.
    /// `prepare` is also allowed to start from any settled state and publishes
    /// `Downloading`/`Prepared` directly, without passing through `Checking`.
    /// Keep this list in sync with `docs/design/protocol.md` §6.2 and the
    /// `command_paths_are_documented` test in the plugin.
    pub fn can_transition_to(&self, next: &Self) -> bool {
        use EngineState::*;
        matches!(
            (self, next),
            (Idle, Checking)
                | (Idle, UpdateRequired { .. })
                | (Idle, Prepared { .. })
                | (Idle, Downloading { .. })
                | (Idle, Committed { .. })
                | (Idle, RollingBack { .. })
                | (Idle, RolledBack { .. })
                | (Idle, Failed { .. })
                | (UpdateRequired { .. }, Checking)
                | (UpdateRequired { .. }, Prepared { .. })
                | (UpdateRequired { .. }, Downloading { .. })
                | (UpdateRequired { .. }, Cancelled { .. })
                | (UpdateRequired { .. }, Failed { .. })
                | (UpdateRequired { .. }, Idle)
                | (Checking, UpdateRequired { .. })
                | (Checking, Downloading { .. })
                | (Checking, Idle)
                | (Checking, Prepared { .. })
                | (Checking, Committed { .. })
                | (Checking, RolledBack { .. })
                | (Checking, Failed { .. })
                | (Downloading { .. }, Preparing { .. })
                | (Downloading { .. }, Cancelled { .. })
                | (Downloading { .. }, Failed { .. })
                | (Preparing { .. }, Prepared { .. })
                | (Preparing { .. }, Cancelled { .. })
                | (Preparing { .. }, Failed { .. })
                | (Cancelled { .. }, Checking)
                | (Cancelled { .. }, Prepared { .. })
                | (Cancelled { .. }, Downloading { .. })
                | (Prepared { .. }, Checking)
                | (Prepared { .. }, Committing { .. })
                | (Prepared { .. }, Failed { .. })
                | (Committed { .. }, Checking)
                | (Committing { .. }, Committed { .. })
                | (Committing { .. }, Failed { .. })
                | (RollingBack { .. }, RolledBack { .. })
                | (RollingBack { .. }, Failed { .. })
                | (RolledBack { .. }, Checking)
                | (RolledBack { .. }, Prepared { .. })
                | (RolledBack { .. }, Downloading { .. })
                | (RolledBack { .. }, Idle)
                | (Committed { .. }, Idle)
                | (Failed { .. }, Checking)
                | (Failed { .. }, Prepared { .. })
                | (Failed { .. }, Downloading { .. })
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
        // `Idle -> Prepared` is legal (a restarted app reuses PREPARED), so use
        // an edge that skips the commit step entirely.
        let error = EngineState::Idle
            .transition_to(EngineState::Committing {
                processed: 0,
                total: 0,
            })
            .unwrap_err();
        assert_eq!(error.code(), "E_INVALID_TRANSITION");
    }

    /// The command layer may enter `Downloading`/`Prepared` from any settled
    /// state (`prepare` does not require a preceding `check`). This is the
    /// contract the plugin's `set_state` validates against.
    #[test]
    fn accepts_command_entry_transitions() {
        let downloading = || EngineState::Downloading {
            total: 10,
            completed: 0,
        };
        let prepared = || EngineState::Prepared {
            version: "2.0.0".into(),
        };
        let settled = [
            EngineState::Idle,
            EngineState::UpdateRequired {
                version: "2.0.0".into(),
                min_supported_version: "1.5.0".into(),
            },
            EngineState::Cancelled {
                version: "2.0.0".into(),
            },
            EngineState::RolledBack {
                to_version: "1.0.0".into(),
                reason: "crash loop".into(),
            },
            EngineState::Failed {
                category: "internal".into(),
                error: EngineError::Cancelled.to_payload(),
            },
        ];
        for from in &settled {
            assert!(
                from.can_transition_to(&downloading()),
                "{} -> Downloading",
                from.name()
            );
            assert!(
                from.can_transition_to(&prepared()),
                "{} -> Prepared",
                from.name()
            );
        }
        // A prepared update that cannot spawn the worker reports `Failed`
        // without leaving `Prepared` first.
        assert!(prepared().can_transition_to(&failed()));
        // A startup that needs attention or a disabled plugin reports `Failed`.
        assert!(EngineState::Idle.can_transition_to(&failed()));
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

    /// Every edge in `docs/design/protocol.md` §6.2, including the prose rule
    /// that a check returns to the stable state it started from.
    #[test]
    fn accepts_every_documented_transition() {
        let checking = || EngineState::Checking;
        let downloading = || EngineState::Downloading {
            total: 10,
            completed: 0,
        };
        let preparing = || EngineState::Preparing {
            verified: 0,
            total: 3,
        };
        let prepared = || EngineState::Prepared {
            version: "2.0.0".into(),
        };
        let cancelled = || EngineState::Cancelled {
            version: "2.0.0".into(),
        };
        let committing = || EngineState::Committing {
            processed: 0,
            total: 0,
        };
        let committed = || EngineState::Committed {
            version: "2.0.0".into(),
        };
        let rolling_back = || EngineState::RollingBack {
            to_version: "1.0.0".into(),
            reason: "crash loop".into(),
        };
        let rolled_back = || EngineState::RolledBack {
            to_version: "1.0.0".into(),
            reason: "crash loop".into(),
        };
        let required = || EngineState::UpdateRequired {
            version: "2.0.0".into(),
            min_supported_version: "1.5.0".into(),
        };

        let cases = [
            (EngineState::Idle, checking()),
            (EngineState::Idle, required()),
            (EngineState::Idle, committed()),
            (EngineState::Idle, rolling_back()),
            (EngineState::Idle, rolled_back()),
            (required(), checking()),
            (required(), EngineState::Idle),
            (checking(), downloading()),
            (checking(), EngineState::Idle),
            (checking(), prepared()),
            (checking(), committed()),
            (checking(), rolled_back()),
            (checking(), failed()),
            (downloading(), preparing()),
            (downloading(), cancelled()),
            (downloading(), failed()),
            (preparing(), prepared()),
            (preparing(), cancelled()),
            (preparing(), failed()),
            (cancelled(), checking()),
            (prepared(), checking()),
            (prepared(), committing()),
            (committed(), checking()),
            (committed(), EngineState::Idle),
            (committing(), committed()),
            (committing(), failed()),
            (rolling_back(), rolled_back()),
            (rolling_back(), failed()),
            (rolled_back(), checking()),
            (rolled_back(), EngineState::Idle),
            (failed(), checking()),
            (failed(), EngineState::Idle),
        ];
        for (from, to) in cases {
            assert!(
                from.can_transition_to(&to),
                "{} -> {} must be legal",
                from.name(),
                to.name()
            );
        }
    }
}
