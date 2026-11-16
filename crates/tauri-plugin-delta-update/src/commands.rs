use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use delta_update_engine::{
    CheckOutcome, EngineError, EngineState, ErrorPayload, PreparedUpdate, StatePayload,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime, State};

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
    /// The running version is below the publisher's `min_supported_version`.
    #[serde(default)]
    pub update_required: bool,
    /// Oldest version the publisher still supports; absent when unset.
    #[serde(default)]
    pub min_supported_version: Option<String>,
    /// Files skipped because they are cloud placeholders; they will be
    /// downloaded in full if the manifest lists them. `default` keeps payloads
    /// from older plugin versions deserializable.
    #[serde(default)]
    pub cloud_placeholders_skipped: u32,
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
            update_required: outcome.update_required,
            min_supported_version: outcome.min_supported_version,
            cloud_placeholders_skipped: outcome.cloud_placeholders_skipped,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticsResult {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClearSkipResult {
    pub cleared: bool,
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
    if let Some(prepared) = state.prepared() {
        return EngineState::Prepared {
            version: prepared.version,
        };
    }
    if let Some(min_supported_version) = state.min_supported_version() {
        return EngineState::UpdateRequired {
            version: state
                .target_version()
                .unwrap_or_else(|| state.current_version().to_string()),
            min_supported_version,
        };
    }
    EngineState::Idle
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
    state.current_version().to_string()
}

async fn run_check<R: Runtime>(
    app: &AppHandle<R>,
    state: &PluginState,
) -> Result<CheckOutcome, EngineError> {
    publish(app, state, EngineState::Checking);
    let outcome = state.engine()?.check().await?;
    if outcome.available {
        state.set_target_version(outcome.version.clone());
    } else {
        state.set_target_version(None);
    }
    state.set_min_supported_version(
        outcome
            .min_supported_version
            .clone()
            .filter(|_| outcome.update_required),
    );
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
        .engine()?
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
    let allowed = matches!(
        before,
        EngineState::Idle | EngineState::UpdateRequired { .. }
    ) || (retry_settled
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
    // The failure state is already published by `prepare_now`; log it so a host
    // that only reads its own logs still sees why the background prepare stopped.
    if let Err(error) = prepare_now(app, &state, outcome.download_bytes, outcome.files_staged).await
    {
        tracing::warn!(%error, "background prepare failed after an automatic check");
    }
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
        .and_then(|engine| engine.cancel_prepare())
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

/// Forget the rollback skip marker so the same version can be tried again.
///
/// Needed when a publisher fixes a release and re-uploads the same version:
/// `check` would otherwise skip it until a higher version appears.
#[tauri::command]
pub async fn clear_skipped_version(
    state: State<'_, Arc<PluginState>>,
) -> Result<ClearSkipResult, ErrorPayload> {
    let engine = state.engine().map_err(|error| error.to_payload())?;
    let cleared =
        delta_update_engine::journal::state::read_skipped_version(state.journal_root()).is_some();
    engine
        .clear_skipped_version()
        .map_err(|error| error.to_payload())?;
    Ok(ClearSkipResult { cleared })
}

/// Write a support bundle next to the app logs and return its path.
///
/// The destination is chosen by the plugin, never by the frontend, so this
/// command cannot be used to write arbitrary files. Users can attach the
/// returned JSON to a ticket without collecting scattered state by hand.
#[tauri::command]
pub async fn export_diagnostics<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, Arc<PluginState>>,
) -> Result<DiagnosticsResult, ErrorPayload> {
    let journal_root = state.journal_root().clone();
    let install_root = state
        .engine()
        .ok()
        .map(|engine| engine.config().install_root.clone());
    let dir = app
        .path()
        .app_log_dir()
        .map_err(|error| EngineError::Internal(format!("app_log_dir: {error}")).to_payload())?;
    std::fs::create_dir_all(&dir).map_err(|error| {
        EngineError::Internal(format!("create {}: {error}", dir.display())).to_payload()
    })?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let path = dir.join(format!("delta-update-diagnostics-{stamp}.json"));
    delta_update_engine::diagnostics::write_bundle(
        &journal_root,
        true,
        "tauri-plugin-delta-update",
        &path,
        install_root.as_deref(),
        true,
    )
    .map_err(|error| error.to_payload())?;
    Ok(DiagnosticsResult {
        path: path.to_string_lossy().into_owned(),
    })
}

#[tauri::command]
pub async fn get_update_history(
    state: State<'_, Arc<PluginState>>,
    limit: Option<usize>,
) -> Result<Vec<delta_update_engine::UpdateRecord>, ErrorPayload> {
    let engine = state.engine().map_err(|error| error.to_payload())?;
    // The file is capped anyway; the clamp only keeps a hostile caller from
    // asking for an unbounded response.
    Ok(engine.update_history(limit.unwrap_or(50).min(500)))
}

/// Open the app log directory in Explorer so the user can attach the newest
/// bundle without searching the filesystem by hand.
///
/// The path comes from `app_log_dir()`, not from the frontend, so this cannot
/// be used to open an arbitrary location.
#[tauri::command]
pub async fn open_diagnostics_folder<R: Runtime>(app: AppHandle<R>) -> Result<(), ErrorPayload> {
    let dir = app
        .path()
        .app_log_dir()
        .map_err(|error| EngineError::Internal(format!("app_log_dir: {error}")).to_payload())?;
    std::process::Command::new("explorer")
        .arg(&dir)
        .spawn()
        .map(|_| ())
        .map_err(|error| {
            EngineError::Internal(format!("open {}: {error}", dir.display())).to_payload()
        })
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
        .and_then(|engine| engine.confirm_healthy())
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
    let engine = state.engine().map_err(|error| error.to_payload())?;
    let pending = engine
        .begin_commit(&prepared)
        .map_err(|error| error.to_payload())?;
    let install_root = engine.config().install_root.clone();
    if let Err(error) = crate::applier::spawn_replacement(
        &install_root,
        state.journal_root(),
        &pending.commit_id,
        true,
    ) {
        if let Err(discard) = engine.discard_pending_commit() {
            tracing::error!(%discard, "failed to withdraw the pending commit");
        }
        // Keep the observable state in sync with the command result: otherwise
        // a UI that only listens to events still shows `Prepared`.
        publish(&app, &state, failed_state(&error));
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
