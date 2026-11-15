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
    assert!(text.contains("delta-update:manage"));
    assert!(text.contains("run_with_delta_update"));
}

#[test]
fn migrate_write_creates_new_files_without_touching_config() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src-tauri")).unwrap();
    let conf = dir.path().join("src-tauri/tauri.conf.json");
    std::fs::write(
        &conf,
        format!(
            r#"{{"plugins":{{"updater":{{"endpoints":["https://example.com/latest.json"],"pubkey":"{KEY}"}}}}}}"#
        ),
    )
    .unwrap();
    let before = std::fs::read(&conf).unwrap();

    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args(["migrate", "--write", "--dir"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    let capability = dir.path().join("src-tauri/capabilities/delta-update.json");
    assert!(capability.is_file());
    let capability: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&capability).unwrap()).unwrap();
    assert!(capability["permissions"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("delta-update:manage")));

    let guide = dir.path().join("delta-update-migration.md");
    let text = std::fs::read_to_string(&guide).unwrap();
    assert!(text.contains("run_with_delta_update"), "{text}");
    assert!(text.contains(KEY), "{text}");

    assert_eq!(
        std::fs::read(&conf).unwrap(),
        before,
        "migrate --write must not touch tauri.conf.json"
    );

    // A second run never overwrites what the user may have edited.
    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args(["migrate", "--write", "--dir"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("未覆盖"));
}

#[test]
fn migrate_prints_a_plan_from_the_official_updater() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src-tauri")).unwrap();
    std::fs::write(
        dir.path().join("src-tauri/tauri.conf.json"),
        format!(
            r#"{{"plugins":{{"updater":{{"endpoints":["https://example.com/latest.json"],"pubkey":"{KEY}"}}}}}}"#
        ),
    )
    .unwrap();

    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args(["migrate", "--dir"])
        .arg(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    let warnings = String::from_utf8_lossy(&output.stderr);
    assert!(text.contains("https://example.com/latest.json"), "{text}");
    assert!(text.contains(KEY), "{text}");
    assert!(warnings.contains("不能复用"), "{warnings}");
    assert!(text.contains("tauri-plugin-updater"), "{text}");
    assert!(text.contains("run_with_delta_update"), "{text}");
}

#[test]
fn key_info_prints_the_public_key_and_id() {
    let dir = tempfile::tempdir().unwrap();
    let keypair = minisign::KeyPair::generate_encrypted_keypair(Some(String::new())).unwrap();
    let key_path = dir.path().join("release.key");
    std::fs::write(&key_path, keypair.sk.to_box(None).unwrap().to_bytes()).unwrap();

    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args(["key-info", "--key"])
        .arg(&key_path)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains(&keypair.pk.to_base64()), "{text}");
    assert!(text.contains("key id:"), "{text}");
}

#[test]
fn permission_set_flag_narrows_the_capability() {
    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args([
            "init",
            "--permission-set",
            "monitor",
            "--endpoint",
            "https://example.com/manifests/latest.json",
            "--public-key",
            KEY,
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("delta-update:monitor"));
    assert!(!text.contains("delta-update:default"));
}

#[test]
fn write_creates_a_dedicated_capability_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src-tauri")).unwrap();

    let run = || {
        Command::cargo_bin("delta-update")
            .unwrap()
            .args([
                "init",
                "--write",
                "--dir",
                dir.path().to_str().unwrap(),
                "--endpoint",
                "https://example.com/manifests/latest.json",
                "--public-key",
                KEY,
            ])
            .output()
            .unwrap()
    };

    let output = run();
    assert!(output.status.success(), "{output:?}");
    let path = dir.path().join("src-tauri/capabilities/delta-update.json");
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(value["identifier"], "delta-update");
    assert!(value["permissions"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("delta-update:manage")));

    let before = std::fs::read(&path).unwrap();
    let second = run();
    assert!(second.status.success(), "{second:?}");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(String::from_utf8_lossy(&second.stdout).contains("未覆盖"));
}

#[test]
fn write_requires_a_tauri_project() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args([
            "init",
            "--write",
            "--dir",
            dir.path().to_str().unwrap(),
            "--endpoint",
            "https://example.com/manifests/latest.json",
            "--public-key",
            KEY,
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--write requires"));
}

#[test]
fn write_warns_when_a_window_is_not_covered() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src-tauri")).unwrap();
    std::fs::write(
        dir.path().join("src-tauri/tauri.conf.json"),
        r#"{"app":{"windows":[{"label":"main"},{"label":"settings"}]}}"#,
    )
    .unwrap();

    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args([
            "init",
            "--write",
            "--dir",
            dir.path().to_str().unwrap(),
            "--endpoint",
            "https://example.com/manifests/latest.json",
            "--public-key",
            KEY,
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("只覆盖窗口"), "{text}");
    assert!(text.contains("settings"), "{text}");
}

#[test]
fn write_reports_an_existing_capability_without_the_permission() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src-tauri/capabilities")).unwrap();
    std::fs::write(
        dir.path().join("src-tauri/capabilities/delta-update.json"),
        r#"{"identifier":"delta-update","windows":["main"],"permissions":["core:default"]}"#,
    )
    .unwrap();

    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args([
            "init",
            "--write",
            "--dir",
            dir.path().to_str().unwrap(),
            "--endpoint",
            "https://example.com/manifests/latest.json",
            "--public-key",
            KEY,
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("缺少 delta-update 权限"), "{text}");
    assert!(text.contains("delta-update:manage"), "{text}");
}

#[test]
fn requires_a_public_key() {
    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args([
            "init",
            "--endpoint",
            "https://example.com/manifests/latest.json",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--public-key"));
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
