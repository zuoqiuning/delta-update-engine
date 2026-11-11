use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use delta_update_engine::fetch::{FetchLimits, HttpFetcher};
use delta_update_engine::{
    Engine, EngineConfig, EngineError, EngineState, Limits, Progress, StartupOutcome,
};
use tauri::{AppHandle, Manager, Runtime};
use url::Url;

use crate::commands;
use crate::config::Config;
use crate::events::emit_state;
use crate::state::{failed_state, PluginState};

const SIGNATURE_MAX_BYTES: u64 = 64 * 1024;

/// Build the engine from plugin config, app paths and package metadata.
pub fn build_engine<R: Runtime>(
    app: &AppHandle<R>,
    config: &Config,
) -> Result<(Engine, PathBuf), EngineError> {
    let data_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|err| EngineError::ConfigInvalid(format!("app_local_data_dir: {err}")))?;
    let base = data_dir.join("delta-update");
    let staging_root = config
        .staging_dir
        .clone()
        .unwrap_or_else(|| base.join("staging"));
    let journal_root = config
        .journal_dir
        .clone()
        .unwrap_or_else(|| base.join("journal"));
    let install_root = match &config.install_dir {
        Some(path) => path.clone(),
        None => std::env::current_exe()
            .map_err(|err| EngineError::ConfigInvalid(format!("current exe: {err}")))?
            .parent()
            .ok_or_else(|| EngineError::ConfigInvalid("current exe has no parent".into()))?
            .to_path_buf(),
    };

    let endpoint = Url::parse(&config.endpoint)
        .map_err(|err| EngineError::ConfigInvalid(format!("endpoint: {err}")))?;
    let signing_endpoint = config
        .signing_endpoint
        .as_deref()
        .map(Url::parse)
        .transpose()
        .map_err(|err| EngineError::ConfigInvalid(format!("signing_endpoint: {err}")))?;

    let mut limits = Limits::default();
    if let Some(value) = config.staging_size_limit {
        limits.staging_size_limit = value;
    }
    if let Some(value) = config.max_patch_file_size {
        limits.max_patch_file_size = value;
    }
    if let Some(value) = config.patch_memory_factor {
        limits.patch_memory_factor = value;
    }
    if let Some(value) = config.manifest_max_bytes {
        limits.manifest_max_bytes = value;
    }
    if let Some(value) = config.max_files {
        limits.max_files = value;
    }
    limits.max_concurrent_downloads = config.max_concurrent_downloads;

    let fetcher = HttpFetcher::new_with_options(
        endpoint.clone(),
        signing_endpoint,
        FetchLimits {
            manifest_max_bytes: limits.manifest_max_bytes,
            signature_max_bytes: SIGNATURE_MAX_BYTES,
            object_max_bytes: limits.max_full_bytes,
        },
        config.allowed_hosts.clone(),
        config.allow_insecure_http,
        config.proxy.as_deref(),
        config
            .headers
            .clone()
            .unwrap_or_default()
            .into_iter()
            .collect(),
    )?;
    let engine = Engine::new(EngineConfig {
        endpoint,
        signing_public_keys: config.signing_public_keys.clone(),
        channel: config.channel.clone(),
        current_version: app.package_info().version.to_string(),
        install_root,
        staging_root,
        journal_root: journal_root.clone(),
        limits,
        fetcher: Arc::new(fetcher),
        allow_insecure_http: config.allow_insecure_http,
        crash_loop_threshold: u32::from(config.crash_loop_threshold),
    })?;
    Ok((engine, journal_root))
}

/// Recover a pending journal, escalating once to an elevated worker when a
/// per-machine install root cannot be written by the current user.
///
/// The worker counts this startup attempt and the caller then inspects the
/// result with `recover_after_elevation`, so the crash-loop counter advances
/// exactly once per start.
fn recover_at_startup(state: &PluginState) -> Result<StartupOutcome, EngineError> {
    let engine = state.engine();
    if engine.recovery_needs_install_write()? {
        let config = engine.config();
        if crate::applier::needs_elevation(&config.install_root)? {
            crate::applier::run_elevated_recovery(
                state.journal_root(),
                config.crash_loop_threshold,
            )?;
            return engine.recover_after_elevation();
        }
    }
    engine.recover_and_cleanup()
}

/// Worker mode must be handled before Tauri starts. If it reaches setup, the
/// host forgot the `run_applier_from_args()` entry and the update would
/// otherwise be silently lost.
fn worker_mode_requested<I: IntoIterator<Item = OsString>>(args: I) -> bool {
    args.into_iter().next().is_some_and(|arg| {
        arg == OsStr::new("--delta-update-apply") || arg == OsStr::new("--delta-update-recover")
    })
}

/// Setup runs recovery before auto-check and user interaction.
pub fn setup<R: Runtime>(app: &AppHandle<R>, config: &Config) -> Result<(), EngineError> {
    crate::desktop::ensure_supported()?;
    if worker_mode_requested(std::env::args_os().skip(1)) {
        return Err(EngineError::ConfigInvalid(
            "worker mode was not handled: call tauri_plugin_delta_update::run_applier_from_args() at the start of main".into(),
        ));
    }
    #[cfg(debug_assertions)]
    if config.auto_apply {
        tracing::warn!(
            "auto_apply is enabled in a debug build: a prepared update can replace the development binary; set auto_apply=false or install_dir for local development"
        );
    }

    let (engine, journal_root) = build_engine(app, config)?;
    let state = Arc::new(PluginState::new(engine, config, journal_root));

    let (next, prepared) = match recover_at_startup(&state) {
        Ok(StartupOutcome::Clean) => (EngineState::Idle, None),
        Ok(StartupOutcome::Recovered {
            ops_completed,
            version,
        }) => {
            tracing::info!(%version, ops_completed, "journal replay completed");
            (EngineState::Committed { version }, None)
        }
        Ok(StartupOutcome::Committed { version }) => {
            tracing::info!(%version, "committed update awaits health confirmation");
            (EngineState::Committed { version }, None)
        }
        Ok(StartupOutcome::RolledBack { to_version, reason }) => {
            tracing::warn!(%to_version, %reason, "unconfirmed update rolled back");
            (EngineState::RolledBack { to_version, reason }, None)
        }
        Ok(StartupOutcome::ReusedPrepared { version }) => {
            tracing::info!(%version, "reusing prepared update");
            match state.engine().resume_prepared() {
                Ok(prepared) => (EngineState::Prepared { version }, Some(prepared)),
                Err(error) => (failed_state(&error), None),
            }
        }
        Ok(StartupOutcome::NeedsAttention { error }) => {
            tracing::error!(%error, "recovery needs attention");
            (failed_state(&error), None)
        }
        Err(error) => (failed_state(&error), None),
        Ok(_) => (EngineState::Idle, None),
    };
    state.set_state(next.clone());
    state.set_prepared(prepared);
    if matches!(next, EngineState::Committed { .. }) {
        state.mark_awaiting_confirm(true);
    }
    app.manage(Arc::clone(&state));
    emit_state(app, state.payload(&next));

    if config.auto_check && matches!(next, EngineState::Idle) {
        let app = AppHandle::clone(app);
        let state = Arc::clone(&state);
        tauri::async_runtime::spawn(async move {
            commands::auto_check_prepare(&app, state).await;
        });
    }
    if let Some(interval) = config.check_interval_secs {
        start_periodic_check(
            AppHandle::clone(app),
            Arc::clone(&state),
            Duration::from_secs(interval),
        );
    }
    Ok(())
}

/// Re-run check+prepare on an interval. The task ends with the app runtime.
fn start_periodic_check<R: Runtime>(
    app: AppHandle<R>,
    state: Arc<PluginState>,
    interval: Duration,
) {
    tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // A laptop waking from sleep must not fire a burst of missed checks.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if !matches!(
                state.snapshot(),
                EngineState::Idle | EngineState::Failed { .. } | EngineState::RolledBack { .. }
            ) {
                continue;
            }
            commands::periodic_check_prepare(&app, Arc::clone(&state)).await;
        }
    });
}

/// Auto-confirm after the first window loads.
pub fn on_window_ready<R: Runtime>(window: &tauri::Window<R>) {
    let Some(state) = window.app_handle().try_state::<Arc<PluginState>>() else {
        return;
    };
    if !state.auto_confirm() || state.confirm_delay_ms() == 0 {
        return;
    }
    if !state.awaiting_confirm() {
        return;
    }
    if !state.mark_auto_confirm_started() {
        return;
    }

    let delay = state.confirm_delay_ms();
    let app = AppHandle::clone(window.app_handle());
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(delay)).await;
        if !state.awaiting_confirm() {
            return;
        }
        match state.engine().confirm_healthy() {
            Ok(()) => {
                state.mark_awaiting_confirm(false);
                let next = EngineState::Idle;
                state.set_state(next.clone());
                emit_state(&app, state.payload(&next));
            }
            Err(error) => tracing::warn!(%error, "auto confirm failed"),
        }
    });
}

/// `auto_apply` exits path: persist the journal and spawn the worker.
pub fn on_exit<R: Runtime>(app: &AppHandle<R>) {
    let Some(state) = app.try_state::<Arc<PluginState>>() else {
        return;
    };
    if !state.auto_apply() {
        return;
    }
    let Ok(_gate) = state.gate().try_lock() else {
        return;
    };
    if !matches!(state.snapshot(), EngineState::Prepared { .. }) {
        return;
    }
    let Some(prepared) = state.prepared() else {
        return;
    };
    match state.engine().begin_commit(&prepared) {
        Ok(pending) => {
            let install_root = &state.engine().config().install_root;
            if let Err(error) = crate::applier::spawn_replacement(
                install_root,
                state.journal_root(),
                &pending.commit_id,
                false,
            ) {
                if let Err(discard) = state.engine().discard_pending_commit() {
                    tracing::error!(%discard, "failed to withdraw the pending commit");
                }
                tracing::warn!(
                    %error,
                    "replacement process was not started; the update stays prepared"
                );
                return;
            }
            let next = EngineState::Committing {
                processed: 0,
                total: 0,
            };
            state.set_state(next.clone());
            emit_state(app, state.payload(&next));
        }
        Err(error) => tracing::error!(%error, "failed to persist commit journal"),
    }
}

/// Progress adapter for prepare events. The file name is returned separately so
/// the plugin can attach it to the wire payload without widening `EngineState`.
pub fn progress_state(
    progress: Progress,
    verified: &AtomicU32,
    total_files: u32,
) -> Option<(EngineState, Option<String>)> {
    match progress {
        Progress::Downloading {
            file,
            total,
            completed,
        } => Some((EngineState::Downloading { total, completed }, Some(file))),
        Progress::Applying { file } => Some((
            EngineState::Preparing {
                verified: verified.load(Ordering::Relaxed),
                total: total_files,
            },
            Some(file),
        )),
        Progress::Verifying { file } => Some((
            EngineState::Preparing {
                verified: verified.fetch_add(1, Ordering::Relaxed) + 1,
                total: total_files,
            },
            Some(file),
        )),
        Progress::Done => None,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_mode_is_detected_from_the_first_argument() {
        assert!(worker_mode_requested([OsString::from(
            "--delta-update-apply"
        )]));
        assert!(worker_mode_requested([OsString::from(
            "--delta-update-recover"
        )]));
        assert!(!worker_mode_requested([OsString::from("--help")]));
        assert!(!worker_mode_requested(std::iter::empty()));
    }

    #[test]
    fn progress_maps_download_apply_and_verify() {
        let verified = AtomicU32::new(0);

        assert_eq!(
            progress_state(
                Progress::Downloading {
                    file: "a.bin".into(),
                    total: 10,
                    completed: 4,
                },
                &verified,
                3,
            ),
            Some((
                EngineState::Downloading {
                    total: 10,
                    completed: 4,
                },
                Some("a.bin".into())
            ))
        );
        assert_eq!(
            progress_state(
                Progress::Applying {
                    file: "a.bin".into(),
                },
                &verified,
                3,
            ),
            Some((
                EngineState::Preparing {
                    verified: 0,
                    total: 3,
                },
                Some("a.bin".into())
            ))
        );
        assert_eq!(
            progress_state(
                Progress::Verifying {
                    file: "a.bin".into(),
                },
                &verified,
                3,
            ),
            Some((
                EngineState::Preparing {
                    verified: 1,
                    total: 3,
                },
                Some("a.bin".into())
            ))
        );
        assert_eq!(verified.load(Ordering::Relaxed), 1);
        assert_eq!(progress_state(Progress::Done, &verified, 3), None);
    }
}
