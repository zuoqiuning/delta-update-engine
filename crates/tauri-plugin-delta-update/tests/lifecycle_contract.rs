//! L3 lifecycle and command-surface contract tests driven through the Tauri
//! mock runtime.
//!
//! These tests build a real plugin instance with a synthetic app config, so
//! recovery, auto-check, auto-apply and IPC dispatch run through the same
//! hooks a shipped application uses.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use delta_update_engine::apply::apply_commit;
use delta_update_engine::crypto::sha256_bytes;
use delta_update_engine::fetch::{FetchLimits, HttpFetcher};
use delta_update_engine::journal::state;
use delta_update_engine::{Engine, EngineConfig, EngineState, Limits, StatePayload};
use delta_update_testkit::manifest::sample_manifest;
use delta_update_testkit::{tree, MockServer, TestKeyPair};
use tauri::test::{mock_builder, mock_context, noop_assets, MockRuntime};
use tauri::{Listener, Manager, WebviewWindowBuilder};
use tauri_plugin_delta_update::state::PluginState;
use tauri_plugin_delta_update::STATE_EVENT;
use tempfile::TempDir;
use url::Url;

/// The mock context reports this package version.
const CURRENT_VERSION: &str = "0.1.0";
const NEW_VERSION: &str = "0.2.0";
const CHANNEL: &str = "stable";

/// Tauri builds share process-wide runtime state, so the tests run one at a
/// time to keep events and windows isolated.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Layout {
    home: TempDir,
    staging: TempDir,
}

impl Layout {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("home");
        std::fs::create_dir_all(home.path().join("root")).expect("root dir");
        std::fs::create_dir_all(home.path().join("journal")).expect("journal dir");
        let staging = tempfile::tempdir().expect("staging");
        Self { home, staging }
    }

    fn root(&self) -> PathBuf {
        self.home.path().join("root")
    }

    fn journal(&self) -> PathBuf {
        self.home.path().join("journal")
    }

    fn staging(&self) -> PathBuf {
        self.staging.path().to_path_buf()
    }
}

fn plugin_config(
    layout: &Layout,
    server: &MockServer,
    keys: &TestKeyPair,
    auto_check: bool,
    auto_apply: bool,
) -> serde_json::Value {
    serde_json::json!({
        "config_version": 1,
        "endpoint": server.url("manifests/latest.json"),
        "signing_public_keys": [keys.public_key()],
        "auto_check": auto_check,
        "auto_apply": auto_apply,
        "auto_confirm": false,
        "channel": CHANNEL,
        "install_dir": layout.root(),
        "staging_dir": layout.staging(),
        "journal_dir": layout.journal(),
        "allow_insecure_http": true,
    })
}

/// Build the plugin against a mock app whose `plugins.delta-update` config is
/// the one under test. `allowed` grants ACL entries to plugin commands, which
/// lets a test reach the handler instead of the ACL gate.
fn build_app(config: &serde_json::Value, allowed: &[&str]) -> tauri::App<MockRuntime> {
    let mut context = mock_context(noop_assets());
    for command in allowed {
        context.runtime_authority_mut().__allow_command(
            (*command).into(),
            tauri::utils::acl::ExecutionContext::Local,
        );
    }
    context
        .config_mut()
        .plugins
        .0
        .insert("delta-update".into(), config.clone());
    mock_builder()
        .plugin(tauri_plugin_delta_update::init())
        .build(context)
        .expect("build mock app")
}

fn invoke(
    webview: &tauri::WebviewWindow<MockRuntime>,
    cmd: &str,
    body: tauri::ipc::InvokeBody,
) -> Result<tauri::ipc::InvokeResponseBody, serde_json::Value> {
    tauri::test::get_ipc_response(
        webview,
        tauri::webview::InvokeRequest {
            cmd: cmd.into(),
            callback: tauri::ipc::CallbackFn(0),
            error: tauri::ipc::CallbackFn(1),
            url: "http://tauri.localhost".parse().expect("url"),
            body,
            headers: Default::default(),
            invoke_key: tauri::test::INVOKE_KEY.to_string(),
        },
    )
}

fn plugin_state(app: &tauri::App<MockRuntime>) -> Arc<PluginState> {
    app.state::<Arc<PluginState>>().inner().clone()
}

/// Serve one signed full-artifact release for `version`.
async fn release_server(keys: &TestKeyPair, version: &str, body: &[u8]) -> MockServer {
    let manifest = sample_manifest(version, &sha256_bytes(body), body.len() as u64);
    let manifest = serde_json::to_vec(&manifest).expect("serialize manifest");
    let signature = keys.sign(&manifest);
    MockServer::start(vec![
        ("manifests/latest.json".to_string(), manifest),
        (
            "manifests/latest.json.minisig".to_string(),
            signature.into_bytes(),
        ),
        (format!("artifacts/full/{version}/App.exe"), body.to_vec()),
    ])
    .await
    .expect("mock server")
}

fn engine_for(layout: &Layout, server: &MockServer, keys: &TestKeyPair) -> Engine {
    let endpoint = Url::parse(&server.url("manifests/latest.json")).expect("endpoint");
    let fetcher =
        HttpFetcher::new_default(endpoint.clone(), None, FetchLimits::default(), None, true)
            .expect("fetcher");
    Engine::new(EngineConfig {
        endpoint,
        signing_public_keys: vec![keys.public_key().to_string()],
        channel: CHANNEL.into(),
        current_version: CURRENT_VERSION.into(),
        install_root: layout.root(),
        staging_root: layout.staging(),
        journal_root: layout.journal(),
        limits: Limits::default(),
        fetcher: Arc::new(fetcher),
        allow_insecure_http: true,
        crash_loop_threshold: 2,
    })
    .expect("engine")
}

/// Apply a prepared update without confirming it, leaving the journal in the
/// state a crashed startup finds.
async fn install_unconfirmed(layout: &Layout, server: &MockServer, keys: &TestKeyPair) {
    let engine = engine_for(layout, server, keys);
    engine.check().await.expect("check");
    let prepared = engine.prepare(Box::new(|_| {})).await.expect("prepare");
    let pending = engine.begin_commit(&prepared).expect("begin_commit");
    apply_commit(&layout.journal(), &pending.commit_id).expect("apply journal");
}

async fn wait_for<F>(state: &PluginState, predicate: F, timeout: Duration)
where
    F: Fn(&EngineState) -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let snapshot = state.snapshot();
        if predicate(&snapshot) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out in state {snapshot:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Install `App.exe`, apply an unconfirmed commit and return its id.
async fn seed_unconfirmed(layout: &Layout, server: &MockServer, keys: &TestKeyPair) -> String {
    tree::write_tree(&layout.root(), &[("App.exe", b"build 0.1.0".as_slice())])
        .expect("seed install");
    install_unconfirmed(layout, server, keys).await;
    state::read_apply_config(&layout.journal())
        .expect("apply config")
        .expect("apply.json")
        .commit_id
}

/// P-01 / P-11: setup replays a committed journal before any window exists and
/// rolls back after the crash-loop threshold.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn setup_recovers_committed_update_and_rolls_back_crash_loop() {
    let _serial = SERIAL.lock().await;
    let layout = Layout::new();
    let old = b"build 0.1.0".to_vec();
    let new = b"build 0.2.0".to_vec();
    tree::write_tree(&layout.root(), &[("App.exe", old.as_slice())]).expect("seed install");

    let keys = TestKeyPair::generate();
    let server = release_server(&keys, NEW_VERSION, &new).await;
    install_unconfirmed(&layout, &server, &keys).await;

    for attempt in 1..=2 {
        let app = build_app(&plugin_config(&layout, &server, &keys, false, false), &[]);
        let state = plugin_state(&app);
        assert!(
            matches!(state.snapshot(), EngineState::Committed { ref version } if version == NEW_VERSION),
            "start {attempt} state: {:?}",
            state.snapshot()
        );
        assert!(app.webview_windows().is_empty());
        assert_eq!(
            std::fs::read(layout.root().join("App.exe")).expect("read install"),
            new
        );
    }

    let app = build_app(&plugin_config(&layout, &server, &keys, false, false), &[]);
    let state = plugin_state(&app);
    assert!(
        matches!(
            state.snapshot(),
            EngineState::RolledBack { ref to_version, .. } if to_version == "0.1.0"
        ),
        "rollback state: {:?}",
        state.snapshot()
    );
    assert_eq!(
        std::fs::read(layout.root().join("App.exe")).expect("read install"),
        old
    );
    assert_eq!(
        state::read_skipped_version(&layout.journal()).as_deref(),
        Some(NEW_VERSION)
    );
}

/// P-04: `auto_check: false` only exposes the commands.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_check_disabled_stays_idle_and_offline() {
    let _serial = SERIAL.lock().await;
    let layout = Layout::new();
    let keys = TestKeyPair::generate();
    let server = release_server(&keys, NEW_VERSION, b"build 0.2.0").await;

    let app = build_app(&plugin_config(&layout, &server, &keys, false, true), &[]);
    let state = plugin_state(&app);

    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(state.snapshot(), EngineState::Idle);
    assert!(
        server.requests().is_empty(),
        "unexpected requests: {:?}",
        server.requests()
    );
    assert!(!layout.journal().join("prepared.json").exists());
}

/// P-02 / P-03: `auto_check: true` prepares on the async runtime and emits the
/// `Prepared` state event.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_check_prepares_in_background_and_emits_prepared() {
    let _serial = SERIAL.lock().await;
    let layout = Layout::new();
    let keys = TestKeyPair::generate();
    let new = b"build 0.2.0".to_vec();
    let server = release_server(&keys, NEW_VERSION, &new).await;
    // Keep a response in flight so the build cannot have waited for prepare.
    server.set_delay(400);

    let app = build_app(&plugin_config(&layout, &server, &keys, true, false), &[]);
    let state = plugin_state(&app);

    let events = Arc::new(std::sync::Mutex::new(Vec::<(String, Option<String>)>::new()));
    let sink = Arc::clone(&events);
    app.listen_any(STATE_EVENT, move |event| {
        if let Ok(payload) = serde_json::from_str::<StatePayload>(event.payload()) {
            sink.lock()
                .expect("events")
                .push((payload.state, payload.version));
        }
    });

    assert!(
        !matches!(state.snapshot(), EngineState::Prepared { .. }),
        "setup must not block on prepare"
    );

    wait_for(
        &state,
        |state| matches!(state, EngineState::Prepared { .. }),
        Duration::from_secs(15),
    )
    .await;

    let payloads = events.lock().expect("events").clone();
    assert!(
        payloads.iter().any(|(name, _)| name == "Prepared"),
        "missing Prepared event: {payloads:?}"
    );
    let downloading = payloads
        .iter()
        .find(|(name, _)| name == "Downloading")
        .unwrap_or_else(|| panic!("missing progress events: {payloads:?}"));
    assert_eq!(
        downloading.1.as_deref(),
        Some(NEW_VERSION),
        "progress events must carry the target version"
    );
    assert!(layout.journal().join("prepared.json").exists());
    assert!(
        server
            .requests()
            .iter()
            .any(|path| path.starts_with("artifacts/full/")),
        "artifact was not downloaded: {:?}",
        server.requests()
    );
}

/// P-14: an exit with `auto_apply` flushes the commit journal before the
/// replacement worker takes over.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_apply_on_exit_flushes_commit_journal() {
    let _serial = SERIAL.lock().await;
    let layout = Layout::new();
    let keys = TestKeyPair::generate();
    let server = release_server(&keys, NEW_VERSION, b"build 0.2.0").await;

    let app = build_app(&plugin_config(&layout, &server, &keys, true, true), &[]);
    let state = plugin_state(&app);
    wait_for(
        &state,
        |state| matches!(state, EngineState::Prepared { .. }),
        Duration::from_secs(15),
    )
    .await;

    tauri_plugin_delta_update::lifecycle::on_exit(app.handle());

    assert!(
        matches!(state.snapshot(), EngineState::Committing { .. }),
        "unexpected state: {:?}",
        state.snapshot()
    );
    assert!(layout.journal().join("commit.journal").exists());
    assert!(layout.journal().join("apply.json").exists());
}

/// P-06: without a granted permission the runtime rejects plugin commands.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_commands_are_denied_without_acl() {
    let _serial = SERIAL.lock().await;
    let layout = Layout::new();
    let keys = TestKeyPair::generate();
    let server = release_server(&keys, NEW_VERSION, b"build 0.2.0").await;

    let app = build_app(&plugin_config(&layout, &server, &keys, false, false), &[]);
    let webview = WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("mock webview");

    let message = invoke(&webview, "plugin:delta-update|status", Default::default())
        .expect_err("plugin command must be rejected without ACL")
        .to_string()
        .to_lowercase();
    assert!(
        message.contains("not allowed"),
        "unexpected rejection: {message}"
    );
}

/// SEC-12: an unknown plugin command never reaches the engine, even when its
/// ACL entry is present.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_plugin_command_is_rejected() {
    let _serial = SERIAL.lock().await;
    let layout = Layout::new();
    let keys = TestKeyPair::generate();
    let server = release_server(&keys, NEW_VERSION, b"build 0.2.0").await;

    let app = build_app(
        &plugin_config(&layout, &server, &keys, false, false),
        &["plugin:delta-update|commit"],
    );
    let state = plugin_state(&app);
    let webview = WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("mock webview");

    let message = invoke(&webview, "plugin:delta-update|commit", Default::default())
        .expect_err("unknown command must be rejected")
        .to_string();
    assert!(
        message.contains("not found"),
        "unexpected rejection: {message}"
    );
    assert_eq!(state.snapshot(), EngineState::Idle);
    assert!(server.requests().is_empty());
}

/// SEC-13: commands take no path arguments; extra IPC body fields are ignored.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepare_ignores_extra_body_fields() {
    let _serial = SERIAL.lock().await;
    let layout = Layout::new();
    let outside = layout.home.path().join("outside.txt");
    let keys = TestKeyPair::generate();
    let server = release_server(&keys, NEW_VERSION, b"build 0.2.0").await;

    let app = build_app(
        &plugin_config(&layout, &server, &keys, false, false),
        &["plugin:delta-update|prepare"],
    );
    let webview = WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("mock webview");

    let body = tauri::ipc::InvokeBody::Json(serde_json::json!({
        "path": outside.display().to_string(),
        "source": outside.display().to_string(),
    }));
    let result = invoke(&webview, "plugin:delta-update|prepare", body)
        .expect("prepare")
        .deserialize::<serde_json::Value>()
        .expect("prepare result");

    assert_eq!(result["state"], "Prepared");
    assert_eq!(result["version"], NEW_VERSION);
    assert!(!outside.exists(), "command wrote outside the install root");
    assert!(layout.staging().join(NEW_VERSION).exists());
}

/// Auto-confirm: the first window schedules a delayed health confirmation and
/// the journal is cleaned once the delay elapses.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn window_ready_auto_confirms_after_delay() {
    let _serial = SERIAL.lock().await;
    let layout = Layout::new();
    let keys = TestKeyPair::generate();
    let server = release_server(&keys, NEW_VERSION, b"build 0.2.0").await;
    let commit_id = seed_unconfirmed(&layout, &server, &keys).await;

    let mut config = plugin_config(&layout, &server, &keys, false, false);
    config["auto_confirm"] = serde_json::json!(true);
    config["confirm_delay_ms"] = serde_json::json!(50);
    let app = build_app(&config, &[]);
    let state = plugin_state(&app);
    assert!(matches!(state.snapshot(), EngineState::Committed { .. }));

    let webview = WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("mock webview");
    wait_for(
        &state,
        |state| matches!(state, EngineState::Idle),
        Duration::from_secs(5),
    )
    .await;

    assert!(!layout.journal().join("commit.journal").exists());
    assert!(!layout.journal().join("commit.status").exists());
    assert!(!layout.root().join(".old").join(&commit_id).exists());
    drop(webview);
}

/// A manual check immediately followed by prepare reuses the verified result
/// instead of downloading the manifest and signature twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepare_reuses_a_recent_check() {
    let _serial = SERIAL.lock().await;
    let layout = Layout::new();
    let keys = TestKeyPair::generate();
    let server = release_server(&keys, NEW_VERSION, b"build 0.2.0").await;

    let app = build_app(
        &plugin_config(&layout, &server, &keys, false, false),
        &["plugin:delta-update|check", "plugin:delta-update|prepare"],
    );
    let state = plugin_state(&app);
    let webview = WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("mock webview");

    invoke(&webview, "plugin:delta-update|check", Default::default()).expect("check");
    invoke(&webview, "plugin:delta-update|prepare", Default::default()).expect("prepare");

    assert!(matches!(state.snapshot(), EngineState::Prepared { .. }));
    let manifest_requests = server
        .requests()
        .iter()
        .filter(|path| path.as_str() == "manifests/latest.json")
        .count();
    assert_eq!(
        manifest_requests, 1,
        "prepare re-fetched the manifest instead of reusing the check"
    );
}

/// The confirm command is the only path allowed to delete backups: it reports
/// the committed state first, then cleans every journal artefact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confirm_command_cleans_up_after_health_check() {
    let _serial = SERIAL.lock().await;
    let layout = Layout::new();
    let keys = TestKeyPair::generate();
    let server = release_server(&keys, NEW_VERSION, b"build 0.2.0").await;
    let commit_id = seed_unconfirmed(&layout, &server, &keys).await;

    let app = build_app(
        &plugin_config(&layout, &server, &keys, false, false),
        &[
            "plugin:delta-update|status",
            "plugin:delta-update|confirm_healthy",
        ],
    );
    let state = plugin_state(&app);
    let webview = WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("mock webview");

    let status = invoke(&webview, "plugin:delta-update|status", Default::default())
        .expect("status")
        .deserialize::<serde_json::Value>()
        .expect("status payload");
    assert_eq!(status["state"], "Committed");
    assert_eq!(status["version"], NEW_VERSION);

    let confirmed = invoke(
        &webview,
        "plugin:delta-update|confirm_healthy",
        Default::default(),
    )
    .expect("confirm")
    .deserialize::<serde_json::Value>()
    .expect("confirm result");
    assert_eq!(confirmed["acknowledged"], true);
    assert_eq!(state.snapshot(), EngineState::Idle);
    assert!(!layout.journal().join("commit.journal").exists());
    assert!(!layout.journal().join("commit.status").exists());
    assert!(!layout.journal().join("prepared.json").exists());
    assert!(!layout.root().join(".old").join(&commit_id).exists());
    assert!(!layout.staging().join(NEW_VERSION).exists());
}
