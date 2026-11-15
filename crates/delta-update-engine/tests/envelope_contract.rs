//! RFC 0001 single-object envelope contract.
//!
//! `latest.envelope.json` carries the manifest bytes and their signature in one
//! PUT, so a reader never lands between the two legacy objects. The envelope is
//! the preferred path; a broken one is fatal (falling back would downgrade the
//! client to the windowed path), while a missing one falls back like 1.0.

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

fn envelope(manifest: &[u8], signature: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schema": 2,
        "manifest": std::str::from_utf8(manifest).unwrap(),
        "signature": signature,
    }))
    .unwrap()
}

struct EnvelopeFetcher {
    envelope: Option<Vec<u8>>,
    manifest: Vec<u8>,
    signature: Vec<u8>,
    manifest_calls: Arc<AtomicUsize>,
    signature_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Fetcher for EnvelopeFetcher {
    async fn fetch_manifest(&self) -> Result<Vec<u8>, EngineError> {
        self.manifest_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.manifest.clone())
    }

    async fn fetch_signature(&self) -> Result<Vec<u8>, EngineError> {
        self.signature_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.signature.clone())
    }

    async fn fetch_envelope(&self) -> Result<Option<Vec<u8>>, EngineError> {
        Ok(self.envelope.clone())
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

fn engine(fetcher: EnvelopeFetcher, keys: &TestKeyPair) -> (tempfile::TempDir, Engine) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let engine = Engine::new(EngineConfig {
        endpoint: Url::parse("https://updates.example.com/manifests/latest.json").unwrap(),
        signing_public_keys: vec![keys.public_key().to_string()],
        channel: "stable".into(),
        current_version: "1.0.0".into(),
        install_root: root,
        staging_root: dir.path().join("staging"),
        journal_root: dir.path().join("journal"),
        limits: Limits::default(),
        fetcher: std::sync::Arc::new(fetcher),
        allow_insecure_http: false,
        crash_loop_threshold: 2,
    })
    .unwrap();
    (dir, engine)
}

#[tokio::test]
async fn the_envelope_is_preferred_and_the_legacy_pair_is_never_fetched() {
    let keys = TestKeyPair::generate();
    let current = manifest("2.0.0");
    let stale = manifest("1.9.0");
    let manifest_calls = Arc::new(AtomicUsize::new(0));
    let signature_calls = Arc::new(AtomicUsize::new(0));
    let fetcher = EnvelopeFetcher {
        envelope: Some(envelope(&current, &keys.sign(&current))),
        manifest: stale.clone(),
        signature: keys.sign(&stale).into_bytes(),
        manifest_calls: Arc::clone(&manifest_calls),
        signature_calls: Arc::clone(&signature_calls),
    };
    let (_dir, engine) = engine(fetcher, &keys);

    let outcome = engine.check().await.expect("envelope verifies");
    assert_eq!(outcome.version.as_deref(), Some("2.0.0"));
    assert_eq!(manifest_calls.load(Ordering::SeqCst), 0);
    assert_eq!(signature_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_broken_envelope_is_fatal_and_does_not_fall_back() {
    let keys = TestKeyPair::generate();
    let current = manifest("2.0.0");
    let signature_calls = Arc::new(AtomicUsize::new(0));
    // Signature belongs to another manifest: the envelope is invalid.
    let fetcher = EnvelopeFetcher {
        envelope: Some(envelope(&current, &keys.sign(&manifest("1.9.0")))),
        manifest: current.clone(),
        signature: keys.sign(&current).into_bytes(),
        manifest_calls: Arc::new(AtomicUsize::new(0)),
        signature_calls: Arc::clone(&signature_calls),
    };
    let (_dir, engine) = engine(fetcher, &keys);

    let error = engine.check().await.unwrap_err();
    assert_eq!(error.code(), "E_SIGNATURE_INVALID");
    assert_eq!(
        signature_calls.load(Ordering::SeqCst),
        0,
        "a broken envelope must not trigger the legacy path"
    );
}

#[tokio::test]
async fn a_missing_envelope_falls_back_to_the_legacy_pair() {
    let keys = TestKeyPair::generate();
    let current = manifest("2.0.0");
    let signature_calls = Arc::new(AtomicUsize::new(0));
    let fetcher = EnvelopeFetcher {
        envelope: None,
        manifest: current.clone(),
        signature: keys.sign(&current).into_bytes(),
        manifest_calls: Arc::new(AtomicUsize::new(0)),
        signature_calls: Arc::clone(&signature_calls),
    };
    let (_dir, engine) = engine(fetcher, &keys);

    let outcome = engine.check().await.expect("legacy pair verifies");
    assert_eq!(outcome.version.as_deref(), Some("2.0.0"));
    assert_eq!(signature_calls.load(Ordering::SeqCst), 1);
}
