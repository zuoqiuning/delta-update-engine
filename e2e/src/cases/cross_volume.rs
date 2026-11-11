use std::path::{Component, Path};

use delta_update_engine::StartupOutcome;

use crate::harness::{self, Release, Scenario};

const OLD_VERSION: &str = "1.0.0";
const NEW_VERSION: &str = "2.0.0";

pub fn run() -> Result<(), String> {
    harness::block_on(run_inner())
}

async fn run_inner() -> Result<(), String> {
    let app_old = harness::payload(b'a', 48 * 1024);
    let app_new = harness::tweak(&app_old, 3072, b"cross-volume");
    let asset_old = harness::payload(b'z', 24 * 1024);
    let asset_new = harness::tweak(&asset_old, 128, b"asset-v2");
    let added = b"new resource".to_vec();

    let old: Vec<(&str, &[u8])> = vec![
        ("App.exe", app_old.as_slice()),
        ("resources/asset.bin", asset_old.as_slice()),
    ];
    let new: Vec<(&str, &[u8])> = vec![
        ("App.exe", app_new.as_slice()),
        ("resources/asset.bin", asset_new.as_slice()),
        ("resources/added.bin", added.as_slice()),
    ];

    let scenario = Scenario::cross_volume().map_err(|err| format!("scenario: {err}"))?;
    let root_volume = volume(&scenario.root());
    let staging_volume = volume(&scenario.staging());
    println!(
        "cross_volume: root {} ({root_volume:?}) -> staging {} ({staging_volume:?})",
        scenario.root().display(),
        scenario.staging().display()
    );
    if root_volume.is_some() && root_volume == staging_volume {
        println!("cross_volume: both roots share a volume; copy fallback is not exercised");
    }

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

fn volume(path: &Path) -> Option<String> {
    match path.components().next()? {
        Component::Prefix(prefix) => Some(prefix.as_os_str().to_string_lossy().into_owned()),
        _ => None,
    }
}
