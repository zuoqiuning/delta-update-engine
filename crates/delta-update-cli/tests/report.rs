use assert_cmd::Command;

#[test]
fn report_history_flags_a_coverage_drop_and_the_new_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let report = |version: &str, modified: u64, patched: u64, fallbacks: &[(&str, &str)]| {
        serde_json::json!({
            "v": 1,
            "tool": "delta-update-cli",
            "version": version,
            "changed": modified,
            "delta_bytes": 1024,
            "changed_bytes": 4096,
            "coverage": {
                "modified": modified,
                "patched": patched,
                "fallback": { "too_large": 1, "ratio": 0, "incompressible": 0 }
            },
            "fallbacks": fallbacks.iter().map(|(path, reason)| serde_json::json!({
                "path": path,
                "reason": reason,
            })).collect::<Vec<_>>(),
            "self_test": { "ran": true, "millis": 5 },
            "audit": { "ok": true }
        })
    };
    std::fs::write(
        dir.path().join("2.0.0.json"),
        serde_json::to_vec(&report("2.0.0", 12, 9, &[])).unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("2.1.0.json"),
        serde_json::to_vec(&report("2.1.0", 15, 6, &[("bin/large.dll", "too_large")])).unwrap(),
    )
    .unwrap();

    let output = Command::cargo_bin("delta-update")
        .unwrap()
        .args(["report", "--history"])
        .arg(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("VERSION"), "{stdout}");
    assert!(stdout.contains("2.0.0"), "{stdout}");
    assert!(stdout.contains("40.0%"), "{stdout}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("below 2.0.0"), "{stderr}");
    assert!(stderr.contains("bin/large.dll (too_large)"), "{stderr}");
}
