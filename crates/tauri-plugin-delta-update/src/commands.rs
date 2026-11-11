use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use delta_update_engine::{
    CheckOutcome, EngineError, EngineState, ErrorPayload, PreparedUpdate, StatePayload,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Runtime, State};

use crate::events::emit_state;
use crate::lifecycle::progress_state;
use crate::state::{failed_state, PluginState};

/// Coalesce high-frequency per-file progress into at most 10 events per second.
const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(100);
/// Reuse a check this recent when the user calls `prepare` right after
/// `check`, so the manifest and signature are not fetched twice.
const CHECK_REUSE_WINDOW: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub available: bool,
    pub version: Option<String>,
    pub current_version: String,
    pub files_changed: u32,
    pub files_staged: u32,
    pub download_bytes: u64,
    pub total_bytes: u64,
    pub notes: Option<String>,
    pub mandatory: bool,
}

impl From<CheckOutcome> for CheckResult {
    fn from(outcome: CheckOutcome) -> Self {
        Self {
            available: outcome.available,
            version: outcome.version,
            current_version: outcome.current_version,
            files_changed: outcome.files_changed,
            files_staged: outcome.files_staged,
            download_bytes: outcome.download_bytes,
            total_bytes: outcome.total_bytes,
            notes: outcome.notes,
            mandatory: outcome.mandatory,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrepareResult {
    pub state: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelResult {
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfirmResult {
    pub acknowledged: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestartResult {
    pub state: String,
    pub version: String,
}

fn publish<R: Runtime>(app: &AppHandle<R>, state: &PluginState, next: EngineState) {
    state.set_state(next.clone());
    emit_state(app, state.payload(&next));
}

/// The state to show once a check has settled. A committed update waiting for
/// health confirmation must survive a check, otherwise the crash-loop guard
/// loses its anchor.
fn resting(before: &EngineState, state: &PluginState) -> EngineState {
    if let EngineState::Committed { version } = before {
        return EngineState::Committed {
            version: version.clone(),
        };
    }
    state
        .prepared()
        .map(|prepared| EngineState::Prepared {
            version: prepared.version,
        })
        .unwrap_or(EngineState::Idle)
}

/// Keep a committed, prepared or rolled-back state when a background refresh
/// fails; only a settled idle app shows the failure.
fn after_error(before: &EngineState, state: &PluginState, error: &EngineError) -> EngineState {
    if matches!(before, EngineState::RolledBack { .. }) {
        return before.clone();
    }
    if matches!(before, EngineState::Committed { .. }) || state.prepared().is_some() {
        resting(before, state)
    } else {
        failed_state(error)
    }
}

fn busy(state: &PluginState, to: &str) -> ErrorPayload {
    EngineError::InvalidTransition {
        from: state.snapshot().name().to_string(),
        to: to.to_string(),
    }
    .to_payload()
}

fn current_version(state: &PluginState) -> String {
    state.engine().config().current_version.clone()
}

async fn run_check<R: Runtime>(
    app: &AppHandle<R>,
    state: &PluginState,
) -> Result<CheckOutcome, EngineError> {
    publish(app, state, EngineState::Checking);
    let outcome = state.engine().check().await?;
    if outcome.available {
        state.set_target_version(outcome.version.clone());
    } else {
        state.set_target_version(None);
    }
    state.record_check(&outcome);
    Ok(outcome)
}

fn report_check_error<R: Runtime>(
    app: &AppHandle<R>,
    state: &PluginState,
    before: &EngineState,
    error: &EngineError,
) {
    publish(app, state, after_error(before, state, error));
}

/// Download and stage the version that `check` verified.
async fn prepare_now<R: Runtime>(
    app: &AppHandle<R>,
    state: &Arc<PluginState>,
    download_bytes: u64,
    total_files: u32,
) -> Result<PreparedUpdate, EngineError> {
    publish(
        app,
        state,
        EngineState::Downloading {
            total: download_bytes,
            completed: 0,
        },
    );

    let verified = Arc::new(AtomicU32::new(0));
    let progress_gate = Arc::new(Mutex::new(()));
    let last_emit = Arc::new(Mutex::new(Instant::now() - PROGRESS_EMIT_INTERVAL));
    let task_app = AppHandle::clone(app);
    let task_state = Arc::clone(state);
    let result = state
        .engine()
        .prepare(Box::new(move |progress| {
            let Ok(_guard) = progress_gate.lock() else {
                return;
            };
            if let Some((next, file)) = progress_state(progress, &verified, total_files) {
                task_state.set_state(next.clone());
                let emit = last_emit
                    .lock()
                    .map(|mut last| {
                        if last.elapsed() >= PROGRESS_EMIT_INTERVAL {
                            *last = Instant::now();
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(true);
                if emit {
                    let mut payload = task_state.payload(&next);
                    if let Some(progress) = payload.progress.as_mut() {
                        progress.file = file;
                    }
                    emit_state(&task_app, payload);
                }
            }
        }))
        .await;

    match result {
        Ok(prepared) => {
            state.set_prepared(Some(prepared.clone()));
            state.set_target_version(Some(prepared.version.clone()));
            publish(
                app,
                state,
                EngineState::Prepared {
                    version: prepared.version.clone(),
                },
            );
            Ok(prepared)
        }
        Err(error) => {
            let next = if matches!(&error, EngineError::Cancelled) {
                EngineState::Cancelled {
                    version: state
                        .target_version()
                        .unwrap_or_else(|| current_version(state)),
                }
            } else {
                failed_state(&error)
            };
            publish(app, state, next);
            Err(error)
        }
    }
}

/// Background path used by `auto_check`. Never surfaces an error to the UI
/// beyond the state event.
pub(crate) async fn auto_check_prepare<R: Runtime>(app: &AppHandle<R>, state: Arc<PluginState>) {
    check_and_prepare(app, state, false).await;
}

/// Periodic path. It retries previously failed or rolled-back checks, but never
/// interrupts a prepared update or an in-flight operation.
pub(crate) async fn periodic_check_prepare<R: Runtime>(
    app: &AppHandle<R>,
    state: Arc<PluginState>,
) {
    check_and_prepare(app, state, true).await;
}

async fn check_and_prepare<R: Runtime>(
    app: &AppHandle<R>,
    state: Arc<PluginState>,
    retry_settled: bool,
) {
    let Ok(_gate) = state.gate().try_lock() else {
        return;
    };
    let before = state.snapshot();
    let allowed = matches!(before, EngineState::Idle)
        || (retry_settled
            && matches!(
                before,
                EngineState::Failed { .. } | EngineState::RolledBack { .. }
            ));
    if !allowed {
        return;
    }
    let outcome = match run_check(app, &state).await {
        Ok(outcome) => outcome,
        Err(error) => {
            report_check_error(app, &state, &before, &error);
            return;
        }
    };
    if !outcome.available {
        publish(app, &state, resting(&before, &state));
        return;
    }
    if let Some(prepared) = state.prepared() {
        publish(
            app,
            &state,
            EngineState::Prepared {
                version: prepared.version,
            },
        );
        return;
    }
    let _ = prepare_now(app, &state, outcome.download_bytes, outcome.files_staged).await;
}

#[tauri::command]
pub async fn check<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, Arc<PluginState>>,
) -> Result<CheckResult, ErrorPayload> {
    let state = Arc::clone(state.inner());
    let Ok(_gate) = state.gate().try_lock() else {
        return Err(busy(&state, "Checking"));
    };
    let before = state.snapshot();
    if before.is_busy() {
        return Err(busy(&state, "Checking"));
    }
    match run_check(&app, &state).await {
        Ok(outcome) => {
            publish(&app, &state, resting(&before, &state));
            Ok(CheckResult::from(outcome))
        }
        Err(error) => {
            report_check_error(&app, &state, &before, &error);
            Err(error.to_payload())
        }
    }
}

#[tauri::command]
pub async fn prepare<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, Arc<PluginState>>,
) -> Result<PrepareResult, ErrorPayload> {
    let state = Arc::clone(state.inner());
    let Ok(_gate) = state.gate().try_lock() else {
        return Ok(PrepareResult {
            state: state.snapshot().name().to_string(),
            version: state
                .target_version()
                .unwrap_or_else(|| current_version(&state)),
        });
    };
    let before = state.snapshot();
    if matches!(before, EngineState::Committed { .. }) {
        return Err(busy(&state, "Preparing"));
    }
    if let Some(prepared) = state.prepared() {
        publish(
            &app,
            &state,
            EngineState::Prepared {
                version: prepared.version.clone(),
            },
        );
        return Ok(PrepareResult {
            state: "Prepared".into(),
            version: prepared.version,
        });
    }
    let outcome = match state.recent_check(CHECK_REUSE_WINDOW) {
        Some(outcome) => outcome,
        None => match run_check(&app, &state).await {
            Ok(outcome) => outcome,
            Err(error) => {
                report_check_error(&app, &state, &before, &error);
                return Err(error.to_payload());
            }
        },
    };
    if !outcome.available {
        publish(&app, &state, resting(&before, &state));
        return Err(EngineError::NotPrepared.to_payload());
    }
    match prepare_now(&app, &state, outcome.download_bytes, outcome.files_staged).await {
        Ok(prepared) => Ok(PrepareResult {
            state: "Prepared".into(),
            version: prepared.version,
        }),
        Err(error) => Err(error.to_payload()),
    }
}

#[tauri::command]
pub async fn cancel_prepare(
    state: State<'_, Arc<PluginState>>,
) -> Result<CancelResult, ErrorPayload> {
    let state = Arc::clone(state.inner());
    state
        .engine()
        .cancel_prepare()
        .map_err(|error| error.to_payload())?;
    // The in-flight prepare publishes the terminal Cancelled/Prepared state
    // once it actually stops; publishing here could race a just-finished
    // prepare and leave Cancelled and Prepared inconsistent.
    Ok(CancelResult {
        state: "Cancelled".into(),
    })
}

#[tauri::command]
pub async fn status(state: State<'_, Arc<PluginState>>) -> Result<StatePayload, ErrorPayload> {
    Ok(state.payload(&state.snapshot()))
}

#[tauri::command]
pub async fn confirm_healthy<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, Arc<PluginState>>,
) -> Result<ConfirmResult, ErrorPayload> {
    let state = Arc::clone(state.inner());
    // Intentionally not gated: a long prepare must not block (or fail) the
    // health acknowledgement, otherwise repeated slow downloads look like a
    // crash loop and trigger a rollback.
    state
        .engine()
        .confirm_healthy()
        .map_err(|error| error.to_payload())?;
    state.mark_awaiting_confirm(false);
    if matches!(state.snapshot(), EngineState::Committed { .. }) {
        publish(&app, &state, EngineState::Idle);
    }
    Ok(ConfirmResult { acknowledged: true })
}

#[tauri::command]
pub async fn restart_to_apply<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, Arc<PluginState>>,
) -> Result<RestartResult, ErrorPayload> {
    let state = Arc::clone(state.inner());
    let Ok(_gate) = state.gate().try_lock() else {
        return Err(busy(&state, "Committing"));
    };
    let Some(prepared) = state.prepared() else {
        return Err(EngineError::NotPrepared.to_payload());
    };
    if !matches!(state.snapshot(), EngineState::Prepared { .. }) {
        return Err(EngineError::NotPrepared.to_payload());
    }
    let pending = state
        .engine()
        .begin_commit(&prepared)
        .map_err(|error| error.to_payload())?;
    let install_root = state.engine().config().install_root.clone();
    if let Err(error) = crate::applier::spawn_replacement(
        &install_root,
        state.journal_root(),
        &pending.commit_id,
        true,
    ) {
        if let Err(discard) = state.engine().discard_pending_commit() {
            tracing::error!(%discard, "failed to withdraw the pending commit");
        }
        return Err(error.to_payload());
    }

    publish(
        &app,
        &state,
        EngineState::Committing {
            processed: 0,
            total: 0,
        },
    );
    let result = RestartResult {
        state: "Committing".into(),
        version: pending.version,
    };
    app.exit(0);
    Ok(result)
}
