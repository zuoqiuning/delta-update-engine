//! The `latest.json` and `latest.json.minisig` objects are published
//! separately, so a release briefly exposes a manifest with the other
//! version's signature. A check must retry that pair instead of surfacing a
//! signature error to the user.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use delta_update_engine::fetch::{DownloadReceipt, Fetcher};
use delta_update_engine::{Engine, EngineConfig, EngineError, Limits};
use delta_update_testkit::TestKeyPair;
use serde_json::json;
use url::Url;

fn manifest(version: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schema": 1,
        "version": version,
        "min_client_schema": 1,
        "files": {
            "App.exe": {
                "sha256": "a".repeat(64),
                "size": 1,
                "strategy": "full",
                "full_key": "artifacts/full/App.exe"
            }
        }
    }))
    .unwrap()
}

/// Serves one manifest but a signature stream that starts stale, like a reader
/// landing between the two object writes of a release.
struct WindowFetcher {
    manifest: Vec<u8>,
    stale_signature: String,
    fresh_signature: String,
    /// When true the mismatch never resolves, like a genuinely tampered pair.
    repeat_stale: bool,
    signature_calls: AtomicUsize,
}

#[async_trait]
impl Fetcher for WindowFetcher {
    async fn fetch_manifest(&self) -> Result<Vec<u8>, EngineError> {
        Ok(self.manifest.clone())
    }

    async fn fetch_signature(&self) -> Result<Vec<u8>, EngineError> {
        let call = self.signature_calls.fetch_add(1, Ordering::SeqCst);
        let signature = if self.repeat_stale || call == 0 {
            &self.stale_signature
        } else {
            &self.fresh_signature
        };
        Ok(signature.as_bytes().to_vec())
    }

    async fn download(
        &self,
        _key: &str,
        _dest: &Path,
        _resume_from: u64,
        _max_bytes: u64,
    ) -> Result<DownloadReceipt, EngineError> {
        Err(EngineError::Internal(
            "download is not used by check".into(),
        ))
    }
}

fn engine(
    fetcher: Arc<WindowFetcher>,
    public_key: &str,
    root: &Path,
    staging: &Path,
    journal: &Path,
) -> Engine {
    Engine::new(EngineConfig {
        endpoint: Url::parse("https://updates.example.com/manifests/latest.json").unwrap(),
        signing_public_keys: vec![public_key.to_string()],
        channel: "stable".into(),
        current_version: "1.0.0".into(),
        install_root: root.to_path_buf(),
        staging_root: staging.to_path_buf(),
        journal_root: journal.to_path_buf(),
        limits: Limits::default(),
        fetcher,
        allow_insecure_http: false,
        crash_loop_threshold: 2,
    })
    .unwrap()
}

fn fixture() -> (tempfile::TempDir, TestKeyPair, String, String) {
    let keys = TestKeyPair::generate();
    let fresh = keys.sign(&manifest("2.0.0"));
    // Same key id, different bytes: exactly the mid-publish mismatch, not a
    // key that the client does not trust.
    let stale = keys.sign(&manifest("1.9.0"));
    (tempfile::tempdir().unwrap(), keys, fresh, stale)
}

#[tokio::test]
async fn a_manifest_read_during_publishing_is_retried_and_succeeds() {
    let (dir, keys, fresh, stale) = fixture();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let fetcher = Arc::new(WindowFetcher {
        manifest: manifest("2.0.0"),
        stale_signature: stale,
        fresh_signature: fresh,
        repeat_stale: false,
        signature_calls: AtomicUsize::new(0),
    });
    let calls = Arc::clone(&fetcher);
    let engine = engine(
        fetcher,
        keys.public_key(),
        &root,
        &dir.path().join("staging"),
        &dir.path().join("journal"),
    );

    let outcome = engine.check().await.expect("retry the mismatched pair");
    assert_eq!(outcome.version.as_deref(), Some("2.0.0"));
    assert_eq!(calls.signature_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_persistent_mismatch_keeps_the_signature_code_and_adds_a_hint() {
    let (dir, keys, _fresh, stale) = fixture();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let fetcher = Arc::new(WindowFetcher {
        manifest: manifest("2.0.0"),
        fresh_signature: keys.sign(&manifest("2.0.0")),
        stale_signature: stale,
        repeat_stale: true,
        signature_calls: AtomicUsize::new(0),
    });
    let calls = Arc::clone(&fetcher);
    let engine = engine(
        fetcher,
        keys.public_key(),
        &root,
        &dir.path().join("staging"),
        &dir.path().join("journal"),
    );

    let error = engine.check().await.unwrap_err();
    assert_eq!(error.code(), "E_SIGNATURE_INVALID");
    assert!(!error.retryable(), "a mismatch must not become transient");
    assert!(error.to_string().contains("may be in progress"), "{error}");
    assert_eq!(calls.signature_calls.load(Ordering::SeqCst), 3);
}
