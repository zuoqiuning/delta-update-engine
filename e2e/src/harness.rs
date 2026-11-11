use std::collections::BTreeMap;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use delta_update_cli::cli::ReleaseArgs;
use delta_update_cli::pipeline;
use delta_update_engine::crypto::sha256_bytes;
use delta_update_engine::fetch::{FetchLimits, HttpFetcher};
use delta_update_engine::patch::container::{Codec, PatchHeader};
use delta_update_engine::{
    CommitStatus, Engine, EngineConfig, Limits, PendingCommit, PreparedUpdate,
};
use delta_update_testkit::{tree, MockServer, TestKeyPair};
use serde_json::{json, Map, Value};
use tempfile::{Builder, TempDir};
use url::Url;

pub const CHANNEL: &str = "stable";

/// Deterministic file body that stays highly similar across versions.
pub fn payload(fill: u8, len: usize) -> Vec<u8> {
    vec![fill; len]
}

pub fn tweak(bytes: &[u8], at: usize, replacement: &[u8]) -> Vec<u8> {
    let mut value = bytes.to_vec();
    value[at..at + replacement.len()].copy_from_slice(replacement);
    value
}

/// Run one async scenario on a single-threaded runtime. The mock server lives
/// in the same runtime, so a current-thread scheduler is enough.
pub fn block_on<F>(future: F) -> Result<(), String>
where
    F: Future<Output = Result<(), String>>,
{
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("tokio runtime: {err}"))?
        .block_on(future)
}

/// Isolated install root, staging root and journal root for one scenario.
pub struct Scenario {
    home: TempDir,
    staging: TempDir,
}

impl Scenario {
    pub fn new() -> io::Result<Self> {
        Self::from_dirs(tempfile::tempdir()?, tempfile::tempdir()?)
    }

    /// Staging lives on the workspace volume while the install root stays on
    /// the system temp volume. When the volumes differ, replacement exercises
    /// the copy + atomic rename fallback.
    pub fn cross_volume() -> io::Result<Self> {
        let home = tempfile::tempdir()?;
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
        std::fs::create_dir_all(&fixtures)?;
        let staging = Builder::new().prefix("staging-").tempdir_in(&fixtures)?;
        Self::from_dirs(home, staging)
    }

    fn from_dirs(home: TempDir, staging: TempDir) -> io::Result<Self> {
        std::fs::create_dir_all(home.path().join("root"))?;
        std::fs::create_dir_all(home.path().join("journal"))?;
        Ok(Self { home, staging })
    }

    pub fn root(&self) -> PathBuf {
        self.home.path().join("root")
    }

    pub fn journal(&self) -> PathBuf {
        self.home.path().join("journal")
    }

    pub fn staging(&self) -> PathBuf {
        self.staging.path().to_path_buf()
    }

    pub fn write_root(&self, files: &[(&str, &[u8])]) -> Result<(), String> {
        tree::write_tree(&self.root(), files).map_err(|err| format!("write root: {err}"))
    }

    /// Seed the historical official file set so removals are classified as
    /// `Removed` instead of `Untracked`.
    pub fn seed_known(&self, files: &[&str]) -> Result<(), String> {
        let body = serde_json::to_vec(&json!({ "files": files }))
            .map_err(|err| format!("serialize known files: {err}"))?;
        std::fs::write(self.journal().join("known_files.json"), body)
            .map_err(|err| format!("write known files: {err}"))
    }

    pub fn engine(
        &self,
        server: &MockServer,
        keys: &TestKeyPair,
        version: &str,
    ) -> Result<Engine, String> {
        self.engine_with_key(server, keys.public_key(), version)
    }

    pub fn engine_with_key(
        &self,
        server: &MockServer,
        public_key: &str,
        version: &str,
    ) -> Result<Engine, String> {
        let endpoint =
            Url::parse(&server.url("manifests/latest.json")).map_err(|err| err.to_string())?;
        let fetcher =
            HttpFetcher::new_default(endpoint.clone(), None, FetchLimits::default(), None, true)
                .map_err(|err| err.to_string())?;
        Engine::new(EngineConfig {
            endpoint,
            signing_public_keys: vec![public_key.to_string()],
            channel: CHANNEL.into(),
            current_version: version.into(),
            install_root: self.root(),
            staging_root: self.staging(),
            journal_root: self.journal(),
            limits: Limits::default(),
            fetcher: Arc::new(fetcher),
            allow_insecure_http: true,
            crash_loop_threshold: 2,
        })
        .map_err(|err| err.to_string())
    }
}

/// A signed manifest plus every object needed to update from `old` to `new`.
pub struct Release {
    pub server: MockServer,
    pub keys: TestKeyPair,
    full_keys: BTreeMap<String, String>,
    delta_keys: BTreeMap<String, String>,
}

impl Release {
    /// Build patches in memory and serve the resulting release over HTTP.
    pub async fn start(
        version: &str,
        from_version: &str,
        old: &[(&str, &[u8])],
        new: &[(&str, &[u8])],
        rollback: bool,
    ) -> Result<Self, String> {
        let keys = TestKeyPair::generate();
        let mut objects: Vec<(String, Vec<u8>)> = Vec::new();
        let mut files = Map::new();
        let mut full_keys = BTreeMap::new();
        let mut delta_keys = BTreeMap::new();

        for (path, new_bytes) in new {
            let previous = old
                .iter()
                .find(|(name, _)| name == path)
                .map(|(_, bytes)| *bytes);
            let entry = match previous {
                Some(bytes) if bytes == *new_bytes => json!({
                    "sha256": sha256_bytes(new_bytes),
                    "size": new_bytes.len(),
                    "strategy": "full",
                }),
                Some(bytes) => {
                    let full_key = format!("artifacts/full/{version}/{path}");
                    objects.push((full_key.clone(), new_bytes.to_vec()));
                    full_keys.insert((*path).to_string(), full_key.clone());

                    let patch = build_patch(bytes, new_bytes);
                    let delta_key = format!("artifacts/{from_version}-to-{version}/{path}.tdp");
                    objects.push((delta_key.clone(), patch.clone()));
                    delta_keys.insert((*path).to_string(), delta_key.clone());

                    json!({
                        "sha256": sha256_bytes(new_bytes),
                        "size": new_bytes.len(),
                        "strategy": "qbsdiff+zstd",
                        "delta_key": delta_key,
                        "delta_size": patch.len(),
                        "full_key": full_key,
                    })
                }
                None => {
                    let full_key = format!("artifacts/full/{version}/{path}");
                    objects.push((full_key.clone(), new_bytes.to_vec()));
                    full_keys.insert((*path).to_string(), full_key.clone());
                    json!({
                        "sha256": sha256_bytes(new_bytes),
                        "size": new_bytes.len(),
                        "strategy": "full",
                        "full_key": full_key,
                    })
                }
            };
            files.insert((*path).to_string(), entry);
        }

        let manifest = json!({
            "schema": 1,
            "version": version,
            "channel": CHANNEL,
            "min_client_schema": 1,
            "rollback": rollback,
            "files": Value::Object(files),
        });
        let manifest =
            serde_json::to_vec(&manifest).map_err(|err| format!("serialize manifest: {err}"))?;
        let signature = keys.sign(&manifest);
        let mut served = vec![
            ("manifests/latest.json".to_string(), manifest),
            (
                "manifests/latest.json.minisig".to_string(),
                signature.into_bytes(),
            ),
        ];
        served.extend(objects);
        let server = MockServer::start(served)
            .await
            .map_err(|err| format!("mock server: {err}"))?;
        Ok(Self {
            server,
            keys,
            full_keys,
            delta_keys,
        })
    }

    pub fn full_key(&self, path: &str) -> Option<&str> {
        self.full_keys.get(path).map(String::as_str)
    }

    pub fn delta_key(&self, path: &str) -> Option<&str> {
        self.delta_keys.get(path).map(String::as_str)
    }

    pub fn requests(&self) -> Vec<String> {
        self.server.requests()
    }

    /// Fail if any of `keys` was requested at all.
    pub fn assert_not_requested(&self, keys: &[&str]) -> Result<(), String> {
        let requests = self.requests();
        for key in keys {
            if requests.iter().any(|request| request == key) {
                return Err(format!("unexpected request for {key}"));
            }
        }
        Ok(())
    }

    pub fn request_count(&self, key: &str) -> usize {
        self.requests()
            .iter()
            .filter(|request| *request == key)
            .count()
    }
}

/// Persist the journal and execute it with the in-process writer, the same
/// path the replacement worker takes.
pub fn apply(engine: &Engine, prepared: &PreparedUpdate) -> Result<PendingCommit, String> {
    let pending = engine
        .begin_commit(prepared)
        .map_err(|err| format!("begin_commit: {err}"))?;
    let status =
        delta_update_engine::apply::apply_commit(&engine.config().journal_root, &pending.commit_id)
            .map_err(|err| format!("apply journal: {err}"))?;
    match status {
        CommitStatus::Completed { .. } => Ok(pending),
        other => Err(format!("unexpected commit status: {other:?}")),
    }
}

pub fn assert_root(root: &Path, files: &[(&str, &[u8])]) -> Result<(), String> {
    for (path, expected) in files {
        let actual = std::fs::read(root.join(path)).map_err(|err| format!("read {path}: {err}"))?;
        if actual.as_slice() != *expected {
            return Err(format!("{path} content mismatch"));
        }
    }
    Ok(())
}

pub fn assert_absent(root: &Path, paths: &[&str]) -> Result<(), String> {
    for path in paths {
        if root.join(path).exists() {
            return Err(format!("{path} should have been removed"));
        }
    }
    Ok(())
}

/// Assert that commit bookkeeping, staging and temporary files were cleaned up.
pub fn assert_clean(scenario: &Scenario, version: &str) -> Result<(), String> {
    let root = scenario.root();
    for dir in [".old", ".trash"] {
        let path = root.join(dir);
        if path.exists()
            && path
                .read_dir()
                .map_err(|err| err.to_string())?
                .next()
                .is_some()
        {
            return Err(format!("{dir} still contains backups"));
        }
    }
    if let Some(path) = temp_residue(&root).map_err(|err| err.to_string())? {
        return Err(format!("temporary file left behind: {}", path.display()));
    }
    for name in [
        "commit.journal",
        "commit.status",
        "apply.json",
        "prepared.json",
    ] {
        if scenario.journal().join(name).exists() {
            return Err(format!("journal file was not cleaned: {name}"));
        }
    }
    let staging = scenario.staging().join(version);
    if staging.exists() {
        return Err(format!("staging was not cleaned: {}", staging.display()));
    }
    Ok(())
}

fn temp_residue(root: &Path) -> io::Result<Option<PathBuf>> {
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            if let Some(found) = temp_residue(&path)? {
                return Ok(Some(found));
            }
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains(".tmp-"))
        {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

/// A release produced by the real CLI pipeline and served from a local bucket.
pub struct CliRelease {
    pub server: MockServer,
    pub public_key: String,
}

/// Run `delta-update release` into a local bucket, then serve that bucket.
pub async fn publish_with_cli(
    bucket: &Path,
    key_path: &Path,
    from: &Path,
    to: &Path,
    version: &str,
    from_version: &str,
    rollback: bool,
) -> Result<CliRelease, String> {
    std::env::set_var("MINISIGN_PASSWORD", "");
    let keypair = minisign::KeyPair::generate_encrypted_keypair(Some(String::new()))
        .map_err(|err| format!("generate release key: {err}"))?;
    let secret = keypair
        .sk
        .to_box(None)
        .map_err(|err| format!("encode release key: {err}"))?;
    std::fs::write(key_path, secret.to_bytes())
        .map_err(|err| format!("write release key: {err}"))?;

    pipeline::release(&ReleaseArgs {
        to: to.to_path_buf(),
        version: version.to_string(),
        bucket: bucket.display().to_string(),
        key: key_path.to_path_buf(),
        from: Some(from.to_path_buf()),
        from_version: Some(from_version.to_string()),
        initial: false,
        rollback,
        channel: CHANNEL.into(),
        notes_file: None,
        mandatory: false,
        region: None,
        prefix: None,
        dry_run: false,
        require_delta: false,
        delta_max_ratio: 0.60,
        patch_max_file_size: 64 * 1024 * 1024,
        verify: false,
        jobs: Some(2),
        verbose: false,
    })
    .map_err(|err| format!("release: {err:#}"))?;

    let server = serve_dir(bucket).await?;
    Ok(CliRelease {
        server,
        public_key: keypair.pk.to_base64(),
    })
}

async fn serve_dir(root: &Path) -> Result<MockServer, String> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)
        .map_err(|err| format!("scan bucket {}: {err}", root.display()))?;
    MockServer::start(files)
        .await
        .map_err(|err| format!("mock server: {err}"))
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_files(root, &path, out)?;
        } else {
            let key = path
                .strip_prefix(root)
                .map_err(io::Error::other)?
                .to_string_lossy()
                .replace('\\', "/");
            out.push((key, std::fs::read(&path)?));
        }
    }
    Ok(())
}

fn build_patch(old: &[u8], new: &[u8]) -> Vec<u8> {
    let mut raw = Vec::new();
    qbsdiff::Bsdiff::new(old, new)
        .compare(&mut raw)
        .expect("generate qbsdiff patch");
    let payload = zstd::encode_all(raw.as_slice(), 3).expect("compress patch");
    let header = PatchHeader {
        format: 1,
        strategy_id: 1,
        codec: Codec::Zstd,
        target_size: new.len() as u64,
        source_sha256: hash32(old),
        target_sha256: hash32(new),
    };
    let mut patch = header.encode().to_vec();
    patch.extend_from_slice(&payload);
    patch
}

fn hash32(bytes: &[u8]) -> [u8; 32] {
    hex::decode(sha256_bytes(bytes))
        .expect("sha256 hex")
        .try_into()
        .expect("sha256 is 32 bytes")
}
