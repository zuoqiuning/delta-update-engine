use std::time::Instant;

use delta_update_engine::StartupOutcome;

use crate::harness::{self, Release, Scenario};

const OLD_VERSION: &str = "1.0.0";
const NEW_VERSION: &str = "2.0.0";

pub fn run() -> Result<(), String> {
    harness::block_on(run_inner())
}

async fn run_inner() -> Result<(), String> {
    let app_old = harness::payload(b'a', 64 * 1024);
    let app_new = harness::tweak(&app_old, 1024, b"version-two");
    let core_old = harness::payload(b'c', 32 * 1024);
    let core_new = harness::tweak(&core_old, 512, b"patched-core");
    let keep = b"unchanged asset".to_vec();
    let added = b"release notes".to_vec();

    let old: Vec<(&str, &[u8])> = vec![
        ("App.exe", app_old.as_slice()),
        ("bin/core.dll", core_old.as_slice()),
        ("assets/keep.bin", keep.as_slice()),
    ];
    let new: Vec<(&str, &[u8])> = vec![
        ("App.exe", app_new.as_slice()),
        ("bin/core.dll", core_new.as_slice()),
        ("assets/keep.bin", keep.as_slice()),
        ("docs/release.txt", added.as_slice()),
    ];

    let scenario = Scenario::new().map_err(|err| format!("scenario: {err}"))?;
    scenario.write_root(&old)?;
    let release = Release::start(NEW_VERSION, OLD_VERSION, &old, &new, false).await?;
    let engine = scenario.engine(&release.server, &release.keys, OLD_VERSION)?;

    let check = engine
        .check()
        .await
        .map_err(|err| format!("check: {err}"))?;
    if !check.available || check.version.as_deref() != Some(NEW_VERSION) {
        return Err(format!("update was not offered: {check:?}"));
    }
    if check.files_changed != 3 {
        return Err(format!(
            "expected 3 changed files, got {}",
            check.files_changed
        ));
    }

    let started = Instant::now();
    let prepared = engine
        .prepare(Box::new(|_| {}))
        .await
        .map_err(|err| format!("prepare: {err}"))?;
    println!("fresh_install: prepare took {:?}", started.elapsed());
    harness::assert_root(&scenario.root(), &old)?;

    let app_full = release
        .full_key("App.exe")
        .ok_or("App.exe should have a full key")?;
    release.assert_not_requested(&[app_full])?;
    let app_delta = release
        .delta_key("App.exe")
        .ok_or("App.exe should have a delta key")?;
    if release.request_count(app_delta) == 0 {
        return Err("the delta object was never downloaded".into());
    }

    let staged = prepared.staging_root.join("files");
    let staged_app =
        std::fs::read(staged.join("App.exe")).map_err(|err| format!("read staged app: {err}"))?;
    if staged_app != app_new {
        return Err("staged App.exe does not match the target".into());
    }
    if staged.join("assets/keep.bin").exists() {
        return Err("unchanged files must not be staged".into());
    }

    harness::apply(&engine, &prepared)?;
    harness::assert_root(&scenario.root(), &new)?;

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
    Ok(())
}
