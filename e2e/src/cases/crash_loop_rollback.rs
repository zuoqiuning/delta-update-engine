use delta_update_engine::journal::state;
use delta_update_engine::StartupOutcome;

use crate::harness::{self, Release, Scenario};

const OLD_VERSION: &str = "1.0.0";
const NEW_VERSION: &str = "2.0.0";

pub fn run() -> Result<(), String> {
    harness::block_on(run_inner())
}

async fn run_inner() -> Result<(), String> {
    let app_old = harness::payload(b'a', 48 * 1024);
    let app_new = harness::tweak(&app_old, 1024, b"crashing-build");
    let added = b"new dependency".to_vec();

    let old: Vec<(&str, &[u8])> = vec![("App.exe", app_old.as_slice())];
    let new: Vec<(&str, &[u8])> = vec![
        ("App.exe", app_new.as_slice()),
        ("lib/new.dll", added.as_slice()),
    ];

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
    harness::apply(&engine, &prepared)?;
    harness::assert_root(&scenario.root(), &new)?;

    let restarted = scenario.engine(&release.server, &release.keys, NEW_VERSION)?;
    for start in 1..=2 {
        match restarted
            .recover_and_cleanup()
            .map_err(|err| format!("recover {start}: {err}"))?
        {
            StartupOutcome::Committed { version } if version == NEW_VERSION => {}
            other => {
                return Err(format!(
                    "start {start} was not a committed start: {other:?}"
                ))
            }
        }
    }
    match restarted
        .recover_and_cleanup()
        .map_err(|err| format!("recover 3: {err}"))?
    {
        StartupOutcome::RolledBack { to_version, .. } if to_version == OLD_VERSION => {}
        other => {
            return Err(format!(
                "third unconfirmed start did not roll back: {other:?}"
            ))
        }
    }

    harness::assert_root(&scenario.root(), &old)?;
    harness::assert_absent(&scenario.root(), &["lib/new.dll"])?;
    if state::read_skipped_version(&scenario.journal()).as_deref() != Some(NEW_VERSION) {
        return Err("the rolled back version was not skipped".into());
    }
    if state::read_attempts(&scenario.journal()) != 0 {
        return Err("crash-loop attempts were not reset".into());
    }
    if scenario.staging().join(NEW_VERSION).exists() {
        return Err("staging was not cleaned after rollback".into());
    }

    // The old version stays usable and the skipped build is not offered again.
    let old_engine = scenario.engine(&release.server, &release.keys, OLD_VERSION)?;
    let outcome = old_engine
        .check()
        .await
        .map_err(|err| format!("check after rollback: {err}"))?;
    if outcome.available {
        return Err("the skipped version was offered again".into());
    }
    Ok(())
}
