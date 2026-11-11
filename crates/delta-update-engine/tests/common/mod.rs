#![allow(dead_code)]

//! Shared integration-test fixture: a real install root, staging root and
//! journal root backed by the real filesystem and a fetcher stub.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use delta_update_engine::crypto::sha256_bytes;
use delta_update_engine::fetch::{DownloadReceipt, Fetcher};
use delta_update_engine::journal::state::{self, ApplyConfig};
use delta_update_engine::staging::{PreparedPlan, StagedFile};
use delta_update_engine::{
    CommitStatus, DirectWriter, Engine, EngineConfig, EngineError, Limits, PendingCommit,
    PreparedUpdate, PrivilegedWriter,
};

pub const VERSION: &str = "2.0.0";
pub const OLD_APP: &[u8] = b"old-app";
pub const NEW_APP: &[u8] = b"new-app";
pub const ADDED_FILE: &[u8] = b"new-added";
pub const REMOVED_FILE: &[u8] = b"old-removed";

/// Create an isolated root/staging/journal triple for one integration test.
pub fn tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    for name in ["root", "staging", "journal"] {
        std::fs::create_dir_all(dir.path().join(name)).expect("create test dir");
    }
    dir
}

pub fn root(dir: &Path) -> PathBuf {
    dir.join("root")
}

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

/// A prepared update from `1.0.0` to `2.0.0`:
/// `App.exe` is replaced, `added.txt` is added and `remove.txt` is removed.
pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub engine: Engine,
    pub prepared: PreparedUpdate,
}

impl Fixture {
    pub fn new(crash_loop_threshold: u32) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let install_root = dir.path().join("root");
        let staging_base = dir.path().join("staging");
        let journal = dir.path().join("journal");
        std::fs::create_dir_all(&install_root).unwrap();
        std::fs::create_dir_all(&journal).unwrap();

        std::fs::write(install_root.join("App.exe"), OLD_APP).unwrap();
        std::fs::write(install_root.join("remove.txt"), REMOVED_FILE).unwrap();

        let staging_root = staging_base.join(VERSION);
        let files = vec![
            staged("App.exe", NEW_APP, &staging_root),
            staged("added.txt", ADDED_FILE, &staging_root),
        ];
        let plan = PreparedPlan {
            version: VERSION.into(),
            manifest_hash: "f".repeat(64),
            staging_root: staging_root.clone(),
            files,
            removed: vec!["remove.txt".into()],
            all_files: vec!["App.exe".into(), "added.txt".into()],
        };
        plan.save(&journal.join("prepared.json")).unwrap();

        let endpoint: url::Url = "https://example.com/latest.json".parse().unwrap();
        let engine = Engine::new(EngineConfig {
            endpoint,
            signing_public_keys: vec!["test-key".into()],
            channel: "stable".into(),
            current_version: "1.0.0".into(),
            install_root,
            staging_root: staging_base,
            journal_root: journal,
            limits: Limits::default(),
            fetcher: Arc::new(StubFetcher),
            allow_insecure_http: false,
            crash_loop_threshold,
        })
        .unwrap();

        let prepared = PreparedUpdate {
            version: plan.version.clone(),
            staging_root: plan.staging_root.clone(),
            manifest_hash: plan.manifest_hash.clone(),
        };
        Self {
            dir,
            engine,
            prepared,
        }
    }

    pub fn root(&self) -> PathBuf {
        self.dir.path().join("root")
    }

    pub fn journal(&self) -> PathBuf {
        self.dir.path().join("journal")
    }

    pub fn staging(&self) -> PathBuf {
        self.dir.path().join("staging").join(VERSION)
    }
}

pub fn fixture() -> Fixture {
    Fixture::new(2)
}

pub fn fixture_with_threshold(threshold: u32) -> Fixture {
    Fixture::new(threshold)
}

fn staged(path: &str, content: &[u8], staging_root: &Path) -> StagedFile {
    let staging_path = staging_root.join("files").join(path);
    if let Some(parent) = staging_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&staging_path, content).unwrap();
    StagedFile {
        path: path.into(),
        staging_path,
        sha256: sha256_bytes(content),
        size: content.len() as u64,
    }
}

pub fn apply(fixture: &Fixture) -> (ApplyConfig, PendingCommit, CommitStatus) {
    let pending = fixture.engine.begin_commit(&fixture.prepared).unwrap();
    let config = state::read_apply_config(&fixture.journal())
        .unwrap()
        .unwrap();
    let status = DirectWriter
        .apply_journal(&config, &pending.journal_path)
        .unwrap();
    assert_eq!(
        status,
        CommitStatus::Completed {
            version: VERSION.into(),
            commit_id: pending.commit_id.clone()
        }
    );
    (config, pending, status)
}
