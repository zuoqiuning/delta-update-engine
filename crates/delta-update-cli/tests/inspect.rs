use assert_cmd::Command;
use delta_update_engine::journal::{state, ApplyConfig, FileOp, Journal};

const COMMIT_ID: &str = "00000000-0000-4000-8000-000000000000";

fn apply_config(root: &std::path::Path) -> ApplyConfig {
    ApplyConfig {
        v: 1,
        install_root: root.join("install"),
        staging_root: root.join("staging"),
        journal_root: root.to_path_buf(),
        version: "2.0.0".into(),
        commit_id: COMMIT_ID.into(),
        parent_pid: 1234,
    }
}

#[test]
fn reports_a_pending_commit_as_json() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    state::write_apply_config(root, &apply_config(root)).unwrap();
    std::fs::write(root.join("attempts"), "2").unwrap();

    let journal = Journal::new(
        COMMIT_ID,
        "2.0.0",
        "1.0.0",
        vec![FileOp::Place {
            staging_src: "files/App.exe".into(),
            dst: "App.exe".into(),
            sha256: "a".repeat(64),
            size: 1,
        }],
    );
    std::fs::write(root.join("commit.journal"), journal.encode().unwrap()).unwrap();

    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args(["inspect", "--journal-dir"])
        .arg(root)
        .arg("--json")
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(1),
        "unfinished journal must be recoverable-with-retry: {output:?}"
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["apply"]["version"], "2.0.0");
    assert_eq!(report["journal"]["ops"], 1);
    assert_eq!(report["journal"]["place"], 1);
    assert_eq!(report["attempts"], 2);
    assert!(report["diagnosis"]
        .as_str()
        .unwrap()
        .contains("未完成的提交日志"));
}

#[test]
fn reports_a_corrupt_journal_and_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("commit.journal"), b"not a journal").unwrap();

    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args(["inspect", "--journal-dir"])
        .arg(root)
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(2),
        "corrupt journal must need manual attention: {output:?}"
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("commit.journal 损坏"), "{text}");
    assert!(text.contains("诊断:"), "{text}");
}

#[test]
fn accepts_a_clean_journal_directory() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args(["inspect", "--journal-dir"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("没有待处理的更新状态"));
}

#[test]
fn journal_dir_reports_the_last_worker_event() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("worker.log"),
        "event=apply_started ts=1763000000 commit=x\n\
         event=apply_failed ts=1763000042 commit=x retries=0 msg=\"access denied\"\n",
    )
    .unwrap();

    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args(["inspect", "--journal-dir"])
        .arg(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("最近 worker 事件: apply_failed — access denied"),
        "{text}"
    );

    // The machine-readable form must carry the same fact, not just the table.
    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args(["inspect", "--journal-dir"])
        .arg(dir.path())
        .arg("--json")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["worker_event"]["name"], "apply_failed");
    assert_eq!(report["worker_event"]["ts"], "1763000042");
}

#[test]
fn journal_dir_reports_recent_update_history() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::write(
        root.join("update_history.jsonl"),
        format!(
            "{{\"v\":1,\"version\":\"2.0.0\",\"finished_at_unix\":{},\"result\":\"rolled_back\",\"reason\":\"crash loop\"}}\n",
            now.saturating_sub(7_200)
        ),
    )
    .unwrap();

    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args(["inspect", "--journal-dir"])
        .arg(root)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("v2.0.0 已回滚 (crash loop) · 2 小时前"),
        "{text}"
    );

    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args(["inspect", "--journal-dir"])
        .arg(root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["history"][0]["version"], "2.0.0");
    assert_eq!(report["history"][0]["result"], "rolled_back");
}

#[test]
fn bundle_cross_checks_journal_ops_and_patch_fallbacks() {
    let dir = tempfile::tempdir().unwrap();
    let bundle_path = dir.path().join("bundle.json");
    write_bundle(
        &bundle_path,
        "apply started commit=x\npatch fallback: bin/core.dll: E_PATCH_APPLY_FAILED\n",
    );

    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args(["inspect", "--bundle"])
        .arg(&bundle_path)
        .arg("--json")
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["v"], 1);
    assert_eq!(report["tool"], "delta-update-cli");
    assert_eq!(report["version"], "2.0.0");
    assert_eq!(report["fell_back"], 1);
    assert_eq!(report["removed"], 1);
    assert!(
        report["worker_event"].is_null(),
        "a pre-`event=` log has no structured event to report"
    );

    let files = report["files"].as_array().unwrap();
    let row = |path: &str| {
        files
            .iter()
            .find(|file| file["path"] == path)
            .unwrap_or_else(|| panic!("missing row for {path}: {files:?}"))
    };
    assert_eq!(row("App.exe")["ops"], serde_json::json!(["Place (landed)"]));
    assert_eq!(
        row("bin/core.dll")["fallbacks"],
        serde_json::json!(["E_PATCH_APPLY_FAILED"])
    );
    assert_eq!(
        row("legacy.txt")["ops"],
        serde_json::json!(["Trash (landed)"])
    );
}

/// A minimal exported bundle: one placed file, one removal, and whatever the
/// worker log contained when it was exported.
fn write_bundle(path: &std::path::Path, worker_log: &str) {
    let bundle = serde_json::json!({
        "tool": "tauri-plugin-delta-update",
        "report": {
            "journal_dir": "<journal_root>",
            "apply": {
                "version": "2.0.0",
                "commit_id": COMMIT_ID,
                "install_root": "<install_root>",
                "staging_root": "<staging_root>",
                "parent_pid": 1
            },
            "status": { "state": "completed", "version": "2.0.0", "commit_id": COMMIT_ID },
            "journal": {
                "file": "commit.journal",
                "commit_id": COMMIT_ID,
                "version": "2.0.0",
                "from_version": "1.0.0",
                "ops": 2,
                "backup": 0,
                "place": 1,
                "trash": 1,
                "restore": 0,
                "remove_trash": 0,
                "ops_detail": [
                    { "kind": "Place", "path": "App.exe", "status": "landed" },
                    { "kind": "Trash", "path": "legacy.txt", "status": "landed" }
                ]
            },
            "rollback_journal": null,
            "prepared": null,
            "attempts": 1,
            "skipped_version": null,
            "warnings": [],
            "diagnosis": "ok"
        },
        "worker_log": worker_log,
        "history": [
            { "v": 1, "version": "2.0.0", "finished_at_unix": 1763000042, "result": "completed" },
            {
                "v": 1,
                "version": "2.1.0",
                "finished_at_unix": 1763000142,
                "result": "rolled_back",
                "reason": "crash loop"
            }
        ]
    });
    std::fs::write(path, serde_json::to_vec(&bundle).unwrap()).unwrap();
}

#[test]
fn bundle_reports_history_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let bundle_path = dir.path().join("bundle.json");
    write_bundle(&bundle_path, "event=apply_ok ts=1763000142 commit=x\n");

    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args(["inspect", "--bundle"])
        .arg(&bundle_path)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("recent updates: v2.1.0 rolled_back (crash loop), v2.0.0 ok"),
        "{output:?}"
    );

    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args(["inspect", "--bundle"])
        .arg(&bundle_path)
        .arg("--json")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["history"][0]["version"], "2.1.0");
    assert_eq!(report["history"][1]["version"], "2.0.0");
}

#[test]
fn bundle_reports_the_newest_structured_worker_event() {
    let dir = tempfile::tempdir().unwrap();
    let bundle_path = dir.path().join("bundle.json");
    write_bundle(
        &bundle_path,
        "event=apply_started ts=1763000000 commit=x\n\
         event=apply_failed ts=1763000042 commit=x ms=8 retries=0 msg=\"locked by antivirus\"\n",
    );

    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args(["inspect", "--bundle"])
        .arg(&bundle_path)
        .arg("--json")
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["worker_event"]["name"], "apply_failed");
    assert_eq!(report["worker_event"]["ts"], "1763000042");
    assert_eq!(report["worker_event"]["msg"], "locked by antivirus");
}

#[test]
fn bundle_conflicts_with_a_journal_directory() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args(["inspect", "--bundle"])
        .arg(dir.path().join("bundle.json"))
        .args(["--journal-dir"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
}

#[test]
fn export_writes_a_support_bundle_with_the_worker_log_tail() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("worker.log"), "apply commit x failed: locked\n").unwrap();
    std::fs::write(root.join("attempts"), "1").unwrap();
    std::fs::write(
        root.join("update_history.jsonl"),
        "{\"v\":1,\"version\":\"2.0.0\",\"finished_at_unix\":1,\"result\":\"rolled_back\",\"reason\":\"crash loop\"}\n",
    )
    .unwrap();
    let bundle_path = root.join("out/bundle.json");

    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args(["inspect", "--journal-dir"])
        .arg(root)
        .arg("--json")
        .arg("--export")
        .arg(&bundle_path)
        .arg("--redact")
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("诊断包已写入"));

    let bundle: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&bundle_path).unwrap()).unwrap();
    assert_eq!(bundle["tool"], "delta-update-cli");
    assert_eq!(bundle["report"]["attempts"], 1);
    assert_eq!(bundle["redacted"], true);
    assert_eq!(bundle["report"]["journal_dir"], "<journal_root>");
    assert!(bundle["system"]["os"].is_string());
    assert!(bundle["worker_log"].as_str().unwrap().contains("locked"));
    let names: Vec<&str> = bundle["journal_dir_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"worker.log"));
    assert!(names.contains(&"attempts"));
    assert_eq!(bundle["history"][0]["version"], "2.0.0");
    assert_eq!(bundle["history"][0]["result"], "rolled_back");
}
