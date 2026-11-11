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
