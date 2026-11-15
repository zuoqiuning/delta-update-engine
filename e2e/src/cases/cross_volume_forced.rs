use std::io;

use delta_update_engine::StartupOutcome;
use delta_update_testkit::FaultyFs;

use crate::harness::{self, Release, Scenario};

const OLD_VERSION: &str = "1.0.0";
const NEW_VERSION: &str = "2.0.0";

/// Force the cross-volume path on a single-volume machine: the rename that
/// places `App.exe` reports `CrossesDevices` once, so the update can only finish
/// through the copy + fsync + atomic rename fallback.
///
/// `cross_volume` skips the fallback silently when both roots share a volume;
/// this case always exercises it.
pub fn run() -> Result<(), String> {
    harness::block_on(run_inner())
}

async fn run_inner() -> Result<(), String> {
    let app_old = harness::payload(b'a', 48 * 1024);
    let app_new = harness::tweak(&app_old, 4096, b"forced cross-volume");
    let asset_old = harness::payload(b'z', 16 * 1024);
    let asset_new = harness::tweak(&asset_old, 64, b"asset-v2");

    let old: Vec<(&str, &[u8])> = vec![
        ("App.exe", app_old.as_slice()),
        ("resources/asset.bin", asset_old.as_slice()),
    ];
    let new: Vec<(&str, &[u8])> = vec![
        ("App.exe", app_new.as_slice()),
        ("resources/asset.bin", asset_new.as_slice()),
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

    let target = scenario.root().join("App.exe");
    println!(
        "cross_volume_forced: injecting CrossesDevices on {}",
        target.display()
    );
    let fs = FaultyFs::new().fail_on_rename(&target, io::ErrorKind::CrossesDevices);
    harness::apply_with_fs(&engine, &prepared, &fs)?;
    if !fs.fired() {
        return Err(format!(
            "the injected CrossesDevices fault never matched {}; the copy fallback was not exercised",
            target.display()
        ));
    }
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
