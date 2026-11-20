//! SBOM derived from the signed manifest.

use anyhow::Context;

/// Inventory derived from the signed manifest: the same files and hashes plus
/// the object keys that carry them. Advisory only; clients never fetch it.
pub(crate) fn build_sbom(raw: &[u8], channel: &str, version: &str) -> anyhow::Result<Vec<u8>> {
    use crate::pipeline::manifest;

    let value: serde_json::Value =
        serde_json::from_slice(raw).context("parse manifest for sbom")?;
    let mut files: Vec<serde_json::Value> = value
        .get("files")
        .and_then(serde_json::Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(path, entry)| {
                    serde_json::json!({
                        "path": path,
                        "sha256": entry.get("sha256"),
                        "size": entry.get("size"),
                        "strategy": entry.get("strategy"),
                        "delta_key": entry.get("delta_key"),
                        "full_key": entry.get("full_key"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    let generated_at_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let sbom = serde_json::json!({
        "v": 1,
        "tool": "delta-update-cli",
        "format": "delta-update-sbom",
        "version": version,
        "channel": channel,
        "generated_at_unix": generated_at_unix,
        "signed_manifest": manifest::version_key("", version),
        "files": files,
    });
    serde_json::to_vec_pretty(&sbom).context("serialize sbom")
}
