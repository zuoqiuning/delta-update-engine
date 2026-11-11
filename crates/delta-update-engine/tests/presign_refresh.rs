use std::sync::Arc;

use delta_update_engine::crypto::sha256_bytes;
use delta_update_engine::fetch::{FetchLimits, HttpFetcher};
use delta_update_engine::{Engine, EngineConfig, Limits};
use delta_update_testkit::{MockServer, TestKeyPair};
use serde_json::json;
use url::Url;

const OBJECT_KEY: &str = "artifacts/full/2.0.0/App.exe";

struct Fixture {
    _dir: tempfile::TempDir,
    engine: Engine,
    server: MockServer,
}

async fn fixture() -> Fixture {
    let keys = TestKeyPair::generate();
    let content = b"new";
    let manifest = serde_json::to_vec(&json!({
        "schema": 1,
        "version": "2.0.0",
        "channel": "stable",
        "min_client_schema": 1,
        "files": {
            "App.exe": {
                "sha256": sha256_bytes(content),
                "size": content.len(),
                "strategy": "full",
                "full_key": OBJECT_KEY
            }
        }
    }))
    .unwrap();
    let signature = keys.sign(&manifest);
    let server = MockServer::start([
        ("manifests/latest.json".to_string(), manifest),
        (
            "manifests/latest.json.minisig".to_string(),
            signature.into_bytes(),
        ),
        (OBJECT_KEY.to_string(), content.to_vec()),
    ])
    .await
    .unwrap();
    server.enable_signing();

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("App.exe"), b"old").unwrap();

    // The manifest endpoint carries a query, so the client must use the
    // signing service for every key.
    let endpoint = Url::parse(&format!(
        "{}?initial=1",
        server.url("manifests/latest.json")
    ))
    .unwrap();
    let signing_endpoint = Url::parse(&server.url("sign")).unwrap();
    let fetcher = HttpFetcher::new(
        reqwest::Client::new(),
        endpoint.clone(),
        Some(signing_endpoint),
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
    Fixture {
        _dir: dir,
        engine,
        server,
    }
}

fn signing_calls(server: &MockServer) -> usize {
    server
        .requests()
        .iter()
        .filter(|path| *path == "sign")
        .count()
}

/// F-02: an expired presigned URL is refreshed and the request is retried.
#[tokio::test]
async fn refreshes_expired_signed_url() {
    let fixture = fixture().await;
    fixture.server.expire_presigned_next(1);

    fixture.engine.check().await.unwrap();
    let prepared = fixture.engine.prepare(Box::new(|_| {})).await.unwrap();

    assert_eq!(
        std::fs::read(prepared.staging_root.join("files/App.exe")).unwrap(),
        b"new"
    );
    assert!(
        signing_calls(&fixture.server) >= 4,
        "expected a refresh for the expired URL"
    );
}

/// F-03: refresh attempts are bounded and surface `E_NETWORK_PRESIGN_EXPIRED`.
#[tokio::test]
async fn gives_up_after_repeated_expired_urls() {
    let fixture = fixture().await;
    fixture.server.expire_presigned_next(u32::MAX);

    let error = fixture.engine.check().await.unwrap_err();
    assert_eq!(error.code(), "E_NETWORK_PRESIGN_EXPIRED");
    assert!(
        signing_calls(&fixture.server) >= 4,
        "expected the bounded refresh loop to run"
    );
}
