use std::time::Instant;

use delta_update_engine::StartupOutcome;

use crate::harness::{self, Release, Scenario};

const OLD_VERSION: &str = "1.0.0";
const NEW_VERSION: &str = "2.0.0";

pub fn run() -> Result<(), String> {
    harness::block_on(run_inner())
}

async fn run_inner() -> Result<(), String> {
    let app_old = harness::payload(b'a', 96 * 1024);
    let app_new = harness::tweak(&app_old, 2048, b"app-v2");
    let core_old = harness::payload(b'b', 48 * 1024);
    let core_new = harness::tweak(&core_old, 4096, b"core-v2");
    let logo = b"binary asset".to_vec();
    let legacy = b"legacy file".to_vec();
    let readme = b"new documentation".to_vec();

    let old: Vec<(&str, &[u8])> = vec![
        ("App.exe", app_old.as_slice()),
        ("bin/core.dll", core_old.as_slice()),
        ("assets/logo.bin", logo.as_slice()),
        ("legacy.txt", legacy.as_slice()),
    ];
    let new: Vec<(&str, &[u8])> = vec![
        ("App.exe", app_new.as_slice()),
        ("bin/core.dll", core_new.as_slice()),
        ("assets/logo.bin", logo.as_slice()),
        ("docs/readme.txt", readme.as_slice()),
    ];

    let scenario = Scenario::new().map_err(|err| format!("scenario: {err}"))?;
    scenario.write_root(&old)?;
    scenario.seed_known(&["legacy.txt"])?;
    let release = Release::start(NEW_VERSION, OLD_VERSION, &old, &new, false).await?;

    // E2E-02: an up-to-date client downloads nothing but the manifest pair.
    let up_to_date = scenario.engine(&release.server, &release.keys, NEW_VERSION)?;
    let outcome = up_to_date
        .check()
        .await
        .map_err(|err| format!("check current version: {err}"))?;
    if outcome.available {
        return Err("an update was offered to an up-to-date client".into());
    }
    let requests = release.requests();
    if requests.len() != 2
        || requests.iter().any(|request| {
            request != "manifests/latest.json" && request != "manifests/latest.json.minisig"
        })
    {
        return Err(format!(
            "unexpected requests for a no-op check: {requests:?}"
        ));
    }

    let engine = scenario.engine(&release.server, &release.keys, OLD_VERSION)?;
    let app_delta = release
        .delta_key("App.exe")
        .ok_or("App.exe should have a delta key")?
        .to_string();
    // E2E-03: one transport failure is retried, not routed to the full object.
    release.server.fail_next(&app_delta, 1);

    let check = engine
        .check()
        .await
        .map_err(|err| format!("check: {err}"))?;
    if !check.available || check.files_changed != 4 {
        return Err(format!("unexpected check outcome: {check:?}"));
    }
    if check.download_bytes * 2 >= check.total_bytes {
        return Err(format!(
            "patch ratio baseline failed: {} of {} bytes",
            check.download_bytes, check.total_bytes
        ));
    }

    let started = Instant::now();
    let prepared = engine
        .prepare(Box::new(|_| {}))
        .await
        .map_err(|err| format!("prepare: {err}"))?;
    println!(
        "delta_update: prepare took {:?}, delta {} / full {} bytes",
        started.elapsed(),
        check.download_bytes,
        check.total_bytes
    );
    harness::assert_root(&scenario.root(), &old)?;

    let app_full = release
        .full_key("App.exe")
        .ok_or("App.exe should have a full key")?;
    let core_full = release
        .full_key("bin/core.dll")
        .ok_or("bin/core.dll should have a full key")?;
    release.assert_not_requested(&[app_full, core_full])?;
    if release.request_count(&app_delta) < 2 {
        return Err("the failed delta download was not retried".into());
    }
    let staged = prepared.staging_root.join("files");
    let staged_core = std::fs::read(staged.join("bin/core.dll"))
        .map_err(|err| format!("read staged core: {err}"))?;
    if staged_core != core_new {
        return Err("staged bin/core.dll does not match the target".into());
    }
    if staged.join("assets/logo.bin").exists() {
        return Err("unchanged files must not be staged".into());
    }

    harness::apply(&engine, &prepared)?;
    harness::assert_root(&scenario.root(), &new)?;
    harness::assert_absent(&scenario.root(), &["legacy.txt"])?;

    let restarted = scenario.engine(&release.server, &release.keys, NEW_VERSION)?;
    match restarted
        .recover_and_cleanup()
        .map_err(|err| format!("recover: {err}"))?
    {
        StartupOutcome::Committed { version } if version == NEW_VERSION => {}
        other => return Err(format!("unexpected startup outcome: {other:?}")),
    }
    restarted
        .confirm_healthy()
        .map_err(|err| format!("confirm: {err}"))?;
    harness::assert_clean(&scenario, NEW_VERSION)?;

    let known: serde_json::Value = serde_json::from_slice(
        &std::fs::read(scenario.journal().join("known_files.json"))
            .map_err(|err| format!("read known files: {err}"))?,
    )
    .map_err(|err| format!("parse known files: {err}"))?;
    let expected = serde_json::json!([
        "App.exe",
        "assets/logo.bin",
        "bin/core.dll",
        "docs/readme.txt"
    ]);
    if known["files"] != expected {
        return Err(format!("known files were not updated: {known}"));
    }
    Ok(())
}
