use serde_json::{json, Value};

/// Minimal valid manifest used by contract tests.
pub fn sample_manifest(version: &str, sha256: &str, size: u64) -> Value {
    json!({
        "schema": 1,
        "version": version,
        "channel": "stable",
        "min_client_schema": 1,
        "rollback": false,
        "files": {
            "App.exe": {
                "sha256": sha256,
                "size": size,
                "strategy": "full",
                "full_key": format!("artifacts/full/{version}/App.exe"),
                "executable": true
            }
        }
    })
}
