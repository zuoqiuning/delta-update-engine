use std::sync::{Arc, Mutex};

use delta_update_engine::crypto::sha256_bytes;
use delta_update_engine::fetch::{FetchLimits, HttpFetcher};
use delta_update_engine::patch::container::{Codec, PatchHeader};
use delta_update_engine::staging::PreparedPlan;
use delta_update_engine::{Engine, EngineConfig, Limits, Progress, StartupOutcome};
use delta_update_testkit::{MockServer, TestKeyPair};
use serde_json::{json, Value};
use url::Url;

fn full_entry(version: &str, path: &str, content: &[u8]) -> Value {
    json!({
        "sha256": sha256_bytes(content),
        "size": content.len(),
        "strategy": "full",
        "full_key": format!("artifacts/full/{version}/{path}")
    })
}

async fn start(
    manifest: Value,
    keys: &TestKeyPair,
    current_version: &str,
    limits: Limits,
    local: &[(&str, &[u8])],
    known: &[&str],
    objects: &[(&str, &[u8])],
) -> (tempfile::TempDir, Engine, MockServer) {
    let manifest = serde_json::to_vec(&manifest).unwrap();
    let signature = keys.sign(&manifest);
    let mut files: Vec<(String, Vec<u8>)> = vec![
        ("manifests/latest.json".into(), manifest),
        (
            "manifests/latest.json.minisig".into(),
            signature.into_bytes(),
        ),
    ];
    files.extend(
        objects
            .iter()
            .map(|(key, bytes)| ((*key).to_string(), bytes.to_vec())),
    );
    let server = MockServer::start(files).await.unwrap();

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    for (path, bytes) in local {
        let full = root.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, bytes).unwrap();
    }
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    std::fs::write(
        journal.join("known_files.json"),
        serde_json::to_vec(&json!({ "files": known })).unwrap(),
    )
    .unwrap();

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
        current_version: current_version.into(),
        install_root: root,
        staging_root: dir.path().join("staging"),
        journal_root: journal,
        limits,
        fetcher: Arc::new(fetcher),
        allow_insecure_http: true,
        crash_loop_threshold: 2,
    })
    .unwrap();

    (dir, engine, server)
}

fn manifest_with(files: Value) -> Value {
    json!({
        "schema": 1,
        "version": "2.0.0",
        "channel": "stable",
        "min_client_schema": 1,
        "files": files
    })
}

#[tokio::test]
async fn prepare_materializes_only_changed_files_and_persists_plan() {
    let keys = TestKeyPair::generate();
    let new_app = b"new";
    let icon = b"icon";
    let keep = b"keep";
    let manifest = manifest_with(json!({
        "App.exe": full_entry("2.0.0", "App.exe", new_app),
        "assets/icon.bin": full_entry("2.0.0", "assets/icon.bin", icon),
        "keep.bin": full_entry("2.0.0", "keep.bin", keep)
    }));
    let (dir, engine, _server) = start(
        manifest,
        &keys,
        "1.0.0",
        Limits::default(),
        &[
            ("App.exe", b"old"),
            ("keep.bin", keep),
            ("old.txt", b"gone"),
        ],
        &["old.txt"],
        &[
            ("artifacts/full/2.0.0/App.exe", new_app),
            ("artifacts/full/2.0.0/assets/icon.bin", icon),
        ],
    )
    .await;

    let outcome = engine.check().await.unwrap();
    assert!(outcome.available);
    assert_eq!(outcome.files_changed, 3);

    let progress = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&progress);
    let prepared = engine
        .prepare(Box::new(move |event| {
            sink.lock().unwrap().push(event);
        }))
        .await
        .unwrap();
    assert_eq!(prepared.version, "2.0.0");
    assert_eq!(
        prepared.staging_root,
        dir.path().join("staging").join("2.0.0")
    );

    let staged_app = prepared.staging_root.join("files/App.exe");
    let staged_icon = prepared.staging_root.join("files/assets/icon.bin");
    assert_eq!(std::fs::read(&staged_app).unwrap(), new_app);
    assert_eq!(std::fs::read(&staged_icon).unwrap(), icon);
    assert!(!prepared.staging_root.join("files/keep.bin").exists());

    let plan = PreparedPlan::load(&dir.path().join("journal/prepared.json")).unwrap();
    assert_eq!(plan.removed, vec!["old.txt"]);
    assert_eq!(plan.all_files.len(), 3);
    assert!(plan.all_files.contains(&"keep.bin".to_string()));

    let events = progress.lock().unwrap();
    let downloads: Vec<(u64, u64)> = events
        .iter()
        .filter_map(|event| match event {
            Progress::Downloading {
                total, completed, ..
            } => Some((*total, *completed)),
            _ => None,
        })
        .collect();
    assert!(downloads.windows(2).all(|pair| pair[0].1 <= pair[1].1));
    assert_eq!(
        downloads.last().copied(),
        Some((outcome.download_bytes, outcome.download_bytes))
    );
    assert!(events
        .iter()
        .any(|event| matches!(event, Progress::Verifying { .. })));
    assert!(events.iter().any(|event| matches!(event, Progress::Done)));
}

#[tokio::test]
async fn prepare_applies_qbsdiff_patch() {
    let keys = TestKeyPair::generate();
    let old = b"hello world";
    let new = b"hello brave new world";

    let mut bsdiff = Vec::new();
    qbsdiff::Bsdiff::new(old, new).compare(&mut bsdiff).unwrap();
    let payload = zstd::encode_all(bsdiff.as_slice(), 3).unwrap();
    let header = PatchHeader {
        format: 1,
        strategy_id: 1,
        codec: Codec::Zstd,
        target_size: new.len() as u64,
        source_sha256: hex_to_32(&sha256_bytes(old)),
        target_sha256: hex_to_32(&sha256_bytes(new)),
    };
    let mut patch = header.encode().to_vec();
    patch.extend_from_slice(&payload);

    let manifest = manifest_with(json!({
        "App.exe": {
            "sha256": sha256_bytes(new),
            "size": new.len(),
            "strategy": "qbsdiff+zstd",
            "delta_key": "artifacts/1.0.0-to-2.0.0/App.exe.tdp",
            "delta_size": patch.len(),
            "full_key": "artifacts/full/2.0.0/App.exe"
        }
    }));
    let (dir, engine, _server) = start(
        manifest,
        &keys,
        "1.0.0",
        Limits::default(),
        &[("App.exe", old)],
        &[],
        &[("artifacts/1.0.0-to-2.0.0/App.exe.tdp", &patch)],
    )
    .await;

    engine.check().await.unwrap();
    let progress = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&progress);
    let prepared = engine
        .prepare(Box::new(move |event| {
            sink.lock().unwrap().push(format!("{event:?}"));
        }))
        .await
        .unwrap();

    assert_eq!(
        std::fs::read(prepared.staging_root.join("files/App.exe")).unwrap(),
        new
    );
    assert!(progress.lock().unwrap().join(" ").contains("Applying"));
    drop(dir);
}

#[tokio::test]
async fn prepare_falls_back_to_full_when_patch_is_corrupt() {
    let keys = TestKeyPair::generate();
    let old = b"hello world";
    let new = b"hello brave new world";
    let corrupt = vec![0_u8; 100];

    let manifest = manifest_with(json!({
        "App.exe": {
            "sha256": sha256_bytes(new),
            "size": new.len(),
            "strategy": "qbsdiff+zstd",
            "delta_key": "artifacts/bad.tdp",
            "delta_size": corrupt.len(),
            "full_key": "artifacts/full/2.0.0/App.exe"
        }
    }));
    let (dir, engine, _server) = start(
        manifest,
        &keys,
        "1.0.0",
        Limits::default(),
        &[("App.exe", old)],
        &[],
        &[
            ("artifacts/bad.tdp", &corrupt),
            ("artifacts/full/2.0.0/App.exe", new),
        ],
    )
    .await;

    engine.check().await.unwrap();
    let prepared = engine.prepare(Box::new(|_| {})).await.unwrap();
    assert_eq!(
        std::fs::read(prepared.staging_root.join("files/App.exe")).unwrap(),
        new
    );
    drop(dir);
}

#[tokio::test]
async fn prepare_rejects_staging_over_limit() {
    let keys = TestKeyPair::generate();
    let new_app = b"new";
    let manifest = manifest_with(json!({
        "App.exe": full_entry("2.0.0", "App.exe", new_app)
    }));
    let mut limits = Limits::default();
    limits.staging_size_limit = 1;
    let (_dir, engine, _server) = start(
        manifest,
        &keys,
        "1.0.0",
        limits,
        &[("App.exe", b"old")],
        &[],
        &[("artifacts/full/2.0.0/App.exe", new_app)],
    )
    .await;

    engine.check().await.unwrap();
    let error = engine.prepare(Box::new(|_| {})).await.unwrap_err();
    assert_eq!(error.code(), "E_DISK_STAGING_LIMIT");
}

/// S-05: free space is checked before the first object download. A declared
/// size larger than any real test volume triggers the check deterministically;
/// the staging limit is lifted so this is not just the S-06 path.
#[tokio::test]
async fn prepare_rejects_insufficient_disk_space_before_download() {
    let keys = TestKeyPair::generate();
    let huge = 1_u64 << 50;
    let manifest = manifest_with(json!({
        "App.exe": {
            "sha256": sha256_bytes(b"new"),
            "size": huge,
            "strategy": "full",
            "full_key": "artifacts/full/2.0.0/App.exe"
        }
    }));
    let mut limits = Limits::default();
    limits.max_full_bytes = 1 << 60;
    limits.staging_size_limit = u64::MAX;
    let (_dir, engine, server) = start(
        manifest,
        &keys,
        "1.0.0",
        limits,
        &[("App.exe", b"old")],
        &[],
        &[],
    )
    .await;

    engine.check().await.unwrap();
    let before = server.requests();
    let error = engine.prepare(Box::new(|_| {})).await.unwrap_err();
    assert_eq!(error.code(), "E_DISK_INSUFFICIENT");
    assert_eq!(
        server.requests(),
        before,
        "prepare must fail before downloading"
    );
    assert!(
        !server
            .requests()
            .iter()
            .any(|path| path.starts_with("artifacts/")),
        "no object may be requested: {:?}",
        server.requests()
    );
}

#[tokio::test]
async fn prepare_detects_unchanged_file_changed_after_check() {
    let keys = TestKeyPair::generate();
    let keep = b"keep";
    let manifest = manifest_with(json!({
        "keep.bin": full_entry("2.0.0", "keep.bin", keep)
    }));
    let (dir, engine, _server) = start(
        manifest,
        &keys,
        "1.0.0",
        Limits::default(),
        &[("keep.bin", keep)],
        &[],
        &[],
    )
    .await;
    engine.check().await.unwrap();
    std::fs::write(dir.path().join("root/keep.bin"), b"changed").unwrap();

    let error = engine.prepare(Box::new(|_| {})).await.unwrap_err();
    assert_eq!(error.code(), "E_PATCH_SOURCE_MISMATCH");
}

/// S-04: when the patch is unusable and the full object also fails its hash
/// check, prepare fails instead of staging unverified bytes.
#[tokio::test]
async fn prepare_fails_when_patch_and_full_both_fail() {
    let keys = TestKeyPair::generate();
    let old = b"hello world";
    let new = b"hello brave new world";
    let corrupt = vec![0_u8; 100];
    let wrong_full = b"wrong object bytes";

    let manifest = manifest_with(json!({
        "App.exe": {
            "sha256": sha256_bytes(new),
            "size": new.len(),
            "strategy": "qbsdiff+zstd",
            "delta_key": "artifacts/bad.tdp",
            "delta_size": corrupt.len(),
            "full_key": "artifacts/full/2.0.0/App.exe"
        }
    }));
    let (_dir, engine, _server) = start(
        manifest,
        &keys,
        "1.0.0",
        Limits::default(),
        &[("App.exe", old)],
        &[],
        &[
            ("artifacts/bad.tdp", &corrupt),
            ("artifacts/full/2.0.0/App.exe", wrong_full),
        ],
    )
    .await;

    engine.check().await.unwrap();
    let error = engine.prepare(Box::new(|_| {})).await.unwrap_err();
    assert_eq!(error.code(), "E_PATCH_TARGET_HASH_MISMATCH");
}

/// S-07: a restart reuses the persisted PREPARED plan without downloading.
#[tokio::test]
async fn prepared_plan_is_reused_after_restart() {
    let keys = TestKeyPair::generate();
    let new_app = b"new";
    let manifest = manifest_with(json!({
        "App.exe": full_entry("2.0.0", "App.exe", new_app)
    }));
    let (_dir, engine, _server) = start(
        manifest,
        &keys,
        "1.0.0",
        Limits::default(),
        &[("App.exe", b"old")],
        &[],
        &[("artifacts/full/2.0.0/App.exe", new_app)],
    )
    .await;

    engine.check().await.unwrap();
    let prepared = engine.prepare(Box::new(|_| {})).await.unwrap();

    let restarted = Engine::new(engine.config().clone()).unwrap();
    match restarted.recover_and_cleanup().unwrap() {
        StartupOutcome::ReusedPrepared { version } => assert_eq!(version, "2.0.0"),
        other => panic!("unexpected outcome: {other:?}"),
    }
    let resumed = restarted.resume_prepared().unwrap();
    assert_eq!(resumed.staging_root, prepared.staging_root);
    assert_eq!(resumed.manifest_hash, prepared.manifest_hash);
    assert_eq!(resumed.version, "2.0.0");
}

/// S-08: a staged file modified after prepare is rejected before the commit
/// point, and no journal is written.
#[tokio::test]
async fn tampered_staging_file_blocks_commit() {
    let keys = TestKeyPair::generate();
    let new_app = b"new";
    let manifest = manifest_with(json!({
        "App.exe": full_entry("2.0.0", "App.exe", new_app)
    }));
    let (_dir, engine, _server) = start(
        manifest,
        &keys,
        "1.0.0",
        Limits::default(),
        &[("App.exe", b"old")],
        &[],
        &[("artifacts/full/2.0.0/App.exe", new_app)],
    )
    .await;

    engine.check().await.unwrap();
    let prepared = engine.prepare(Box::new(|_| {})).await.unwrap();
    std::fs::write(prepared.staging_root.join("files/App.exe"), b"tampered").unwrap();

    let error = engine.begin_commit(&prepared).unwrap_err();
    assert_eq!(error.code(), "E_PATCH_TARGET_HASH_MISMATCH");
    assert!(!engine.config().journal_root.join("commit.journal").exists());
}

/// F-05: a patch whose container header matches the manifest but whose
/// payload produces different bytes is discarded and the full object is used.
#[tokio::test]
async fn patch_with_wrong_payload_falls_back_to_full() {
    let keys = TestKeyPair::generate();
    let old = b"hello world";
    let new = b"hello brave new world";
    let wrong = b"hello wrong new world";
    assert_eq!(new.len(), wrong.len());

    let mut bsdiff = Vec::new();
    qbsdiff::Bsdiff::new(old, wrong)
        .compare(&mut bsdiff)
        .unwrap();
    let payload = zstd::encode_all(bsdiff.as_slice(), 3).unwrap();
    let header = PatchHeader {
        format: 1,
        strategy_id: 1,
        codec: Codec::Zstd,
        target_size: new.len() as u64,
        source_sha256: hex_to_32(&sha256_bytes(old)),
        target_sha256: hex_to_32(&sha256_bytes(new)),
    };
    let mut patch = header.encode().to_vec();
    patch.extend_from_slice(&payload);

    let manifest = manifest_with(json!({
        "App.exe": {
            "sha256": sha256_bytes(new),
            "size": new.len(),
            "strategy": "qbsdiff+zstd",
            "delta_key": "artifacts/wrong.tdp",
            "delta_size": patch.len(),
            "full_key": "artifacts/full/2.0.0/App.exe"
        }
    }));
    let (_dir, engine, server) = start(
        manifest,
        &keys,
        "1.0.0",
        Limits::default(),
        &[("App.exe", old)],
        &[],
        &[
            ("artifacts/wrong.tdp", &patch),
            ("artifacts/full/2.0.0/App.exe", new),
        ],
    )
    .await;

    engine.check().await.unwrap();
    let prepared = engine.prepare(Box::new(|_| {})).await.unwrap();
    assert_eq!(
        std::fs::read(prepared.staging_root.join("files/App.exe")).unwrap(),
        new
    );
    assert!(
        server
            .requests()
            .iter()
            .any(|key| key == "artifacts/full/2.0.0/App.exe"),
        "the full object should be used after the payload mismatch"
    );
}

/// Staging and PREPARED persistence share the cross-process update lock, so a
/// second instance cannot interleave writes with an in-flight prepare.
#[tokio::test]
async fn prepare_rejects_a_concurrent_update_lock() {
    use delta_update_engine::journal::UpdateLock;

    let keys = TestKeyPair::generate();
    let new = b"new";
    let manifest = manifest_with(json!({
        "App.exe": full_entry("2.0.0", "App.exe", new),
    }));
    let (dir, engine, _server) = start(
        manifest,
        &keys,
        "1.0.0",
        Limits::default(),
        &[("App.exe", b"old")],
        &[],
        &[("artifacts/full/2.0.0/App.exe", new)],
    )
    .await;
    engine.check().await.unwrap();

    let guard = UpdateLock::acquire(&dir.path().join("journal/update.lock")).unwrap();
    let error = engine.prepare(Box::new(|_| {})).await.unwrap_err();
    assert!(
        matches!(error, delta_update_engine::EngineError::UpdateLocked),
        "unexpected error: {error}"
    );
    drop(guard);
}

fn hex_to_32(value: &str) -> [u8; 32] {
    hex::decode(value).unwrap().try_into().unwrap()
}
