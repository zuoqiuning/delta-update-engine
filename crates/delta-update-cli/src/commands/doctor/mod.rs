//! `doctor`: online endpoint checks, offline config checks, failure simulation
//! and install-tree verification.

mod config;
mod endpoint;
mod simulate;

use std::path::Path;

use anyhow::{bail, Context};

use crate::cli::DoctorArgs;
use crate::output;
use crate::pipeline::keys::read_public_keys;
use endpoint::check_endpoint;

pub use simulate::simulated_error;

pub fn run(args: &DoctorArgs) -> anyhow::Result<()> {
    doctor(args)
}

pub fn doctor(args: &DoctorArgs) -> anyhow::Result<()> {
    if let Some(journal_dir) = &args.journal_dir {
        return verify_install_files(args, journal_dir);
    }
    if let Some(mode) = args.simulate_failure {
        return simulate::simulate_failure(args, mode);
    }
    if args.check_config {
        return config::check_plugin_config(args);
    }
    let Some(duration) = args.watch else {
        return doctor_once(args);
    };
    if args.json {
        bail!("--watch cannot be combined with --json; run doctor per interval instead");
    }
    if args.interval == 0 {
        bail!("--interval must be at least 1 second");
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(duration);
    let mut pass = 0_u32;
    loop {
        pass += 1;
        output::info(&format!("watch pass {pass}"));
        doctor_once(args)?;
        let now = std::time::Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline.duration_since(now);
        std::thread::sleep(std::time::Duration::from_secs(args.interval).min(remaining));
    }
    output::info(&format!("watch finished: {pass} pass(es)"));
    Ok(())
}

/// Verify an install tree against the newest manifest recorded in the journal.
///
/// This answers "my app files look wrong" without a network round trip: the
/// journal keeps the manifest that the last check/commit verified, so a missing
/// or altered file is detected even when the remote version has not changed
/// (a plain `check` returns "up to date" without scanning).
fn verify_install_files(args: &DoctorArgs, journal_dir: &Path) -> anyhow::Result<()> {
    use delta_update_engine::Manifest;

    let install_root = args
        .install_root
        .as_deref()
        .context("--install-root is required with --journal-dir")?;
    if !journal_dir.is_dir() {
        bail!(
            "journal directory does not exist: {}",
            journal_dir.display()
        );
    }
    if !install_root.is_dir() {
        bail!("install root does not exist: {}", install_root.display());
    }
    let (manifest_path, source) = [
        ("installed_manifest.json", "installed"),
        ("verified_manifest.json", "verified"),
    ]
    .iter()
    .map(|(name, source)| (journal_dir.join(name), *source))
    .find(|(path, _)| path.is_file())
    .with_context(|| {
        format!(
            "no installed_manifest.json or verified_manifest.json in {}",
            journal_dir.display()
        )
    })?;
    let bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest: Manifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse {}", manifest_path.display()))?;

    let mut missing: Vec<&String> = Vec::new();
    let mut mismatched = Vec::new();
    for (path, entry) in &manifest.files {
        match delta_update_engine::crypto::sha256_file(&install_root.join(path)) {
            Ok(actual) if actual == entry.sha256 => {}
            Ok(actual) => mismatched.push(serde_json::json!({
                "path": path,
                "expected": entry.sha256,
                "actual": actual,
            })),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => missing.push(path),
            Err(error) => bail!("hash {path}: {error}"),
        }
    }
    let ok = missing.is_empty() && mismatched.is_empty();
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "v": 1,
                "tool": "delta-update-cli",
                "mode": "verify-files",
                "ok": ok,
                "manifest": source,
                "version": manifest.version,
                "checked": manifest.files.len(),
                "missing": missing,
                "mismatched": mismatched,
            }))?
        );
    } else {
        output::info(&format!(
            "校验 {source} 清单（v{}）：{} 个文件",
            manifest.version,
            manifest.files.len()
        ));
        if source == "verified" {
            output::warn("使用的是验证清单；若替换尚未完成，旧版本的文件会整体显示为不一致");
        }
        for path in &missing {
            output::warn(&format!("缺失: {path}"));
        }
        for item in &mismatched {
            output::warn(&format!(
                "不一致: {}",
                item["path"].as_str().unwrap_or_default()
            ));
        }
        if ok {
            output::info("安装树与清单一致");
        }
    }
    if !ok {
        bail!(
            "{} file(s) missing, {} mismatched against the {source} manifest",
            missing.len(),
            mismatched.len()
        );
    }
    Ok(())
}

fn doctor_once(args: &DoctorArgs) -> anyhow::Result<()> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .context("build doctor http client")?;
    let keys = match &args.public_key {
        Some(path) => Some(read_public_keys(path)?),
        None => None,
    };

    let mut reports = Vec::new();
    for endpoint in &args.endpoint {
        reports.push(check_endpoint(&client, endpoint, keys.as_deref()));
    }
    let versions: std::collections::BTreeSet<&str> = reports
        .iter()
        .filter_map(|report| report.version.as_deref())
        .collect();
    let consistent = versions.len() <= 1;
    let all_ok = reports.iter().all(|report| report.ok) && consistent;

    if args.json {
        let value = serde_json::json!({
            "v": 1,
            "tool": "delta-update-cli",
            "mode": "endpoints",
            "ok": all_ok,
            "consistent": consistent,
            "versions": versions,
            "endpoints": reports.iter().map(|report| serde_json::json!({
                "endpoint": report.endpoint,
                "ok": report.ok,
                "version": report.version,
                "verified": report.verified,
                "errors": report.errors,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        for report in &reports {
            output::info(&format!(
                "{}: {}",
                report.endpoint,
                report.version.as_deref().unwrap_or("no manifest")
            ));
            for error in &report.errors {
                output::warn(error);
            }
            if !report.verified {
                output::warn(&format!(
                    "{}: signature not verified (pass --public-key)",
                    report.endpoint
                ));
            }
        }
        if !consistent {
            output::error("endpoints expose different versions");
        }
    }

    if !all_ok {
        bail!("doctor found problems");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn doctor_args(journal_dir: Option<PathBuf>, install_root: Option<PathBuf>) -> DoctorArgs {
        DoctorArgs {
            endpoint: Vec::new(),
            public_key: None,
            check_config: false,
            journal_dir,
            install_root,
            simulate_failure: None,
            reason: None,
            dir: PathBuf::from("."),
            watch: None,
            interval: 60,
            json: true,
        }
    }

    #[test]
    fn verify_files_finds_missing_and_mismatched_entries() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("journal");
        let root = dir.path().join("root");
        std::fs::create_dir_all(&journal).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("good.txt"), b"good").unwrap();
        std::fs::write(root.join("bad.txt"), b"evil").unwrap();

        let manifest = serde_json::json!({
            "schema": 1,
            "version": "2.0.0",
            "channel": "stable",
            "min_client_schema": 1,
            "files": {
                "good.txt": {
                    "sha256": delta_update_engine::crypto::sha256_bytes(b"good"),
                    "size": 4,
                    "strategy": "full",
                    "full_key": "blobs/good"
                },
                "bad.txt": {
                    "sha256": delta_update_engine::crypto::sha256_bytes(b"orig"),
                    "size": 4,
                    "strategy": "full",
                    "full_key": "blobs/bad"
                },
                "gone.txt": {
                    "sha256": delta_update_engine::crypto::sha256_bytes(b"gone"),
                    "size": 4,
                    "strategy": "full",
                    "full_key": "blobs/gone"
                }
            }
        });
        std::fs::write(
            journal.join("installed_manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();

        let args = doctor_args(Some(journal.clone()), Some(root.clone()));
        let error = verify_install_files(&args, &journal).unwrap_err();
        assert!(
            error.to_string().contains("1 file(s) missing")
                && error.to_string().contains("1 mismatched"),
            "unexpected error: {error}"
        );

        // A tree that matches the manifest passes.
        std::fs::write(root.join("bad.txt"), b"orig").unwrap();
        std::fs::write(root.join("gone.txt"), b"gone").unwrap();
        verify_install_files(&args, &journal).unwrap();
    }
}
