//! Cost of scanning a large install tree through `Engine::diff`.
//!
//! `diff` walks the install root, hashes every file the journal cache does not
//! know yet, and classifies each manifest entry. `prepare` re-hashes the
//! unchanged subset with the same helper before any download starts, so the
//! cold number is the upper bound for both paths and no separate prepare
//! benchmark is needed.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use delta_update_engine::crypto::sha256_bytes;
use delta_update_engine::fetch::{DownloadReceipt, Fetcher};
use delta_update_engine::manifest::{parse_manifest, HashCache, Manifest};
use delta_update_engine::{Engine, EngineConfig, EngineError, Limits};

const FILES: usize = 10_000;
const FILE_BYTES: usize = 1024;

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

struct Fixture {
    /// Keeps the temporary install tree alive for the whole benchmark.
    _dir: tempfile::TempDir,
    engine: Engine,
    install_root: PathBuf,
    manifest: Manifest,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let install_root = dir.path().join("install");
        let staging_root = dir.path().join("staging");
        let journal_root = dir.path().join("journal");
        for root in [&install_root, &staging_root, &journal_root] {
            std::fs::create_dir_all(root).unwrap();
        }

        let content = vec![0x5a_u8; FILE_BYTES];
        let sha256 = sha256_bytes(&content);
        let files = install_root.join("assets");
        std::fs::create_dir_all(&files).unwrap();
        for index in 0..FILES {
            std::fs::write(files.join(name(index)), &content).unwrap();
        }

        let endpoint = "https://example.com/latest.json".parse().unwrap();
        let engine = Engine::new(EngineConfig {
            endpoint,
            signing_public_keys: vec!["bench-key".into()],
            channel: "stable".into(),
            current_version: "1.0.0".into(),
            install_root: install_root.clone(),
            staging_root,
            journal_root,
            limits: Limits::default(),
            fetcher: Arc::new(StubFetcher),
            allow_insecure_http: false,
            crash_loop_threshold: 3,
        })
        .unwrap();

        Self {
            _dir: dir,
            engine,
            install_root,
            manifest: manifest(&sha256),
        }
    }
}

fn name(index: usize) -> String {
    format!("tile-{index:05}.bin")
}

/// A manifest whose entries all match the tree, so every file lands in the
/// `unchanged` bucket and no repair/removal work happens during the scan.
fn manifest(sha256: &str) -> Manifest {
    let files = (0..FILES)
        .map(|index| {
            let entry = serde_json::json!({
                "sha256": sha256,
                "size": FILE_BYTES as u64,
                "strategy": "full",
            });
            (format!("assets/{}", name(index)), entry)
        })
        .collect::<serde_json::Map<_, _>>();
    let raw = serde_json::to_vec(&serde_json::json!({
        "schema": 1,
        "version": "2.0.0",
        "channel": "stable",
        "min_client_schema": 1,
        "files": files,
    }))
    .unwrap();
    parse_manifest(&raw, &Limits::default()).unwrap()
}

fn scan_tree(c: &mut Criterion) {
    let fixture = Fixture::new();
    let journal_root = fixture.engine.config().journal_root.clone();

    let mut group = c.benchmark_group("scan_tree");
    group.throughput(Throughput::Bytes((FILES * FILE_BYTES) as u64));
    // 10k files take hundreds of milliseconds per iteration; the default 100
    // samples would keep a CI runner busy for minutes.
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));

    group.bench_function("cold", |b| {
        // The hash cache is what makes a repeated scan cheap, so clearing it
        // in the setup phase keeps the measurement on the first-scan path.
        b.iter_batched(
            || HashCache::clear(&journal_root),
            |()| {
                fixture
                    .engine
                    .diff(&fixture.install_root, &fixture.manifest)
                    .unwrap()
            },
            BatchSize::SmallInput,
        );
    });

    fixture
        .engine
        .diff(&fixture.install_root, &fixture.manifest)
        .unwrap();
    // Warm scans still read and write the cache file, so a slow CI disk shows
    // up here rather than in `cold`; compare like-for-like before calling it a
    // regression.
    group.bench_function("warm", |b| {
        b.iter(|| {
            fixture
                .engine
                .diff(&fixture.install_root, &fixture.manifest)
                .unwrap()
        });
    });

    group.finish();
}

criterion_group!(benches, scan_tree);
criterion_main!(benches);
