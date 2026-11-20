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
    let current_exe = std::env::current_exe()
        .map_err(|err| EngineError::ConfigInvalid(format!("current exe: {err}")))?;
    let install_root = if cfg!(debug_assertions) {
        // Tests and `tauri dev` often run the executable outside the configured
        // install_dir. Production builds use the strict resolver below.
        match config.install_dir.as_deref() {
            Some(path) => dunce::canonicalize(path).map_err(|err| {
                EngineError::ConfigInvalid(format!(
                    "install_dir ({}) is not accessible: {err}",
                    path.display()
                ))
            })?,
            None => current_exe
                .parent()
                .ok_or_else(|| EngineError::ConfigInvalid("current exe has no parent".into()))
                .and_then(|parent| {
                    dunce::canonicalize(parent).map_err(|err| {
                        EngineError::ConfigInvalid(format!("current exe parent: {err}"))
                    })
                })?,
        }
    } else {
        delta_update_engine::path::resolve_install_root(
            config.install_dir.as_deref(),
            &current_exe,
        )?
    };
    let exe_parent = current_exe
        .parent()
        .map(std::path::Path::to_path_buf)
        .and_then(|parent| dunce::canonicalize(parent).ok());
    #[cfg(debug_assertions)]
    if config.install_dir.is_some() && exe_parent.as_ref() != Some(&install_root) {
        tracing::warn!(
            "install_dir differs from the running executable's directory: check/prepare work, but the replacement worker requires install_dir == exe parent, so restart_to_apply/auto_apply will not replace files"
        );
    }
    #[cfg(not(debug_assertions))]
    if config.install_dir.is_some() && exe_parent.as_ref() != Some(&install_root) {
        // A release build must update exactly the directory it runs from. A
        // broader install_dir (for example Program Files instead of the app
        // folder) would give the elevated worker a much larger write scope.
        return Err(EngineError::ConfigInvalid(format!(
            "install_dir ({}) must be the running executable's directory ({})",
            install_root.display(),
            exe_parent
                .as_deref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "<unknown>".into())
        )));
    }

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
            ..FetchLimits::default()
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
    )?
    .with_signing_headers(
        config
            .signing_headers
            .clone()
            .unwrap_or_default()
            .into_iter()
            .collect(),
    )?;
    // The keys are embedded at build time; with the `pinned-keys` feature they
    // are narrowed to the set the release environment compiled in.
    let signing_public_keys = crate::pinned::effective_keys(&config.signing_public_keys)?;
    // One line that answers the first support question: which endpoint,
    // channel and version did this process actually resolve? Credentials,
    // query strings and path prefixes are deliberately omitted.
    tracing::info!(
        origin = %endpoint_origin(&endpoint),
        channel = %config.channel,
        current_version = %app.package_info().version,
        install_root = %install_root.display(),
        journal_root = %journal_root.display(),
        auto_check = config.auto_check,
        auto_apply = config.auto_apply,
        auto_confirm = config.auto_confirm,
        "delta update configured"
    );
    let engine = Engine::new(EngineConfig {
        endpoint,
        signing_public_keys,
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

/// Scheme + host + port only: presigned endpoints and custom prefixes must not
/// end up in logs or support bundles.
fn endpoint_origin(url: &Url) -> String {
    let host = url.host_str().unwrap_or("<unknown>");
    match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    }
}

/// Recover a pending journal, escalating once to an elevated worker when a
/// per-machine install root cannot be written by the current user.
///
/// The worker counts this startup attempt and the caller then inspects the
/// result with `recover_after_elevation`, so the crash-loop counter advances
/// exactly once per start.
fn recover_at_startup(state: &PluginState) -> Result<StartupOutcome, EngineError> {
    let engine = state.engine()?;
    if engine.recovery_needs_install_write()? {
        let config = engine.config();
        if crate::applier::needs_elevation(&config.install_root)? {
            if let Err(error) = crate::applier::run_elevated_recovery(
                state.journal_root(),
                config.crash_loop_threshold,
            ) {
                tracing::warn!(
                    %error,
                    "elevated startup recovery did not complete; the next start retries the same journal"
                );
                return Err(error);
            }
            return engine.recover_after_elevation();
        }
    }
    engine.recover_and_cleanup()
}

/// Worker mode must be handled before Tauri starts. If it reaches setup, the
/// host forgot the `run_applier_from_context()` entry and the update would
/// otherwise be silently lost.
pub(crate) fn worker_mode_requested<I: IntoIterator<Item = OsString>>(args: I) -> bool {
    args.into_iter().next().is_some_and(|arg| {
        arg == OsStr::new("--delta-update-apply") || arg == OsStr::new("--delta-update-recover")
    })
}

/// Setup runs recovery before auto-check and user interaction.
///
/// Configuration or platform errors must not prevent the host application from
/// starting. The update feature is disabled, the error is emitted as a state
/// payload, and the rest of the window lifecycle continues normally.
pub fn setup<R: Runtime>(app: &AppHandle<R>, config: &Config) -> Result<(), EngineError> {
    if worker_mode_requested(std::env::args_os().skip(1)) {
        return Err(EngineError::ConfigInvalid(
            "worker mode was not handled: call tauri_plugin_delta_update::run_applier_from_context(&context) before tauri::Builder::run".into(),
        ));
    }
    if !crate::entry_used() {
        // The plugin is installed and configured, so the host clearly meant to
        // use it, yet the process entry was the plain `.run(context)`. Updates
        // would prepare fine and then never be applied, because the restart
        // lands in a worker mode this binary never handles. Debug builds panic
        // so the mistake cannot be shipped; release builds disable the feature
        // with the same message instead of crashing on every launch.
        let message = "delta-update is installed but its entry point was not used: replace `.run(context)` with `.run_with_delta_update(context)`, or call run_applier_from_context(&context) before tauri::Builder::run";
        #[cfg(debug_assertions)]
        panic!("{message}");
        #[cfg(not(debug_assertions))]
        return setup_disabled(app, config, EngineError::ConfigInvalid(message.into()));
    }
    if let Err(error) = crate::desktop::ensure_supported() {
        return setup_disabled(app, config, error);
    }
    if let Err(error) = config.validate() {
        return setup_disabled(app, config, error);
    }
    #[cfg(debug_assertions)]
    if config.auto_apply {
        tracing::warn!(
            "auto_apply is enabled in a debug build: a prepared update can replace the development binary; set auto_apply=false or install_dir for local development"
        );
    }
    #[cfg(debug_assertions)]
    if config.auto_check {
        tracing::warn!(
            "auto_check is enabled in a debug build and will contact the configured endpoint on every launch; set auto_check=false for local development"
        );
    }
    #[cfg(not(debug_assertions))]
    if config.allow_insecure_http {
        tracing::warn!(
            "allow_insecure_http is enabled in a release build; plain HTTP is only accepted for loopback endpoints"
        );
    }

    let (engine, journal_root) = match build_engine(app, config) {
        Ok(value) => value,
        Err(error) => return setup_disabled(app, config, error),
    };
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
            match state.engine().and_then(|engine| engine.resume_prepared()) {
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
    if !state.is_disabled() {
        if let Some(interval) = config.check_interval_secs {
            start_periodic_check(
                AppHandle::clone(app),
                Arc::clone(&state),
                Duration::from_secs(interval),
            );
        }
    }
    Ok(())
}

fn setup_disabled<R: Runtime>(
    app: &AppHandle<R>,
    config: &Config,
    error: EngineError,
) -> Result<(), EngineError> {
    tracing::error!(%error, "delta-update is disabled; the host application will keep running");
    let state = Arc::new(PluginState::disabled(&error, config));
    let next = failed_state(&error);
    state.set_state(next.clone());
    app.manage(Arc::clone(&state));
    emit_state(app, state.payload(&next));
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
        // `setup` already spawns `auto_check_prepare`; consume the immediate
        // first tick so periodic checks start after one full interval instead
        // of racing the initial check.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if !matches!(
                state.snapshot(),
                EngineState::Idle
                    | EngineState::UpdateRequired { .. }
                    | EngineState::Failed { .. }
                    | EngineState::RolledBack { .. }
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
        match state.engine().and_then(|engine| engine.confirm_healthy()) {
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
///
/// A clean exit also discounts one unconfirmed start: the process survived past
/// the grace period, so it did not crash. Crashes never reach this hook.
pub fn on_exit<R: Runtime>(app: &AppHandle<R>) {
    let Some(state) = app.try_state::<Arc<PluginState>>() else {
        return;
    };
    note_clean_exit(&state);
    let Ok(engine) = state.engine() else {
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
    match engine.begin_commit(&prepared) {
        Ok(pending) => {
            let install_root = &engine.config().install_root;
            if let Err(error) = crate::applier::spawn_replacement(
                install_root,
                state.journal_root(),
                &pending.commit_id,
                false,
            ) {
                if let Err(discard) = engine.discard_pending_commit() {
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

/// Discount one startup attempt when a committed update closes normally after
/// the grace period. Best effort: a failure only leaves the stricter counter.
fn note_clean_exit(state: &PluginState) {
    if !state.awaiting_confirm() || !state.clean_exit_grace_elapsed() {
        return;
    }
    let Ok(engine) = state.engine() else {
        return;
    };
    if let Err(error) = engine.discount_clean_exit() {
        tracing::warn!(%error, "clean exit was not discounted");
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
        Progress::Scanning {
            file,
            completed,
            total,
        } => Some((
            // `total` is the unchanged-file count for this phase, not
            // `files_staged`. The following Downloading event resets the bar.
            EngineState::Preparing {
                verified: u32::try_from(completed).unwrap_or(u32::MAX),
                total: u32::try_from(total).unwrap_or(u32::MAX),
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
    fn endpoint_log_line_drops_credentials_query_and_path() {
        let url = Url::parse(
            "https://user:secret@updates.example.com:8443/private/manifests/latest.json?token=abc",
        )
        .unwrap();
        assert_eq!(endpoint_origin(&url), "https://updates.example.com:8443");
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
