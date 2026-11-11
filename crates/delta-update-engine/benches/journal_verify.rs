use std::path::PathBuf;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use delta_update_engine::crypto::sha256_bytes;
use delta_update_engine::journal::{replay, FileOp, Journal, ReplayContext};
use delta_update_engine::staging::{verify_plan, PreparedPlan, StagedFile};

const COMMIT_ID: &str = "00000000-0000-4000-8000-000000000000";

struct ReplayFixture {
    /// Keeps the temporary tree alive for the duration of the replay.
    _dir: tempfile::TempDir,
    install_root: PathBuf,
    staging_root: PathBuf,
    journal_root: PathBuf,
    journal: Journal,
}

impl ReplayFixture {
    /// 50 replaced files produce 100 journal operations (Backup + Place).
    fn new(files: usize) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let install_root = dir.path().join("install");
        let staging_root = dir.path().join("staging");
        let journal_root = dir.path().join("journal");
        std::fs::create_dir_all(install_root.join("bin")).unwrap();
        std::fs::create_dir_all(&journal_root).unwrap();

        let mut ops = Vec::with_capacity(files * 2);
        for index in 0..files {
            let name = format!("bin/file-{index:03}.bin");
            let old = format!("old-{index}");
            let new = format!("new-{index}-{}", "x".repeat(64));
            std::fs::write(install_root.join(&name), old.as_bytes()).unwrap();
            let staged = staging_root.join("files").join(&name);
            std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
            std::fs::write(&staged, new.as_bytes()).unwrap();
            ops.push(FileOp::Backup {
                src: name.clone(),
                dst: format!(".old/{COMMIT_ID}/{name}"),
            });
            ops.push(FileOp::Place {
                staging_src: format!("files/{name}"),
                dst: name,
                sha256: sha256_bytes(new.as_bytes()),
                size: new.len() as u64,
            });
        }

        Self {
            _dir: dir,
            install_root,
            staging_root,
            journal_root,
            journal: Journal::new(COMMIT_ID, "2.0.0", "1.0.0", ops),
        }
    }

    fn replay(&self) {
        let context = ReplayContext {
            install_root: &self.install_root,
            staging_root: &self.staging_root,
            journal_root: &self.journal_root,
            commit_id: COMMIT_ID,
        };
        replay(&context, &self.journal).expect("journal replay");
    }
}

fn journal_replay(c: &mut Criterion) {
    let mut group = c.benchmark_group("journal");
    group.bench_function("replay_100_ops", |b| {
        b.iter_batched(
            || ReplayFixture::new(50),
            |fixture| fixture.replay(),
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn journal_codec(c: &mut Criterion) {
    let ops: Vec<FileOp> = (0..50_000)
        .map(|index| FileOp::Place {
            staging_src: format!("files/assets/tile-{index:05}.png"),
            dst: format!("assets/tile-{index:05}.png"),
            sha256: "a".repeat(64),
            size: 4096,
        })
        .collect();
    let journal = Journal::new(COMMIT_ID, "2.0.0", "1.0.0", ops);
    let encoded = journal.encode().expect("encode journal");

    let mut group = c.benchmark_group("journal");
    group.throughput(Throughput::Elements(50_000));
    group.bench_function("encode_50k_ops", |b| {
        b.iter(|| journal.encode().expect("encode journal"));
    });
    group.bench_function("decode_50k_ops", |b| {
        b.iter(|| Journal::decode(&encoded).expect("decode journal"));
    });
    group.finish();
}

fn verify_prepared_plan(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let staging_root = dir.path().join("staging");
    let content = vec![0x5a_u8; 1024];
    let sha256 = sha256_bytes(&content);
    let mut files = Vec::with_capacity(1000);
    for index in 0..1000 {
        let path = format!("files/file-{index:04}.bin");
        let staging_path = staging_root.join(&path);
        std::fs::create_dir_all(staging_path.parent().unwrap()).unwrap();
        std::fs::write(&staging_path, &content).unwrap();
        files.push(StagedFile {
            path: path.trim_start_matches("files/").to_string(),
            staging_path,
            sha256: sha256.clone(),
            size: content.len() as u64,
        });
    }
    let plan = PreparedPlan {
        version: "2.0.0".into(),
        manifest_hash: "f".repeat(64),
        staging_root,
        files,
        removed: Vec::new(),
        all_files: Vec::new(),
    };

    let mut group = c.benchmark_group("prepare");
    group.throughput(Throughput::Elements(1000));
    group.bench_function("verify_plan_1000_files", |b| {
        b.iter(|| verify_plan(&plan).expect("verify plan"));
    });
    group.finish();
}

criterion_group!(benches, journal_replay, journal_codec, verify_prepared_plan);
criterion_main!(benches);
