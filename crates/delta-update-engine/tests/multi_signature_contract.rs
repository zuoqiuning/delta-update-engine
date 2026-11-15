//! RFC 0001 detached multi-signature contract.
//!
//! The primary `.minisig` stays authoritative for old clients. When it does not
//! verify, the client tries the `.minisig.<key_id>` files the manifest declares
//! and accepts the first one that does. Missing or unknown attachments must
//! never change the outcome of a release that only has a primary signature.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use delta_update_engine::fetch::{DownloadReceipt, Fetcher};
use delta_update_engine::{Engine, EngineConfig, EngineError, Limits};
use delta_update_testkit::TestKeyPair;
use serde_json::json;
use url::Url;

fn manifest(version: &str, key_ids: &[String]) -> Vec<u8> {
    let mut value = json!({
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
    });
    if !key_ids.is_empty() {
        value["signing_key_ids"] = serde_json::to_value(key_ids).unwrap();
    }
    serde_json::to_vec(&value).unwrap()
}

struct FixtureFetcher {
    manifest: Vec<u8>,
    primary: Vec<u8>,
    attachments: HashMap<String, Vec<u8>>,
    fetched: Mutex<Vec<String>>,
}

#[async_trait]
impl Fetcher for FixtureFetcher {
    async fn fetch_manifest(&self) -> Result<Vec<u8>, EngineError> {
        Ok(self.manifest.clone())
    }

    async fn fetch_signature(&self) -> Result<Vec<u8>, EngineError> {
        Ok(self.primary.clone())
    }

    async fn fetch_attachment(&self, key_id: &str) -> Result<Option<Vec<u8>>, EngineError> {
        self.fetched.lock().unwrap().push(key_id.to_string());
        Ok(self.attachments.get(key_id).cloned())
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

fn setup(
    manifest: Vec<u8>,
    primary: &str,
    attachments: &[(&TestKeyPair, &[u8])],
    trusted: &[&TestKeyPair],
) -> (tempfile::TempDir, Arc<FixtureFetcher>, Engine) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let fetcher = Arc::new(FixtureFetcher {
        manifest,
        primary: primary.as_bytes().to_vec(),
        attachments: attachments
            .iter()
            .map(|(key, bytes)| (key.key_id(), bytes.to_vec()))
            .collect(),
        fetched: Mutex::new(Vec::new()),
    });
    let engine = Engine::new(EngineConfig {
        endpoint: Url::parse("https://updates.example.com/manifests/latest.json").unwrap(),
        signing_public_keys: trusted.iter().map(|key| key.public_key().into()).collect(),
        channel: "stable".into(),
        current_version: "1.0.0".into(),
        install_root: root,
        staging_root: dir.path().join("staging"),
        journal_root: dir.path().join("journal"),
        limits: Limits::default(),
        fetcher: Arc::clone(&fetcher) as Arc<dyn Fetcher>,
        allow_insecure_http: false,
        crash_loop_threshold: 2,
    })
    .unwrap();
    (dir, fetcher, engine)
}

#[tokio::test]
async fn a_primary_signature_alone_never_fetches_attachments() {
    let keys = TestKeyPair::generate();
    let bytes = manifest("2.0.0", &[]);
    let signature = keys.sign(&bytes);
    let (_dir, fetcher, engine) = setup(bytes, &signature, &[], &[&keys]);

    let outcome = engine.check().await.expect("primary signature verifies");
    assert_eq!(outcome.version.as_deref(), Some("2.0.0"));
    assert!(fetcher.fetched.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_valid_attachment_is_accepted_when_the_primary_does_not_verify() {
    let old = TestKeyPair::generate();
    let new = TestKeyPair::generate();
    let bytes = manifest("2.0.0", &[new.key_id()]);
    // Primary claims the same key id but signs different bytes: exactly the
    // mid-rotation case the additional signature exists for.
    let stale_primary = old.sign(&manifest("1.9.0", &[]));
    let attachment = new.sign(&bytes);
    let (_dir, fetcher, engine) = setup(
        bytes,
        &stale_primary,
        &[(&new, attachment.as_bytes())],
        &[&old, &new],
    );

    let outcome = engine.check().await.expect("attachment is accepted");
    assert_eq!(outcome.version.as_deref(), Some("2.0.0"));
    assert_eq!(
        *fetcher.fetched.lock().unwrap(),
        vec![new.key_id()],
        "only the declared attachment is fetched"
    );
}

#[tokio::test]
async fn every_signature_invalid_returns_the_signature_error() {
    let old = TestKeyPair::generate();
    let new = TestKeyPair::generate();
    let bytes = manifest("2.0.0", &[new.key_id()]);
    let stale_primary = old.sign(&manifest("1.9.0", &[]));
    let stale_attachment = new.sign(&manifest("1.9.0", &[]));
    let (_dir, _fetcher, engine) = setup(
        bytes,
        &stale_primary,
        &[(&new, stale_attachment.as_bytes())],
        &[&old, &new],
    );

    let error = engine.check().await.unwrap_err();
    assert_eq!(error.code(), "E_SIGNATURE_INVALID");
    assert!(!error.retryable());
}

#[tokio::test]
async fn a_declared_but_missing_attachment_keeps_the_primary_error() {
    let old = TestKeyPair::generate();
    let new = TestKeyPair::generate();
    let bytes = manifest("2.0.0", &[new.key_id()]);
    let stale_primary = old.sign(&manifest("1.9.0", &[]));
    let (_dir, fetcher, engine) = setup(bytes, &stale_primary, &[], &[&old, &new]);

    let error = engine.check().await.unwrap_err();
    assert_eq!(error.code(), "E_SIGNATURE_INVALID");
    let fetched = fetcher.fetched.lock().unwrap().clone();
    assert!(
        fetched.iter().all(|key_id| *key_id == new.key_id()),
        "only declared ids are fetched: {fetched:?}"
    );
    assert!(!fetched.is_empty());
}

#[tokio::test]
async fn an_undeclared_attachment_file_is_never_fetched() {
    let old = TestKeyPair::generate();
    let new = TestKeyPair::generate();
    let bytes = manifest("2.0.0", &[]);
    let stale_primary = old.sign(&manifest("1.9.0", &[]));
    let attachment = new.sign(&bytes);
    let (_dir, fetcher, engine) = setup(
        bytes,
        &stale_primary,
        &[(&new, attachment.as_bytes())],
        &[&old, &new],
    );

    let error = engine.check().await.unwrap_err();
    assert_eq!(error.code(), "E_SIGNATURE_INVALID");
    assert!(
        fetcher.fetched.lock().unwrap().is_empty(),
        "an attachment that is not declared in the manifest is invisible"
    );
}
