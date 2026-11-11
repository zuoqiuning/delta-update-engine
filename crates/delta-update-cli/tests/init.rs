use assert_cmd::Command;

const KEY: &str = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";

#[test]
fn prints_config_and_capability_snippets() {
    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args([
            "init",
            "--endpoint",
            "https://example.com/manifests/latest.json",
            "--public-key",
            KEY,
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("https://example.com/manifests/latest.json"));
    assert!(text.contains(KEY));
    assert!(text.contains("delta-update:default"));
    assert!(text.contains("run_applier_from_args"));
}

#[test]
fn rejects_invalid_public_keys() {
    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args([
            "init",
            "--endpoint",
            "https://example.com/latest.json",
            "--public-key",
            "not-a-key",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
}

#[test]
fn reuses_existing_delta_config_as_defaults() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src-tauri")).unwrap();
    std::fs::write(
        dir.path().join("src-tauri/tauri.conf.json"),
        format!(
            r#"{{"plugins":{{"delta-update":{{"endpoint":"https://example.com/manifests/latest.json","signing_public_keys":["{KEY}"]}}}}}}"#
        ),
    )
    .unwrap();

    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args(["init", "--dir"])
        .arg(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("https://example.com/manifests/latest.json"));
    assert!(text.contains(KEY));
    assert!(text.contains("已从"));
}

#[test]
fn falls_back_to_the_official_updater_pubkey() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src-tauri")).unwrap();
    std::fs::write(
        dir.path().join("src-tauri/tauri.conf.json"),
        format!(r#"{{"plugins":{{"updater":{{"pubkey":"{KEY}"}}}}}}"#),
    )
    .unwrap();

    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args(["init", "--dir"])
        .arg(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains(KEY));
    assert!(text.contains("官方 updater 公钥"));
    assert!(
        text.contains("占位值"),
        "endpoint still needs an explicit value"
    );
}
