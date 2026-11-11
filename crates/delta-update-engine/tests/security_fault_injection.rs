mod common;

use std::sync::Arc;

use delta_update_engine::crypto::sha256_bytes;
use delta_update_engine::fetch::{FetchLimits, HttpFetcher};
use delta_update_engine::journal::{FileOp, Journal};
use delta_update_engine::patch::container::Codec;
use delta_update_engine::patch::{apply_patch, PatchHeader};
use delta_update_engine::{Engine, EngineConfig, Limits, StartupOutcome};
use delta_update_testkit::{MockServer, TestKeyPair};
use serde_json::{json, Value};
use url::Url;

use common::{fixture, NEW_APP, VERSION};

fn manifest(version: &str, content: &[u8]) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schema": 1,
        "version": version,
        "channel": "stable",
        "min_client_schema": 1,
        "files": {
            "App.exe": {
                "sha256": sha256_bytes(content),
                "size": content.len(),
                "strategy": "full",
                "full_key": format!("artifacts/full/{version}/App.exe")
            }
        }
    }))
    .unwrap()
}

async fn start(
    manifest: Vec<u8>,
    signature: Option<Vec<u8>>,
    trusted: Vec<String>,
    manifest_max: u64,
) -> (tempfile::TempDir, Engine, MockServer) {
    let mut files = vec![("manifests/latest.json".to_string(), manifest)];
    if let Some(signature) = signature {
        files.push(("manifests/latest.json.minisig".to_string(), signature));
    }
    let server = MockServer::start(files).await.unwrap();

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let endpoint = Url::parse(&server.url("manifests/latest.json")).unwrap();
    let fetcher = HttpFetcher::new(
        reqwest::Client::new(),
        endpoint.clone(),
        None,
        FetchLimits {
            manifest_max_bytes: manifest_max,
            ..FetchLimits::default()
        },
        None,
        true,
    )
    .unwrap();
    let mut limits = Limits::default();
    limits.manifest_max_bytes = manifest_max;
    let engine = Engine::new(EngineConfig {
        endpoint,
        signing_public_keys: trusted,
        channel: "stable".into(),
        current_version: "1.0.0".into(),
        install_root: root,
        staging_root: dir.path().join("staging"),
        journal_root: dir.path().join("journal"),
        limits,
        fetcher: Arc::new(fetcher),
        allow_insecure_http: true,
        crash_loop_threshold: 2,
    })
    .unwrap();
    (dir, engine, server)
}

/// SEC-01..03: changing any signed manifest field invalidates the signature.
#[tokio::test]
async fn tampered_manifest_fields_are_rejected() {
    let keys = TestKeyPair::generate();
    let original = manifest("2.0.0", b"new");
    let signature = keys.sign(&original).into_bytes();

    for field in 0..2 {
        let mut value: Value = serde_json::from_slice(&original).unwrap();
        match field {
            0 => value["version"] = json!("2.0.1"),
            _ => value["files"]["App.exe"]["sha256"] = json!("0".repeat(64)),
        }
        let tampered = serde_json::to_vec(&value).unwrap();
        let (_dir, engine, _server) = start(
            tampered,
            Some(signature.clone()),
            vec![keys.public_key().to_string()],
            8 * 1024 * 1024,
        )
        .await;

        let error = engine.check().await.unwrap_err();
        assert_eq!(error.code(), "E_SIGNATURE_INVALID");
    }
}

/// SEC-08: a missing signature object has its own error code.
#[tokio::test]
async fn missing_signature_object_is_reported() {
    let keys = TestKeyPair::generate();
    let (_dir, engine, _server) = start(
        manifest("2.0.0", b"new"),
        None,
        vec![keys.public_key().to_string()],
        8 * 1024 * 1024,
    )
    .await;

    let error = engine.check().await.unwrap_err();
    assert_eq!(error.code(), "E_SIGNATURE_SIG_MISSING");
}

/// SEC-16: any trusted key in the set can verify the manifest.
#[tokio::test]
async fn any_trusted_key_can_verify() {
    let keys = TestKeyPair::generate();
    let other = TestKeyPair::generate();
    let original = manifest("2.0.0", b"new");
    let signature = keys.sign(&original).into_bytes();

    let (_dir, engine, _server) = start(
        original,
        Some(signature),
        vec![
            other.public_key().to_string(),
            keys.public_key().to_string(),
        ],
        8 * 1024 * 1024,
    )
    .await;

    assert!(engine.check().await.unwrap().available);
}

/// SEC-10: an unknown `key_rotation` field is ignored and cannot widen the
/// trust set.
#[tokio::test]
async fn key_rotation_field_does_not_change_trust() {
    let keys = TestKeyPair::generate();
    let mut value: Value = serde_json::from_slice(&manifest("2.0.0", b"new")).unwrap();
    value["key_rotation"] =
        json!({ "next": "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3" });
    let body = serde_json::to_vec(&value).unwrap();
    let signature = keys.sign(&body).into_bytes();

    let (_dir, engine, _server) = start(
        body,
        Some(signature),
        vec![keys.public_key().to_string()],
        8 * 1024 * 1024,
    )
    .await;

    assert!(engine.check().await.unwrap().available);
}

/// SEC-18: an oversized manifest is rejected before parsing.
#[tokio::test]
async fn oversized_manifest_is_rejected() {
    let keys = TestKeyPair::generate();
    let original = manifest("2.0.0", b"new");
    let signature = keys.sign(&original).into_bytes();

    let (_dir, engine, _server) = start(
        original,
        Some(signature),
        vec![keys.public_key().to_string()],
        64,
    )
    .await;

    let error = engine.check().await.unwrap_err();
    assert_eq!(error.code(), "E_MANIFEST_TOO_LARGE");
}

/// SEC-14: a forged journal entry escaping the install root is rejected.
#[test]
fn journal_path_escape_is_rejected() {
    let fixture = fixture();
    let journal = Journal::new(
        "00000000-0000-4000-8000-000000000000",
        VERSION,
        "1.0.0",
        vec![FileOp::Place {
            staging_src: "files/App.exe".into(),
            dst: "../evil.dll".into(),
            sha256: sha256_bytes(NEW_APP),
            size: NEW_APP.len() as u64,
        }],
    );
    journal
        .write_atomic(&fixture.journal().join("commit.journal"))
        .unwrap();

    match fixture.engine.recover_and_cleanup().unwrap() {
        StartupOutcome::NeedsAttention { error } => {
            assert_eq!(error.code(), "E_JOURNAL_REPLAY_FAILED")
        }
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert!(!fixture.dir.path().join("evil.dll").exists());
}

/// SEC-17: a staged source modified after the journal was written fails the
/// replay hash check instead of landing unverified bytes.
#[test]
fn tampered_staging_source_fails_replay() {
    let fixture = fixture();
    let pending = fixture.engine.begin_commit(&fixture.prepared).unwrap();
    std::fs::write(fixture.staging().join("files/App.exe"), b"tampered").unwrap();

    match fixture.engine.recover_and_cleanup().unwrap() {
        StartupOutcome::NeedsAttention { error } => {
            assert_eq!(error.code(), "E_JOURNAL_REPLAY_FAILED")
        }
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert!(!fixture.root().join("App.exe").exists());
    assert!(fixture
        .root()
        .join(".old")
        .join(&pending.commit_id)
        .join("App.exe")
        .exists());
}

/// SEC-19: decompressing a patch beyond the budget aborts immediately.
#[test]
fn decompression_limit_is_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("old");
    std::fs::write(&old, b"hello world").unwrap();
    let patch_path = dir.path().join("payload.tdp");

    let header = PatchHeader {
        format: 1,
        strategy_id: 1,
        codec: Codec::Zstd,
        target_size: 32,
        source_sha256: hex32(b"hello world"),
        target_sha256: [0; 32],
    };
    let payload = zstd::encode_all(&vec![0_u8; 1024][..], 3).unwrap();
    let mut patch = header.encode().to_vec();
    patch.extend_from_slice(&payload);
    std::fs::write(&patch_path, patch).unwrap();

    let mut limits = Limits::default();
    limits.max_patch_file_size = 64;
    let error =
        apply_patch(&old, &patch_path, &dir.path().join("new"), &header, &limits).unwrap_err();
    assert_eq!(error.code(), "E_PATCH_DECOMPRESSION_LIMIT");
}

fn hex32(bytes: &[u8]) -> [u8; 32] {
    hex::decode(sha256_bytes(bytes))
        .unwrap()
        .try_into()
        .unwrap()
}
