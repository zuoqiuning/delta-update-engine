use std::net::TcpListener;

use delta_update_testkit::tree;
use url::Url;

use crate::harness::{self, Scenario};

const OLD_VERSION: &str = "1.0.0";
const NEW_VERSION: &str = "2.0.0";

/// Build a release with the real CLI, serve the bucket with `serve`, then run
/// the client path against it. This covers the "5 minute demo" chain without
/// PowerShell: release -> serve -> check -> prepare -> apply -> confirm.
pub fn run() -> Result<(), String> {
    harness::block_on(run_inner())
}

async fn run_inner() -> Result<(), String> {
    let app_old = harness::payload(b'a', 64 * 1024);
    let app_new = harness::tweak(&app_old, 1024, b"served-v2");
    let old: Vec<(&str, &[u8])> = vec![("App.exe", app_old.as_slice())];
    let new: Vec<(&str, &[u8])> = vec![("App.exe", app_new.as_slice())];

    let build = tempfile::tempdir().map_err(|err| format!("build dir: {err}"))?;
    let from = build.path().join("from");
    let to = build.path().join("to");
    tree::write_tree(&from, &old).map_err(|err| format!("write from tree: {err}"))?;
    tree::write_tree(&to, &new).map_err(|err| format!("write to tree: {err}"))?;
    let bucket = build.path().join("bucket");
    let key = build.path().join("release.key");
    let release =
        harness::publish_with_cli(&bucket, &key, &from, &to, NEW_VERSION, OLD_VERSION, false)
            .await?;

    let root =
        std::fs::canonicalize(&bucket).map_err(|err| format!("canonicalize bucket: {err}"))?;
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|err| format!("bind serve: {err}"))?;
    let address = listener
        .local_addr()
        .map_err(|err| format!("serve address: {err}"))?;
    std::thread::spawn(move || delta_update_cli::commands::serve::serve_on(listener, root));

    let scenario = Scenario::new().map_err(|err| format!("scenario: {err}"))?;
    scenario.write_root(&old)?;
    let endpoint = Url::parse(&format!("http://{address}/manifests/latest.json"))
        .map_err(|err| format!("endpoint: {err}"))?;
    let engine = scenario.engine_with_endpoint(endpoint, &release.public_key, OLD_VERSION)?;

    let check = engine
        .check()
        .await
        .map_err(|err| format!("check: {err}"))?;
    if !check.available || check.version.as_deref() != Some(NEW_VERSION) {
        return Err(format!("serve did not offer {NEW_VERSION}: {check:?}"));
    }
    if check.download_bytes >= check.total_bytes {
        return Err(format!(
            "patch was not smaller than the full object: {} of {}",
            check.download_bytes, check.total_bytes
        ));
    }

    let started = std::time::Instant::now();
    let prepared = engine
        .prepare(Box::new(|_| {}))
        .await
        .map_err(|err| format!("prepare: {err}"))?;
    println!("http_serve: prepare took {:?}", started.elapsed());
    harness::apply(&engine, &prepared)?;
    harness::assert_root(&scenario.root(), &new)?;
    engine
        .confirm_healthy()
        .map_err(|err| format!("confirm: {err}"))?;
    harness::assert_clean(&scenario, NEW_VERSION)?;
    println!("http_serve: release -> serve -> check/prepare/apply/confirm ok");
    Ok(())
}
