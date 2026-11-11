use std::sync::Arc;

use delta_update_engine::crypto::sha256_bytes;
use delta_update_engine::fetch::{FetchLimits, HttpFetcher};
use delta_update_engine::{Engine, EngineConfig, Limits};
use delta_update_testkit::{MockServer, TestKeyPair};
use serde_json::{json, Value};
use url::Url;

fn manifest(
    version: &str,
    channel: Option<&str>,
    rollback: bool,
    sha256: &str,
    size: u64,
) -> Vec<u8> {
    let mut value = json!({
        "schema": 1,
        "version": version,
        "min_client_schema": 1,
        "rollback": rollback,
        "files": {
            "App.exe": {
                "sha256": sha256,
                "size": size,
                "strategy": "full",
                "full_key": format!("artifacts/full/{version}/App.exe")
            }
        }
    });
    if let Some(channel) = channel {
        value["channel"] = Value::String(channel.to_string());
    }
    serde_json::to_vec(&value).unwrap()
}

async fn start(
    manifest: Vec<u8>,
    signer: &TestKeyPair,
    trusted: &TestKeyPair,
    current_version: &str,
) -> (tempfile::TempDir, Engine, MockServer) {
    let signature = signer.sign(&manifest);
    let server = MockServer::start([
        ("manifests/latest.json", manifest),
        ("manifests/latest.json.minisig", signature.into_bytes()),
    ])
    .await
    .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("App.exe"), b"old").unwrap();

    let endpoint = Url::parse(&server.url("manifests/latest.json")).unwrap();
    let fetcher = HttpFetcher::new(
        reqwest::Client::new(),
        endpoint.clone(),
        None,
        FetchLimits::default(),
        None,
        true,
    )
    .unwrap();
    let engine = Engine::new(EngineConfig {
        endpoint,
        signing_public_keys: vec![trusted.public_key().to_string()],
        channel: "stable".into(),
        current_version: current_version.into(),
        install_root: root,
        staging_root: dir.path().join("staging"),
        journal_root: dir.path().join("journal"),
        limits: Limits::default(),
        fetcher: Arc::new(fetcher),
        allow_insecure_http: true,
        crash_loop_threshold: 2,
    })
    .unwrap();

    (dir, engine, server)
}

#[tokio::test]
async fn check_reports_update_and_caches_verified_manifest() {
    let keys = TestKeyPair::generate();
    let update = manifest("2.0.0", Some("stable"), false, &sha256_bytes(b"new"), 3);
    let (_dir, engine, server) = start(update, &keys, &keys, "1.0.0").await;

    let outcome = engine.check().await.unwrap();
    assert!(outcome.available);
    assert_eq!(outcome.version.as_deref(), Some("2.0.0"));
    assert_eq!(outcome.current_version, "1.0.0");
    assert_eq!(outcome.files_changed, 1);
    assert_eq!(outcome.files_staged, 1);
    assert_eq!(outcome.download_bytes, 3);
    assert_eq!(outcome.total_bytes, 3);

    // prepare must consume the verified snapshot, not hit the network again.
    drop(server);
    let error = engine.prepare(Box::new(|_| {})).await.unwrap_err();
    assert!(error.to_string().contains("2.0.0"));
}

#[tokio::test]
async fn check_exposes_release_notes_and_mandatory() {
    let keys = TestKeyPair::generate();
    let mut value: Value = serde_json::from_slice(&manifest(
        "2.0.0",
        Some("stable"),
        false,
        &sha256_bytes(b"new"),
        3,
    ))
    .unwrap();
    value["notes"] = json!("修复若干问题\n新增功能");
    value["mandatory"] = json!(true);
    let update = serde_json::to_vec(&value).unwrap();
    let (_dir, engine, _server) = start(update, &keys, &keys, "1.0.0").await;

    let outcome = engine.check().await.unwrap();
    assert_eq!(outcome.notes.as_deref(), Some("修复若干问题\n新增功能"));
    assert!(outcome.mandatory);
}

#[tokio::test]
async fn oversized_release_notes_are_rejected() {
    let keys = TestKeyPair::generate();
    let mut value: Value = serde_json::from_slice(&manifest(
        "2.0.0",
        Some("stable"),
        false,
        &sha256_bytes(b"new"),
        3,
    ))
    .unwrap();
    value["notes"] = json!("x".repeat(64 * 1024 + 1));
    let update = serde_json::to_vec(&value).unwrap();
    let (_dir, engine, _server) = start(update, &keys, &keys, "1.0.0").await;

    let error = engine.check().await.unwrap_err();
    assert_eq!(error.code(), "E_MANIFEST_PARSE");
}

#[tokio::test]
async fn check_rejects_signature_from_untrusted_key() {
    let signer = TestKeyPair::generate();
    let trusted = TestKeyPair::generate();
    let update = manifest("2.0.0", Some("stable"), false, &sha256_bytes(b"new"), 3);
    let (_dir, engine, _server) = start(update, &signer, &trusted, "1.0.0").await;

    let error = engine.check().await.unwrap_err();
    assert_eq!(error.code(), "E_SIGNATURE_NO_TRUSTED_KEY");
}

#[tokio::test]
async fn check_rejects_rollback_without_flag() {
    let keys = TestKeyPair::generate();
    let downgrade = manifest("1.9.0", Some("stable"), false, &sha256_bytes(b"new"), 3);
    let (_dir, engine, _server) = start(downgrade, &keys, &keys, "2.0.0").await;

    let error = engine.check().await.unwrap_err();
    assert_eq!(error.code(), "E_VERSION_ROLLBACK");
}

#[tokio::test]
async fn check_accepts_authorized_rollback() {
    let keys = TestKeyPair::generate();
    let downgrade = manifest("1.9.0", Some("stable"), true, &sha256_bytes(b"new"), 3);
    let (_dir, engine, _server) = start(downgrade, &keys, &keys, "2.0.0").await;

    let outcome = engine.check().await.unwrap();
    assert!(outcome.available);
    assert_eq!(outcome.version.as_deref(), Some("1.9.0"));
}

#[tokio::test]
async fn check_treats_equal_version_as_no_update() {
    let keys = TestKeyPair::generate();
    let same = manifest("2.0.0", Some("stable"), false, &sha256_bytes(b"new"), 3);
    let (_dir, engine, _server) = start(same, &keys, &keys, "2.0.0").await;

    let outcome = engine.check().await.unwrap();
    assert!(!outcome.available);
    assert_eq!(outcome.version, None);
    assert_eq!(outcome.files_changed, 0);
}

#[tokio::test]
async fn check_ignores_other_channels() {
    let keys = TestKeyPair::generate();
    let beta = manifest("2.0.0", Some("beta"), false, &sha256_bytes(b"new"), 3);
    let (_dir, engine, _server) = start(beta, &keys, &keys, "1.0.0").await;

    let outcome = engine.check().await.unwrap();
    assert!(!outcome.available);
    assert_eq!(outcome.version, None);
}

#[tokio::test]
async fn check_rejects_too_new_schema() {
    let keys = TestKeyPair::generate();
    let mut value: Value = serde_json::from_slice(&manifest(
        "2.0.0",
        Some("stable"),
        false,
        &sha256_bytes(b"new"),
        3,
    ))
    .unwrap();
    value["schema"] = json!(2);
    let (_dir, engine, _server) =
        start(serde_json::to_vec(&value).unwrap(), &keys, &keys, "1.0.0").await;

    let error = engine.check().await.unwrap_err();
    assert_eq!(error.code(), "E_MANIFEST_SCHEMA_TOO_NEW");
}

#[tokio::test]
async fn check_retries_server_errors() {
    let keys = TestKeyPair::generate();
    let update = manifest("2.0.0", Some("stable"), false, &sha256_bytes(b"new"), 3);
    let (_dir, engine, server) = start(update, &keys, &keys, "1.0.0").await;
    server.fail_next("manifests/latest.json", 2);

    let outcome = engine.check().await.unwrap();
    assert!(outcome.available);
}

#[tokio::test]
async fn check_does_not_retry_client_errors() {
    let keys = TestKeyPair::generate();
    let update = manifest("2.0.0", Some("stable"), false, &sha256_bytes(b"new"), 3);
    let signature = keys.sign(&update);
    let server = MockServer::start([
        ("manifests/latest.json", update),
        ("manifests/latest.json.minisig", signature.into_bytes()),
    ])
    .await
    .unwrap();
    server.forbid_next("manifests/latest.json", 1);

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let endpoint = Url::parse(&server.url("manifests/latest.json")).unwrap();
    let fetcher = HttpFetcher::new(
        reqwest::Client::new(),
        endpoint.clone(),
        None,
        FetchLimits::default(),
        None,
        true,
    )
    .unwrap();
    let engine = Engine::new(EngineConfig {
        endpoint,
        signing_public_keys: vec![keys.public_key().to_string()],
        channel: "stable".into(),
        current_version: "1.0.0".into(),
        install_root: root,
        staging_root: dir.path().join("staging"),
        journal_root: dir.path().join("journal"),
        limits: Limits::default(),
        fetcher: Arc::new(fetcher),
        allow_insecure_http: true,
        crash_loop_threshold: 2,
    })
    .unwrap();

    let error = engine.check().await.unwrap_err();
    assert_eq!(error.code(), "E_NETWORK_HTTP_4XX");
}
