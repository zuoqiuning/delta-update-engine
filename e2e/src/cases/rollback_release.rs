use delta_update_engine::StartupOutcome;
use delta_update_testkit::tree;

use crate::harness::{self, Scenario};

const CURRENT_VERSION: &str = "2.0.0";
const TARGET_VERSION: &str = "1.5.0";

pub fn run() -> Result<(), String> {
    harness::block_on(run_inner())
}

async fn run_inner() -> Result<(), String> {
    let app_current = harness::payload(b'a', 80 * 1024);
    let app_target = harness::tweak(&app_current, 4096, b"rollback");
    let font = b"shared font".to_vec();

    let current: Vec<(&str, &[u8])> = vec![
        ("App.exe", app_current.as_slice()),
        ("resources/font.bin", font.as_slice()),
    ];
    let target: Vec<(&str, &[u8])> = vec![
        ("App.exe", app_target.as_slice()),
        ("resources/font.bin", font.as_slice()),
    ];

    let build = tempfile::tempdir().map_err(|err| format!("build dir: {err}"))?;
    let from = build.path().join("from");
    let to = build.path().join("to");
    tree::write_tree(&from, &current).map_err(|err| format!("write from tree: {err}"))?;
    tree::write_tree(&to, &target).map_err(|err| format!("write to tree: {err}"))?;
    let bucket = build.path().join("bucket");
    let key = build.path().join("release.key");
    let release = harness::publish_with_cli(
        &bucket,
        &key,
        &from,
        &to,
        TARGET_VERSION,
        CURRENT_VERSION,
        true,
    )
    .await?;

    let scenario = Scenario::new().map_err(|err| format!("scenario: {err}"))?;
    scenario.write_root(&current)?;
    let engine = scenario.engine_with_key(&release.server, &release.public_key, CURRENT_VERSION)?;

    let check = engine
        .check()
        .await
        .map_err(|err| format!("check: {err}"))?;
    if !check.available || check.version.as_deref() != Some(TARGET_VERSION) {
        return Err(format!("rollback release was not accepted: {check:?}"));
    }
    if check.files_changed != 1 {
        return Err(format!(
            "expected 1 changed file, got {}",
            check.files_changed
        ));
    }

    let prepared = engine
        .prepare(Box::new(|_| {}))
        .await
        .map_err(|err| format!("prepare: {err}"))?;
    let delta = format!("artifacts/{CURRENT_VERSION}-to-{TARGET_VERSION}/App.exe.tdp");
    let full = format!("artifacts/full/{TARGET_VERSION}/App.exe");
    let requests = release.server.requests();
    if !requests.iter().any(|request| request == &delta) {
        return Err(format!("rollback delta was not downloaded: {requests:?}"));
    }
    if requests.iter().any(|request| request == &full) {
        return Err("rollback fell back to the full object".into());
    }

    harness::apply(&engine, &prepared)?;
    harness::assert_root(&scenario.root(), &target)?;

    let restarted =
        scenario.engine_with_key(&release.server, &release.public_key, TARGET_VERSION)?;
    match restarted
        .recover_and_cleanup()
        .map_err(|err| format!("recover: {err}"))?
    {
        StartupOutcome::Committed { version } if version == TARGET_VERSION => {}
        other => return Err(format!("unexpected startup outcome: {other:?}")),
    }
    restarted
        .confirm_healthy()
        .map_err(|err| format!("confirm: {err}"))?;
    harness::assert_clean(&scenario, TARGET_VERSION)?;
    if scenario.journal().join("skipped_version").exists() {
        return Err("an accepted rollback must not leave a skip marker".into());
    }
    Ok(())
}
