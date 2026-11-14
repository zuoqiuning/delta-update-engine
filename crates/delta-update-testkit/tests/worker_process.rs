//! Process-level worker tests.
//!
//! The real replacement binary (`worker_harness`) is copied into a temporary
//! install root and started with the same arguments the plugin uses. This
//! covers the cross-process parts that in-process tests cannot: scope
//! validation against the worker's own directory, apply.json handoff, signed
//! manifest re-verification and the commit itself.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use async_trait::async_trait;
use delta_update_engine::crypto::sha256_bytes;
use delta_update_engine::error::NetworkError;
use delta_update_engine::fetch::{DownloadReceipt, Fetcher};
use delta_update_engine::journal::state;
use delta_update_engine::{Engine, EngineConfig, EngineError, Limits};
use delta_update_testkit::TestKeyPair;
use serde_json::json;
use url::Url;

const ADDED: &[u8] = b"added by the update";

/// Parse `retries N` out of a worker log line. The exact value depends on
/// whether the CI runner's antivirus happened to touch the temp directory, so
/// tests assert the format here and leave exact counts to the fs retry tests.
fn retries_in_worker_log(text: &str) -> Option<u64> {
    let (_, rest) = text.split_once("retries ")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

struct MapFetcher {
    manifest: Vec<u8>,
    signature: Vec<u8>,
    objects: HashMap<String, Vec<u8>>,
}

#[async_trait]
impl Fetcher for MapFetcher {
    async fn fetch_manifest(&self) -> Result<Vec<u8>, EngineError> {
        Ok(self.manifest.clone())
    }

    async fn fetch_signature(&self) -> Result<Vec<u8>, EngineError> {
        Ok(self.signature.clone())
    }

    async fn download(
        &self,
        key: &str,
        dest: &Path,
        resume_from: u64,
        max_bytes: u64,
    ) -> Result<DownloadReceipt, EngineError> {
        let bytes = self.objects.get(key).ok_or_else(|| {
            EngineError::Network(NetworkError::Http4xx {
                status: 404,
                detail: key.to_string(),
            })
        })?;
        let start = (resume_from as usize).min(bytes.len());
        let end = if max_bytes == 0 {
            bytes.len()
        } else {
            start.saturating_add(max_bytes as usize).min(bytes.len())
        };
        if start == 0 {
            tokio::fs::write(dest, &bytes[start..end])
                .await
                .map_err(|err| EngineError::Internal(err.to_string()))?;
        } else {
            let mut merged = tokio::fs::read(dest)
                .await
                .map_err(|err| EngineError::Internal(err.to_string()))?;
            merged.extend_from_slice(&bytes[start..end]);
            tokio::fs::write(dest, merged)
                .await
                .map_err(|err| EngineError::Internal(err.to_string()))?;
        }
        Ok(DownloadReceipt {
            bytes_written: (end - start) as u64,
            resumed: start > 0,
        })
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    install: PathBuf,
    harness: PathBuf,
    journal: PathBuf,
    commit_id: String,
    keys: TestKeyPair,
    new_app: Vec<u8>,
    original_app: Vec<u8>,
}

fn full_entry(content: &[u8]) -> serde_json::Value {
    json!({
        "sha256": sha256_bytes(content),
        "size": content.len(),
        "strategy": "full",
        "full_key": format!("blobs/{}", sha256_bytes(content))
    })
}

async fn prepare_fixture() -> Fixture {
    let keys = TestKeyPair::generate();
    let dir = tempfile::tempdir().unwrap();
    let install = dir.path().join("install");
    let staging = dir.path().join("staging");
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&install).unwrap();
    std::fs::create_dir_all(&journal).unwrap();

    let harness = install.join("harness.exe");
    std::fs::copy(
        PathBuf::from(env!("CARGO_BIN_EXE_worker_harness")),
        &harness,
    )
    .unwrap();
    let old_app = b"old application binary";
    std::fs::write(install.join("App.exe"), old_app).unwrap();
    let original_app = old_app.to_vec();
    let keep = b"unchanged content";
    std::fs::write(install.join("keep.txt"), keep).unwrap();
    std::fs::write(install.join("old.txt"), b"remove me").unwrap();
    std::fs::write(
        journal.join("known_files.json"),
        serde_json::to_vec(&json!({ "files": ["old.txt"] })).unwrap(),
    )
    .unwrap();

    let new_app = b"new application binary".to_vec();
    let objects = HashMap::from([
        (format!("blobs/{}", sha256_bytes(&new_app)), new_app.clone()),
        (format!("blobs/{}", sha256_bytes(ADDED)), ADDED.to_vec()),
    ]);
    let manifest = json!({
        "schema": 1,
        "version": "2.0.0",
        "channel": "stable",
        "min_client_schema": 1,
        "files": {
            "App.exe": full_entry(&new_app),
            "added.txt": full_entry(ADDED),
            "keep.txt": full_entry(keep)
        }
    });
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    let signature = keys.sign(&manifest_bytes).into_bytes();
    let engine = Engine::new(EngineConfig {
        endpoint: Url::parse("https://updates.example.com/manifests/latest.json").unwrap(),
        signing_public_keys: vec![keys.public_key().to_string()],
        channel: "stable".into(),
        current_version: "1.0.0".into(),
        install_root: install.clone(),
        staging_root: staging,
        journal_root: journal.clone(),
        limits: Limits::default(),
        fetcher: Arc::new(MapFetcher {
            manifest: manifest_bytes,
            signature,
            objects,
        }),
        allow_insecure_http: false,
        crash_loop_threshold: 2,
    })
    .unwrap();

    engine.check().await.unwrap();
    let prepared = engine.prepare(Box::new(|_| {})).await.unwrap();
    let pending = engine.begin_commit(&prepared).unwrap();

    // The worker normally waits for the app to exit. The test process is the
    // parent here, so disable the wait explicitly.
    let mut config = state::read_apply_config(&journal).unwrap().unwrap();
    config.parent_pid = 0;
    state::write_apply_config(&journal, &config).unwrap();

    Fixture {
        _dir: dir,
        install,
        harness,
        journal,
        commit_id: pending.commit_id,
        keys,
        new_app,
        original_app,
    }
}

fn run_worker(fixture: &Fixture, public_key: &str) -> std::process::Output {
    Command::new(&fixture.harness)
        .arg("--delta-update-apply")
        .arg(&fixture.journal)
        .arg(&fixture.commit_id)
        .env("WORKER_HARNESS_KEYS", public_key)
        .output()
        .unwrap()
}

fn run_recovery(fixture: &Fixture, public_key: &str) -> std::process::Output {
    Command::new(&fixture.harness)
        .arg("--delta-update-recover")
        .arg(&fixture.journal)
        .arg("2")
        .env("WORKER_HARNESS_KEYS", public_key)
        .output()
        .unwrap()
}

#[tokio::test]
async fn worker_applies_a_verified_commit_in_a_separate_process() {
    let fixture = prepare_fixture().await;

    let output = run_worker(&fixture, fixture.keys.public_key());
    assert!(
        output.status.success(),
        "worker exited with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(
        std::fs::read(fixture.install.join("App.exe")).unwrap(),
        fixture.new_app
    );
    assert_eq!(
        std::fs::read(fixture.install.join("added.txt")).unwrap(),
        ADDED
    );
    assert!(!fixture.install.join("old.txt").exists());
    assert!(fixture
        .install
        .join(".old")
        .join(&fixture.commit_id)
        .join("App.exe")
        .exists());
    assert!(fixture.journal.join("commit.status").exists());
    let worker_log = std::fs::read_to_string(fixture.journal.join("worker.log")).unwrap();
    assert!(
        worker_log.contains(&format!("apply commit {} ok in", fixture.commit_id)),
        "worker.log should record a successful commit: {worker_log}"
    );
    let retries =
        retries_in_worker_log(&worker_log).expect("worker.log should report a numeric retry count");
    assert!(
        retries <= 3,
        "retries must match the 3-attempt budget: {retries}"
    );
}

/// SIGKILL (`TerminateProcess`) mid-commit: no `commit.status`, the journal may
/// be partially applied. The next start must replay the remaining ops and
/// finish. This is the antivirus / Task Manager failure mode, which never runs
/// destructors or the panic hook.
#[cfg(windows)]
#[tokio::test]
async fn worker_killed_mid_commit_recovers_on_next_start() {
    use std::os::windows::fs::OpenOptionsExt;

    let fixture = prepare_fixture().await;
    // Block `old.txt` with no sharing so the worker stalls inside its retry
    // budget instead of finishing before we can terminate it.
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(fixture.install.join("old.txt"))
        .unwrap();

    let mut child = Command::new(&fixture.harness)
        .arg("--delta-update-apply")
        .arg(&fixture.journal)
        .arg(&fixture.commit_id)
        .env("WORKER_HARNESS_KEYS", fixture.keys.public_key())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    child.kill().unwrap();
    let _ = child.wait();
    drop(lock);

    let log = std::fs::read_to_string(fixture.journal.join("worker.log")).unwrap();
    assert!(
        log.contains("apply started"),
        "worker.log must record the killed start: {log}"
    );
    assert!(
        !fixture.journal.join("commit.status").exists(),
        "a killed worker cannot write a status"
    );

    let output = run_recovery(&fixture, fixture.keys.public_key());
    assert!(
        output.status.success(),
        "recovery after SIGKILL exited with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read(fixture.install.join("App.exe")).unwrap(),
        fixture.new_app
    );
    assert_eq!(
        std::fs::read(fixture.install.join("added.txt")).unwrap(),
        ADDED
    );
    assert!(!fixture.install.join("old.txt").exists());
    assert!(fixture.journal.join("commit.status").exists());
}

#[tokio::test]
async fn worker_recovery_replays_a_pending_commit_in_a_separate_process() {
    let fixture = prepare_fixture().await;

    let output = run_recovery(&fixture, fixture.keys.public_key());
    assert!(
        output.status.success(),
        "recovery worker exited with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(
        std::fs::read(fixture.install.join("App.exe")).unwrap(),
        fixture.new_app
    );
    assert_eq!(
        std::fs::read(fixture.install.join("added.txt")).unwrap(),
        ADDED
    );
    assert!(!fixture.install.join("old.txt").exists());
    assert!(fixture.journal.join("commit.status").exists());
}

#[tokio::test]
async fn worker_recovery_rolls_back_after_the_crash_loop_threshold() {
    let fixture = prepare_fixture().await;
    let key = fixture.keys.public_key().to_string();

    for _ in 0..3 {
        let output = run_recovery(&fixture, &key);
        assert!(
            output.status.success(),
            "recovery worker exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    assert_eq!(
        std::fs::read(fixture.install.join("App.exe")).unwrap(),
        fixture.original_app
    );
    assert!(!fixture.install.join("added.txt").exists());
    assert!(fixture.install.join("old.txt").exists());
    assert_eq!(
        std::fs::read_to_string(fixture.journal.join("skipped_version")).unwrap(),
        "2.0.0"
    );
    assert!(!fixture.journal.join("commit.journal").exists());
}

#[tokio::test]
async fn worker_recovery_rejects_an_untrusted_key() {
    let fixture = prepare_fixture().await;
    let other = TestKeyPair::generate();

    let output = run_recovery(&fixture, other.public_key());
    assert!(
        !output.status.success(),
        "recovery must not replay an unverified journal"
    );

    assert_eq!(
        std::fs::read(fixture.install.join("App.exe")).unwrap(),
        fixture.original_app
    );
    assert!(!fixture.install.join("added.txt").exists());
    assert!(fixture.install.join("old.txt").exists());
    let worker_log = std::fs::read_to_string(fixture.journal.join("worker.log")).unwrap();
    assert!(
        worker_log.contains("recovery failed"),
        "worker.log should record the rejection: {worker_log}"
    );
}

#[tokio::test]
async fn worker_rejects_a_journal_without_the_matching_key() {
    let fixture = prepare_fixture().await;
    let other = TestKeyPair::generate();

    let output = run_worker(&fixture, other.public_key());
    assert!(
        !output.status.success(),
        "worker must not apply an unverified journal"
    );

    assert_eq!(
        std::fs::read(fixture.install.join("App.exe")).unwrap(),
        fixture.original_app
    );
    assert!(!fixture.install.join("added.txt").exists());
    assert!(fixture.install.join("old.txt").exists());
    let worker_log = std::fs::read_to_string(fixture.journal.join("worker.log")).unwrap();
    assert!(
        worker_log.contains("apply commit"),
        "worker.log should record the rejection: {worker_log}"
    );
    assert!(
        retries_in_worker_log(&worker_log).is_some(),
        "failure lines should also report a retry count: {worker_log}"
    );
}
