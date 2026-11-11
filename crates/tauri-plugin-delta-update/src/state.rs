use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use delta_update_engine::{
    CheckOutcome, Engine, EngineError, EngineState, PreparedUpdate, StatePayload,
};
use tokio::sync::Mutex;

use crate::config::Config;

/// Map an engine failure to the observable state. The category string matches
/// the serde representation used on the wire.
pub fn failed_state(error: &EngineError) -> EngineState {
    let category = serde_json::to_value(error.category())
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| error.category().to_string());
    EngineState::Failed {
        category,
        error: error.to_payload(),
    }
}

/// Process-wide plugin state: engine handle, UI snapshot, PREPARED update and
/// the async command gate.
pub struct PluginState {
    engine: Arc<Engine>,
    snapshot: RwLock<EngineState>,
    target_version: RwLock<Option<String>>,
    last_check: RwLock<Option<(Instant, CheckOutcome)>>,
    prepared: RwLock<Option<PreparedUpdate>>,
    journal_root: PathBuf,
    auto_apply: bool,
    auto_confirm: bool,
    confirm_delay_ms: u64,
    auto_confirm_started: AtomicBool,
    awaiting_confirm: AtomicBool,
    gate: Mutex<()>,
}

impl PluginState {
    pub fn new(engine: Engine, config: &Config, journal_root: PathBuf) -> Self {
        Self {
            engine: Arc::new(engine),
            snapshot: RwLock::new(EngineState::Idle),
            target_version: RwLock::new(None),
            last_check: RwLock::new(None),
            prepared: RwLock::new(None),
            journal_root,
            auto_apply: config.auto_apply,
            auto_confirm: config.auto_confirm,
            confirm_delay_ms: config.confirm_delay_ms,
            auto_confirm_started: AtomicBool::new(false),
            awaiting_confirm: AtomicBool::new(false),
            gate: Mutex::new(()),
        }
    }

    pub fn engine(&self) -> &Arc<Engine> {
        &self.engine
    }

    pub fn snapshot(&self) -> EngineState {
        self.snapshot
            .read()
            .map(|state| state.clone())
            .unwrap_or(EngineState::Idle)
    }

    pub fn set_state(&self, state: EngineState) {
        if let Ok(mut current) = self.snapshot.write() {
            *current = state;
        }
    }

    /// Wire payload for a snapshot. Download/prepare/commit states carry the
    /// target version so the UI can label progress with "v2.1.0" instead of
    /// only a percentage; the engine states themselves stay version-agnostic.
    pub fn payload(&self, snapshot: &EngineState) -> StatePayload {
        let mut payload = StatePayload::from_state(snapshot);
        if payload.version.is_none()
            && matches!(
                snapshot,
                EngineState::Downloading { .. }
                    | EngineState::Preparing { .. }
                    | EngineState::Committing { .. }
                    | EngineState::Failed { .. }
            )
        {
            payload.version = self.target_version();
        }
        payload
    }

    pub fn target_version(&self) -> Option<String> {
        self.target_version
            .read()
            .ok()
            .and_then(|value| value.clone())
    }

    pub fn set_target_version(&self, version: Option<String>) {
        if let Ok(mut current) = self.target_version.write() {
            *current = version;
        }
    }

    pub fn record_check(&self, outcome: &CheckOutcome) {
        if let Ok(mut current) = self.last_check.write() {
            *current = Some((Instant::now(), outcome.clone()));
        }
    }

    /// The most recent successful check, if it is younger than `max_age`.
    /// `prepare` reuses it so a manual `check()` followed by `prepare()` does
    /// not fetch the manifest and signature twice.
    pub fn recent_check(&self, max_age: Duration) -> Option<CheckOutcome> {
        self.last_check
            .read()
            .ok()
            .and_then(|current| current.clone())
            .filter(|(at, _)| at.elapsed() <= max_age)
            .map(|(_, outcome)| outcome)
    }

    pub fn prepared(&self) -> Option<PreparedUpdate> {
        self.prepared.read().ok().and_then(|value| value.clone())
    }

    pub fn set_prepared(&self, prepared: Option<PreparedUpdate>) {
        if let Ok(mut current) = self.prepared.write() {
            *current = prepared;
        }
    }

    pub fn journal_root(&self) -> &PathBuf {
        &self.journal_root
    }

    pub fn auto_apply(&self) -> bool {
        self.auto_apply
    }

    pub fn auto_confirm(&self) -> bool {
        self.auto_confirm
    }

    pub fn confirm_delay_ms(&self) -> u64 {
        self.confirm_delay_ms
    }

    pub fn mark_auto_confirm_started(&self) -> bool {
        !self.auto_confirm_started.swap(true, Ordering::SeqCst)
    }

    /// A committed update is waiting for a health acknowledgement.
    pub fn mark_awaiting_confirm(&self, value: bool) {
        self.awaiting_confirm.store(value, Ordering::SeqCst);
    }

    pub fn awaiting_confirm(&self) -> bool {
        self.awaiting_confirm.load(Ordering::SeqCst)
    }

    pub fn gate(&self) -> &Mutex<()> {
        &self.gate
    }
}

#[cfg(test)]
mod tests {
    use delta_update_engine::{ErrorCategory, NetworkError};

    use super::*;

    #[test]
    fn failed_state_uses_the_wire_category() {
        let error = EngineError::Network(NetworkError::Timeout("slow".into()));
        match failed_state(&error) {
            EngineState::Failed { category, error } => {
                assert_eq!(category, "network");
                assert_eq!(error.code, "E_NETWORK_TIMEOUT");
                assert_eq!(error.category, ErrorCategory::Network);
                assert!(error.retryable);
            }
            other => panic!("unexpected state: {other:?}"),
        }
    }
}
