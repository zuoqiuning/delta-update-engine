//! Key and object-key helpers shared by release, verify and audit.

use std::path::Path;

use anyhow::{bail, Context};

use crate::pipeline::manifest::prefixed;

pub(crate) fn read_public_keys(path: &Path) -> anyhow::Result<Vec<String>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read public key {}", path.display()))?;
    let mut keys = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("untrusted comment:") {
            continue;
        }
        for item in line.split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            delta_update_engine::crypto::validate_public_key(item)?;
            keys.push(item.to_string());
        }
    }
    if keys.is_empty() {
        bail!("{} contains no public keys", path.display());
    }
    Ok(keys)
}

pub(crate) fn full_key(prefix: &str, sha256: &str) -> String {
    prefixed(prefix, &format!("blobs/{sha256}"))
}

pub(crate) fn delta_key(prefix: &str, from_version: &str, version: &str, path: &str) -> String {
    prefixed(
        prefix,
        &format!("artifacts/{from_version}-to-{version}/{path}.tdp"),
    )
}

pub(crate) fn normalize_prefix(value: Option<&str>) -> anyhow::Result<String> {
    let prefix = value.unwrap_or("").trim_matches('/');
    if prefix.is_empty() {
        return Ok(String::new());
    }
    delta_update_engine::path::validate_rel_path(prefix)
        .with_context(|| format!("invalid --prefix: {prefix}"))?;
    Ok(prefix.to_string())
}

pub(crate) fn parse_delta_key(key: &str) -> Option<(&str, &str)> {
    let segments: Vec<&str> = key.split('/').collect();
    for pair in segments.windows(2) {
        if pair[0] == "artifacts" {
            if let Some(parts) = pair[1].rsplit_once("-to-") {
                return Some(parts);
            }
        }
    }
    None
}

pub(crate) fn human_bytes(value: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_from_version_from_delta_key() {
        assert_eq!(
            parse_delta_key("artifacts/2.0.0-to-2.1.0/App.exe.tdp"),
            Some(("2.0.0", "2.1.0"))
        );
        assert_eq!(
            parse_delta_key("artifacts/2.0.0-beta.1-to-2.1.0/a.tdp"),
            Some(("2.0.0-beta.1", "2.1.0"))
        );
        assert_eq!(
            parse_delta_key("releases/artifacts/2.0.0-to-2.1.0/a.tdp"),
            Some(("2.0.0", "2.1.0"))
        );
    }

    #[test]
    fn formats_bytes_for_humans() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
    }
}
