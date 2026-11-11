use delta_update_engine::StartupOutcome;

use crate::harness::{self, Release, Scenario};

const OLD_VERSION: &str = "1.0.0";
const NEW_VERSION: &str = "2.0.0";

pub fn run() -> Result<(), String> {
    harness::block_on(run_inner())
}

async fn run_inner() -> Result<(), String> {
    let app_old = harness::payload(b'a', 40 * 1024);
    let app_new = harness::tweak(&app_old, 2048, b"elevated-build");
    let old: Vec<(&str, &[u8])> = vec![("App.exe", app_old.as_slice())];
    let new: Vec<(&str, &[u8])> = vec![("App.exe", app_new.as_slice())];

    let scenario = Scenario::new().map_err(|err| format!("scenario: {err}"))?;
    scenario.write_root(&old)?;
    let release = Release::start(NEW_VERSION, OLD_VERSION, &old, &new, false).await?;
    let engine = scenario.engine(&release.server, &release.keys, OLD_VERSION)?;

    engine
        .check()
        .await
        .map_err(|err| format!("check: {err}"))?;
    let prepared = engine
        .prepare(Box::new(|_| {}))
        .await
        .map_err(|err| format!("prepare: {err}"))?;

    // The UAC prompt is declined: the worker never starts, so the plugin
    // withdraws the commit point and keeps the PREPARED plan for next time.
    engine
        .begin_commit(&prepared)
        .map_err(|err| format!("begin_commit: {err}"))?;
    engine
        .discard_pending_commit()
        .map_err(|err| format!("discard commit: {err}"))?;
    harness::assert_root(&scenario.root(), &old)?;
    for name in ["commit.journal", "apply.json"] {
        if scenario.journal().join(name).exists() {
            return Err(format!("{name} should be withdrawn after a denied prompt"));
        }
    }
    if !scenario.journal().join("prepared.json").exists() {
        return Err("prepared.json must survive a denied elevation".into());
    }

    // The next start reuses the prepared plan instead of replaying a journal.
    let retried = scenario.engine(&release.server, &release.keys, OLD_VERSION)?;
    match retried
        .recover_and_cleanup()
        .map_err(|err| format!("recover: {err}"))?
    {
        StartupOutcome::ReusedPrepared { version } if version == NEW_VERSION => {}
        other => return Err(format!("prepared plan was not reused: {other:?}")),
    }

    // The next exit retries the elevated commit and succeeds.
    let resumed = retried
        .resume_prepared()
        .map_err(|err| format!("resume: {err}"))?;
    harness::apply(&retried, &resumed)?;
    harness::assert_root(&scenario.root(), &new)?;

    // The first start of the new binary then confirms health and cleans up.
    let restarted = scenario.engine(&release.server, &release.keys, NEW_VERSION)?;
    match restarted
        .recover_and_cleanup()
        .map_err(|err| format!("start new version: {err}"))?
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
