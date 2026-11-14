//! Real end-to-end demo client for the delta update engine.
//!
//! The same binary plays three roles:
//! - normal app: prints the version stored in `version.txt`
//! - update client: check -> prepare -> write journal -> spawn worker
//! - worker: `--delta-update-apply` replaces files after the parent exits

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use delta_update_engine::fetch::{FetchLimits, HttpFetcher};
use delta_update_engine::{Engine, EngineConfig, Limits};
use serde::Deserialize;

#[derive(Deserialize)]
struct DemoConfig {
    endpoint: String,
    signing_public_key: String,
}

fn main() {
    // Must run before anything else: the worker does not load the demo logic.
    if let Some(code) = worker_entry() {
        std::process::exit(code);
    }

    if let Err(error) = run() {
        eprintln!("demo-app: {error:#}");
        std::process::exit(1);
    }
}

/// The real plugin reads keys from the embedded Tauri config. The demo keeps
/// them next to the executable and refuses to run the worker without one.
fn worker_entry() -> Option<i32> {
    let mode = std::env::args_os().nth(1)?;
    if mode != "--delta-update-apply" && mode != "--delta-update-recover" {
        return None;
    }
    let keys = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|root| root.join("demo-config.json")))
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|raw| serde_json::from_slice::<DemoConfig>(&raw).ok())
        .map(|config| vec![config.signing_public_key])
        .unwrap_or_default();
    if keys.is_empty() {
        eprintln!("demo-app worker: demo-config.json has no signing_public_key");
        return Some(1);
    }
    delta_update_engine::apply::run_applier_from_args_with_keys(&keys)
}

fn run() -> Result<()> {
    let exe = std::env::current_exe()?;
    let root = exe
        .parent()
        .context("demo-app has no parent directory")?
        .to_path_buf();
    let command = std::env::args().nth(1).unwrap_or_else(|| "version".into());

    match command.as_str() {
        "version" => {
            println!("Demo App v{}", read_version(&root)?);
            Ok(())
        }
        "update" => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(update(&root))
        }
        "confirm" => confirm(&root),
        "status" => status(&root),
        other => bail!("unknown command {other}; use version | update | confirm | status"),
    }
}

async fn update(root: &Path) -> Result<()> {
    let version = read_version(root)?;
    let engine = build_engine(root, &version)?;

    let outcome = engine.check().await?;
    if !outcome.available {
        println!("already up to date: v{}", outcome.current_version);
        return Ok(());
    }
    println!(
        "remote v{}: {} files, download {} bytes (full {} bytes)",
        outcome.version.as_deref().unwrap_or("?"),
        outcome.files_changed,
        outcome.download_bytes,
        outcome.total_bytes
    );

    let prepared = engine
        .prepare(Box::new(|_| {}))
        .await
        .context("prepare update")?;
    println!(
        "staged v{} in {}",
        prepared.version,
        prepared.staging_root.display()
    );

    let pending = engine.begin_commit(&prepared).context("write journal")?;
    let exe = std::env::current_exe()?;
    Command::new(&exe)
        .arg("--delta-update-apply")
        .arg(engine.config().journal_root.display().to_string())
        .arg(&pending.commit_id)
        .spawn()
        .context("spawn update worker")?;
    println!("worker spawned; demo-app exits so files can be replaced");
    Ok(())
}

fn confirm(root: &Path) -> Result<()> {
    let version = read_version(root)?;
    let engine = build_engine(root, &version)?;
    engine.confirm_healthy().context("confirm healthy")?;
    println!("confirmed v{version}; journal and backups cleaned");
    Ok(())
}

fn status(root: &Path) -> Result<()> {
    let version = read_version(root)?;
    let state = state_root(root);
    println!("installed version: v{version}");
    println!("install root:      {}", root.display());
    println!("state root:        {}", state.display());
    if let Ok(entries) = std::fs::read_dir(state.join("journal")) {
        for entry in entries.flatten() {
            println!("journal: {}", entry.file_name().to_string_lossy());
        }
    }
    Ok(())
}

fn build_engine(root: &Path, version: &str) -> Result<Engine> {
    let config_path = root.join("demo-config.json");
    let config: DemoConfig = serde_json::from_slice(
        &std::fs::read(&config_path).with_context(|| format!("read {}", config_path.display()))?,
    )
    .context("parse demo-config.json")?;
    let endpoint: url::Url = config
        .endpoint
        .parse()
        .with_context(|| format!("invalid endpoint {}", config.endpoint))?;
    let state = state_root(root);

    let fetcher =
        HttpFetcher::new_default(endpoint.clone(), None, FetchLimits::default(), None, true)?;

    Engine::new(EngineConfig {
        endpoint,
        signing_public_keys: vec![config.signing_public_key],
        channel: "stable".into(),
        current_version: version.to_string(),
        install_root: root.to_path_buf(),
        staging_root: state.join("staging"),
        journal_root: state.join("journal"),
        limits: Limits::default(),
        fetcher: Arc::new(fetcher),
        allow_insecure_http: true,
        crash_loop_threshold: 2,
        allow_unsigned_journal_recovery: false,
    })
    .context("build engine")
}

fn state_root(root: &Path) -> PathBuf {
    root.parent()
        .expect("install root always has a parent")
        .join("state")
}

fn read_version(root: &Path) -> Result<String> {
    let path = root.join("version.txt");
    let value =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    Ok(value.trim().to_string())
}
