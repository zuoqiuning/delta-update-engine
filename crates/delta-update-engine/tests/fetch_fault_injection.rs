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

async fn fixture(content: &[u8]) -> Fixture {
    fixture_with(&[("App.exe", content)], 4).await
}

async fn fixture_with(files: &[(&str, &[u8])], max_concurrent: usize) -> Fixture {
    fixture_with_client(files, max_concurrent, reqwest::Client::new(), Vec::new()).await
}

async fn fixture_with_client(
    files: &[(&str, &[u8])],
    max_concurrent: usize,
    client: reqwest::Client,
    headers: Vec<(String, String)>,
) -> Fixture {
    let keys = TestKeyPair::generate();
    let mut manifest_files = serde_json::Map::new();
    let mut objects: Vec<(String, Vec<u8>)> = Vec::new();
    for (path, content) in files {
        let full_key = format!("artifacts/full/2.0.0/{path}");
        manifest_files.insert(
            (*path).to_string(),
            json!({
                "sha256": sha256_bytes(content),
                "size": content.len(),
                "strategy": "full",
                "full_key": full_key
            }),
        );
        objects.push((full_key, content.to_vec()));
    }
    let manifest = serde_json::to_vec(&json!({
        "schema": 1,
        "version": "2.0.0",
        "channel": "stable",
        "min_client_schema": 1,
        "files": manifest_files
    }))
    .unwrap();
    let signature = keys.sign(&manifest);
    let mut served = vec![
        ("manifests/latest.json".to_string(), manifest),
        (
            "manifests/latest.json.minisig".to_string(),
            signature.into_bytes(),
        ),
    ];
    served.extend(objects);
    let server = MockServer::start(served).await.unwrap();

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    for (path, _) in files {
        let full = root.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, b"old").unwrap();
    }
    let endpoint = Url::parse(&server.url("manifests/latest.json")).unwrap();
    let fetcher = if headers.is_empty() {
        HttpFetcher::new(
            client,
            endpoint.clone(),
            None,
            FetchLimits::default(),
            None,
            true,
        )
        .unwrap()
    } else {
        HttpFetcher::new_with_options(
            endpoint.clone(),
            None,
            FetchLimits::default(),
            None,
            true,
            None,
            headers,
        )
        .unwrap()
    };
    let mut limits = Limits::default();
    limits.max_concurrent_downloads = max_concurrent;
    let engine = Engine::new(EngineConfig {
        endpoint,
        signing_public_keys: vec![keys.public_key().to_string()],
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
    Fixture {
        _dir: dir,
        engine,
        server,
    }
}

/// F-01: a connection that drops mid-body resumes from the received offset.
#[tokio::test]
async fn resumes_after_mid_body_disconnect() {
    let content = vec![7_u8; 64 * 1024];
    let fixture = fixture(&content).await;
    fixture.server.cut_next(OBJECT_KEY, 20 * 1024);

    fixture.engine.check().await.unwrap();
    let prepared = fixture.engine.prepare(Box::new(|_| {})).await.unwrap();

    let staged = std::fs::read(prepared.staging_root.join("files/App.exe")).unwrap();
    assert_eq!(staged, content);
    let ranges = fixture.server.ranges();
    assert!(
        ranges
            .iter()
            .any(|(path, value)| path == OBJECT_KEY && value == "bytes=20480-"),
        "expected a resumed Range request, got {ranges:?}"
    );
}

/// F-04: when the server ignores Range, the object is downloaded again from
/// zero and still lands complete.
#[tokio::test]
async fn range_unsupported_falls_back_to_full_download() {
    let content = vec![9_u8; 48 * 1024];
    let fixture = fixture(&content).await;
    fixture.server.set_range_supported(false);
    fixture.server.cut_next(OBJECT_KEY, 16 * 1024);

    fixture.engine.check().await.unwrap();
    let prepared = fixture.engine.prepare(Box::new(|_| {})).await.unwrap();

    let staged = std::fs::read(prepared.staging_root.join("files/App.exe")).unwrap();
    assert_eq!(staged, content);
    assert!(
        fixture
            .server
            .ranges()
            .iter()
            .any(|(path, _)| path == OBJECT_KEY),
        "the retry should still send a Range request"
    );
}

/// F-06: retries are bounded; repeated 5xx responses surface the network error.
#[tokio::test]
async fn retries_are_bounded_on_server_errors() {
    let content = vec![3_u8; 1024];
    let fixture = fixture(&content).await;
    fixture.server.fail_next(OBJECT_KEY, 10);

    fixture.engine.check().await.unwrap();
    let error = fixture.engine.prepare(Box::new(|_| {})).await.unwrap_err();

    assert_eq!(error.code(), "E_NETWORK_HTTP_5XX");
    let attempts = fixture
        .server
        .requests()
        .iter()
        .filter(|path| *path == OBJECT_KEY)
        .count();
    assert_eq!(attempts, 3, "expected the initial attempt plus 2 retries");
}

/// F-09: prepare downloads concurrently but never above the configured limit.
#[tokio::test]
async fn concurrent_downloads_are_bounded() {
    let entries: Vec<(String, Vec<u8>)> = (0..10)
        .map(|index| (format!("file{index}.bin"), vec![index as u8; 4096]))
        .collect();
    let files: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(path, content)| (path.as_str(), content.as_slice()))
        .collect();
    let fixture = fixture_with(&files, 3).await;

    fixture.engine.check().await.unwrap();
    fixture.server.reset_max_concurrent();
    fixture.server.set_delay(100);
    let prepared = fixture.engine.prepare(Box::new(|_| {})).await.unwrap();

    let peak = fixture.server.max_concurrent();
    assert!(peak <= 3, "download limit was exceeded: {peak}");
    assert!(peak >= 2, "downloads did not overlap: {peak}");
    for (path, content) in &entries {
        let staged = std::fs::read(prepared.staging_root.join("files").join(path)).unwrap();
        assert_eq!(staged.as_slice(), content.as_slice(), "{path} mismatch");
    }
}

/// F-10: a response that never arrives within the read timeout is aborted and
/// retried a bounded number of times.
#[tokio::test]
async fn slow_responses_time_out() {
    let content = vec![5_u8; 1024];
    let client = reqwest::Client::builder()
        .read_timeout(std::time::Duration::from_millis(200))
        .build()
        .unwrap();
    let fixture = fixture_with_client(&[("App.exe", &content)], 4, client, Vec::new()).await;

    fixture.engine.check().await.unwrap();
    fixture.server.set_delay(1_000);
    let error = fixture.engine.prepare(Box::new(|_| {})).await.unwrap_err();

    assert_eq!(error.code(), "E_NETWORK_TIMEOUT");
    let attempts = fixture
        .server
        .requests()
        .iter()
        .filter(|path| *path == OBJECT_KEY)
        .count();
    assert_eq!(attempts, 3, "expected the initial attempt plus 2 retries");
}

/// P-12: cancelling prepare stops in-flight downloads but keeps staged files,
/// so the next attempt resumes without downloading them again.
#[tokio::test]
async fn cancelled_prepare_keeps_staged_files() {
    let entries: Vec<(String, Vec<u8>)> = (0..20)
        .map(|index| (format!("file{index}.bin"), vec![index as u8; 4096]))
        .collect();
    let files: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(path, content)| (path.as_str(), content.as_slice()))
        .collect();
    let Fixture {
        _dir,
        engine,
        server,
    } = fixture_with(&files, 1).await;

    engine.check().await.unwrap();
    server.set_delay(100);
    let engine = Arc::new(engine);
    let runner = Arc::clone(&engine);
    let task = tokio::spawn(async move { runner.prepare(Box::new(|_| {})).await });

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let completed = loop {
        let objects: Vec<String> = server
            .requests()
            .into_iter()
            .filter(|path| path.starts_with("artifacts/full/"))
            .collect();
        if objects.len() >= 2 {
            // Downloads are scheduled unordered: the first request is the one
            // that finished before the in-flight download was cancelled.
            break objects[0].clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "prepare did not start downloading"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };

    engine.cancel_prepare().unwrap();
    let error = task.await.unwrap().unwrap_err();
    assert_eq!(error.code(), "E_CANCELLED");

    server.set_delay(0);
    let prepared = engine.prepare(Box::new(|_| {})).await.unwrap();
    for (path, content) in &entries {
        let staged = std::fs::read(prepared.staging_root.join("files").join(path)).unwrap();
        assert_eq!(staged.as_slice(), content.as_slice(), "{path} mismatch");
    }
    assert_eq!(
        fixture_request_count(&server, &completed),
        1,
        "the completed file should not be downloaded again: {:?}",
        server.requests()
    );
}

/// Objects above the chunk threshold are fetched as parallel Range requests.
#[tokio::test]
async fn large_objects_use_parallel_ranges() {
    let content = vec![0x5a_u8; 10 * 1024 * 1024];
    let fixture = fixture_with(&[("App.exe", &content)], 2).await;

    fixture.engine.check().await.unwrap();
    let prepared = fixture.engine.prepare(Box::new(|_| {})).await.unwrap();
    assert_eq!(
        std::fs::read(prepared.staging_root.join("files/App.exe")).unwrap(),
        content
    );

    let ranges: Vec<_> = fixture
        .server
        .ranges()
        .into_iter()
        .filter(|(path, _)| path == OBJECT_KEY)
        .collect();
    assert!(
        ranges.len() >= 2,
        "expected parallel range requests, got {ranges:?}"
    );
    assert!(
        ranges.iter().all(|(_, range)| range.starts_with("bytes=")),
        "{ranges:?}"
    );
}

/// Custom headers authenticate manifest/signature requests and must never be
/// forwarded to third-party object hosts.
#[tokio::test]
async fn custom_headers_only_reach_manifest_and_signature() {
    let content = vec![7_u8; 1024];
    let fixture = fixture_with_client(
        &[("App.exe", &content)],
        2,
        reqwest::Client::new(),
        vec![("x-api-key".into(), "secret".into())],
    )
    .await;

    fixture.engine.check().await.unwrap();
    assert_eq!(
        fixture
            .server
            .last_header("manifests/latest.json", "x-api-key")
            .as_deref(),
        Some("secret")
    );
    assert_eq!(
        fixture
            .server
            .last_header("manifests/latest.json.minisig", "x-api-key")
            .as_deref(),
        Some("secret")
    );

    fixture.engine.prepare(Box::new(|_| {})).await.unwrap();
    assert_eq!(
        fixture.server.last_header(OBJECT_KEY, "x-api-key"),
        None,
        "object downloads must not receive credentials"
    );
}

fn fixture_request_count(server: &MockServer, key: &str) -> usize {
    server
        .requests()
        .iter()
        .filter(|request| *request == key)
        .count()
}

/// F-08: a 4xx response is not retried.
#[tokio::test]
async fn client_errors_are_not_retried() {
    let content = vec![1_u8; 1024];
    let fixture = fixture(&content).await;
    fixture.server.forbid_next(OBJECT_KEY, 10);

    fixture.engine.check().await.unwrap();
    let error = fixture.engine.prepare(Box::new(|_| {})).await.unwrap_err();

    assert_eq!(error.code(), "E_NETWORK_HTTP_4XX");
    let attempts = fixture
        .server
        .requests()
        .iter()
        .filter(|path| *path == OBJECT_KEY)
        .count();
    assert_eq!(attempts, 1);
}
