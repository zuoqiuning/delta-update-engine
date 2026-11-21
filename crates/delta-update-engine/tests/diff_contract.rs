use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use delta_update_engine::crypto::sha256_bytes;
use delta_update_engine::fetch::{DownloadReceipt, Fetcher};
use delta_update_engine::manifest::parse_manifest;
use delta_update_engine::{Engine, EngineConfig, EngineError, Limits};

struct StubFetcher;

#[async_trait]
impl Fetcher for StubFetcher {
    async fn fetch_manifest(&self) -> Result<Vec<u8>, EngineError> {
        Err(EngineError::Cancelled)
    }

    async fn fetch_signature(&self) -> Result<Vec<u8>, EngineError> {
        Err(EngineError::Cancelled)
    }

    async fn download(
        &self,
        _key: &str,
        _dest: &Path,
        _resume_from: u64,
        _max_bytes: u64,
    ) -> Result<DownloadReceipt, EngineError> {
        Err(EngineError::Cancelled)
    }
}

fn engine(install: PathBuf, journal: PathBuf) -> Engine {
    engine_for(install, journal, "1.0.0")
}

fn engine_for(install: PathBuf, journal: PathBuf, version: &str) -> Engine {
    Engine::new(EngineConfig {
        endpoint: "https://example.com/manifests/latest.json".parse().unwrap(),
        signing_public_keys: vec!["RWQtest".into()],
        channel: "stable".into(),
        current_version: version.into(),
        install_root: install,
        staging_root: journal.join("staging"),
        journal_root: journal,
        limits: Limits::default(),
        fetcher: Arc::new(StubFetcher),
        allow_insecure_http: false,
        crash_loop_threshold: 2,
    })
    .unwrap()
}

fn full_entry(sha: &str, size: u64) -> serde_json::Value {
    serde_json::json!({
        "sha256": sha,
        "size": size,
        "strategy": "full",
        "full_key": format!("artifacts/full/2.1.0/{sha}.bin"),
    })
}

#[test]
fn classifies_every_change_kind_and_skips_bookkeeping() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let journal = temp.path().join("journal");
    std::fs::create_dir_all(root.join(".old/commit")).unwrap();
    std::fs::create_dir_all(&journal).unwrap();

    std::fs::write(root.join("unchanged.bin"), b"same").unwrap();
    std::fs::write(root.join("modified.bin"), b"old").unwrap();
    std::fs::write(root.join("removed.txt"), b"gone").unwrap();
    std::fs::write(root.join("user.dat"), b"user").unwrap();
    std::fs::write(root.join(".old/commit/backup.bin"), b"backup").unwrap();
    std::fs::write(
        journal.join("known_files.json"),
        serde_json::json!({ "files": ["removed.txt"] }).to_string(),
    )
    .unwrap();

    let remote = serde_json::json!({
        "schema": 1,
        "version": "2.1.0",
        "min_client_schema": 1,
        "files": {
            "unchanged.bin": full_entry(&sha256_bytes(b"same"), 4),
            "modified.bin": full_entry(&sha256_bytes(b"new"), 3),
            "added.bin": full_entry(&sha256_bytes(b"added"), 5)
        }
    });
    let manifest = parse_manifest(remote.to_string().as_bytes(), &Limits::default()).unwrap();

    let result = engine(root.clone(), journal)
        .diff(&root, &manifest)
        .unwrap();

    assert_eq!(result.unchanged, vec!["unchanged.bin"]);
    assert_eq!(result.modified.len(), 1);
    assert_eq!(result.modified[0].path, "modified.bin");
    assert_eq!(result.added, vec!["added.bin"]);
    assert_eq!(result.removed, vec!["removed.txt"]);
    assert_eq!(result.untracked, vec!["user.dat"]);
    assert_eq!(result.download_bytes, 3 + 5);
    assert_eq!(result.total_bytes, 4 + 3 + 5);
}

#[test]
fn corrupt_known_history_degrades_to_first_migration() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let journal = temp.path().join("journal");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&journal).unwrap();
    std::fs::write(root.join("extra.txt"), b"extra").unwrap();
    std::fs::write(journal.join("known_files.json"), b"{not json").unwrap();

    let remote = serde_json::json!({
        "schema": 1,
        "version": "2.1.0",
        "min_client_schema": 1,
        "files": {}
    });
    let manifest = parse_manifest(remote.to_string().as_bytes(), &Limits::default()).unwrap();

    let result = engine(root.clone(), journal)
        .diff(&root, &manifest)
        .unwrap();
    assert_eq!(result.removed.len(), 0);
    assert_eq!(result.untracked, vec!["extra.txt"]);
}

#[test]
fn unchanged_entries_do_not_need_full_key() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let journal = temp.path().join("journal");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&journal).unwrap();
    std::fs::write(root.join("same.bin"), b"same").unwrap();

    let remote = serde_json::json!({
        "schema": 1,
        "version": "2.1.0",
        "min_client_schema": 1,
        "files": {
            "same.bin": {
                "sha256": sha256_bytes(b"same"),
                "size": 4,
                "strategy": "full"
            }
        }
    });
    let manifest = parse_manifest(remote.to_string().as_bytes(), &Limits::default()).unwrap();
    let result = engine(root.clone(), journal)
        .diff(&root, &manifest)
        .unwrap();
    assert_eq!(result.unchanged, vec!["same.bin"]);
}

#[test]
fn changed_entries_still_require_full_key() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let journal = temp.path().join("journal");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&journal).unwrap();
    std::fs::write(root.join("same.bin"), b"old").unwrap();

    let remote = serde_json::json!({
        "schema": 1,
        "version": "2.1.0",
        "min_client_schema": 1,
        "files": {
            "same.bin": {
                "sha256": sha256_bytes(b"new"),
                "size": 3,
                "strategy": "full"
            }
        }
    });
    let manifest = parse_manifest(remote.to_string().as_bytes(), &Limits::default()).unwrap();
    let error = engine(root.clone(), journal)
        .diff(&root, &manifest)
        .unwrap_err();
    assert_eq!(error.code(), "E_MANIFEST_PARSE");
}

fn multi_baseline_manifest() -> serde_json::Value {
    serde_json::json!({
        "schema": 1,
        "version": "2.1.0",
        "min_client_schema": 1,
        "files": {
            "app.bin": {
                "sha256": sha256_bytes(b"new"),
                "size": 3,
                "strategy": "qbsdiff+zstd",
                "delta_key": "artifacts/0.9.0-to-2.1.0/app.bin.tdp",
                "delta_size": 900,
                "full_key": "blobs/new",
                "deltas": [
                    {
                        "from_version": "1.0.0",
                        "strategy": "qbsdiff+zstd",
                        "delta_key": "artifacts/1.0.0-to-2.1.0/app.bin.tdp",
                        "delta_size": 40
                    },
                    {
                        "from_version": "0.9.0",
                        "strategy": "qbsdiff+zstd",
                        "delta_key": "artifacts/0.9.0-to-2.1.0/app.bin.tdp",
                        "delta_size": 80
                    }
                ]
            }
        }
    })
}

#[test]
fn selects_the_patch_for_the_running_version() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let journal = temp.path().join("journal");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&journal).unwrap();
    std::fs::write(root.join("app.bin"), b"old").unwrap();

    let manifest = parse_manifest(
        multi_baseline_manifest().to_string().as_bytes(),
        &Limits::default(),
    )
    .unwrap();
    let result = engine_for(root.clone(), journal, "1.0.0")
        .diff(&root, &manifest)
        .unwrap();

    let entry = &result.modified[0].entry;
    assert_eq!(
        entry.delta_key.as_deref(),
        Some("artifacts/1.0.0-to-2.1.0/app.bin.tdp")
    );
    assert_eq!(entry.delta_size, Some(40));
    assert_eq!(
        result.download_bytes, 40,
        "the estimate must match the patch this client will apply"
    );
}

#[test]
fn a_client_without_a_matching_baseline_downloads_full() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let journal = temp.path().join("journal");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&journal).unwrap();
    std::fs::write(root.join("app.bin"), b"old").unwrap();

    let manifest = parse_manifest(
        multi_baseline_manifest().to_string().as_bytes(),
        &Limits::default(),
    )
    .unwrap();
    let result = engine_for(root.clone(), journal, "1.5.0")
        .diff(&root, &manifest)
        .unwrap();

    let entry = &result.modified[0].entry;
    assert_eq!(entry.delta_key, None, "no baseline means no patch attempt");
    assert_eq!(entry.strategy, delta_update_engine::Strategy::Full);
    assert_eq!(result.download_bytes, 3, "full size, not a doomed patch");
}
