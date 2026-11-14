//! Scaffolding shared by `init` and `migrate`.
//!
//! Both only ever create new files. `tauri.conf.json` is never rewritten, so
//! neither command needs backups or merge logic.

use std::path::Path;

use anyhow::Context;
use serde_json::Value;

/// Write `src-tauri/capabilities/delta-update.json` unless it already exists.
///
/// Returns `true` when a file was written. The identifier must be unique, so it
/// is forced to `delta-update` regardless of the caller's template.
pub(crate) fn write_capability(dir: &Path, permissions: &Value) -> anyhow::Result<bool> {
    let src_tauri = dir.join("src-tauri");
    if !src_tauri.is_dir() {
        anyhow::bail!(
            "--write requires {}; run it from a Tauri project root",
            src_tauri.display()
        );
    }
    let capabilities_dir = src_tauri.join("capabilities");
    std::fs::create_dir_all(&capabilities_dir)
        .with_context(|| format!("create {}", capabilities_dir.display()))?;
    let path = capabilities_dir.join("delta-update.json");
    if path.exists() {
        println!("已存在 {}，未覆盖。", path.display());
        return Ok(false);
    }
    let mut capability = permissions.clone();
    capability["identifier"] = Value::String("delta-update".into());
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&capability).context("serialize capability")?,
    )
    .with_context(|| format!("write {}", path.display()))?;
    println!("已写入 {}", path.display());
    Ok(true)
}

/// Write a file unless it exists; user edits are never clobbered.
pub(crate) fn write_new_file(path: &Path, contents: &str) -> anyhow::Result<bool> {
    if path.exists() {
        println!("已存在 {}，未覆盖。", path.display());
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(path, contents).with_context(|| format!("write {}", path.display()))?;
    println!("已写入 {}", path.display());
    Ok(true)
}
