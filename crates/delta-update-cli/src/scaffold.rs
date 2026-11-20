//! Scaffolding shared by `init` and `migrate`.
//!
//! Everything creates new files only, except `--merge-config`, which adds a
//! missing `plugins.delta-update` section to `tauri.conf.json` and keeps the
//! original as `.bak`.

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

/// Merge `plugins.delta-update` into `src-tauri/tauri.conf.json`.
///
/// Only fills in a missing section: an existing section is left untouched, so
/// hand-edited keys are never clobbered. The first rewrite keeps a `.bak` copy
/// of the original file. Key order may change because `serde_json` re-serializes
/// the document; values are preserved.
pub(crate) fn merge_plugin_config(dir: &Path, config: &Value) -> anyhow::Result<bool> {
    let json5 = ["src-tauri/tauri.conf.json5", "tauri.conf.json5"]
        .into_iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file());
    if let Some(path) = json5 {
        anyhow::bail!(
            "检测到 {}：--merge-config 只支持 JSON 配置，请手动把 plugins.delta-update 合并进去",
            path.display()
        );
    }
    let path = ["src-tauri/tauri.conf.json", "tauri.conf.json"]
        .into_iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
        .with_context(|| {
            format!(
                "tauri.conf.json not found under {}; pass --dir <project root>",
                dir.display()
            )
        })?;
    let raw = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let mut value: Value = serde_json::from_slice(&raw)
        .with_context(|| format!("parse {} as JSON", path.display()))?;
    let section = config
        .get("plugins")
        .and_then(|plugins| plugins.get("delta-update"))
        .cloned()
        .context("internal: generated config has no plugins.delta-update")?;

    let plugins = value
        .as_object_mut()
        .with_context(|| format!("{} must be a JSON object", path.display()))?
        .entry("plugins")
        .or_insert_with(|| Value::Object(Default::default()));
    let plugins = plugins
        .as_object_mut()
        .with_context(|| format!("plugins in {} must be an object", path.display()))?;
    if plugins.contains_key("delta-update") {
        println!("{} 已有 plugins.delta-update，未修改。", path.display());
        return Ok(false);
    }
    plugins.insert("delta-update".into(), section);

    let backup = path.with_extension("json.bak");
    if !backup.exists() {
        std::fs::write(&backup, &raw).with_context(|| format!("write {}", backup.display()))?;
    }
    let mut bytes = serde_json::to_vec_pretty(&value).context("serialize tauri.conf.json")?;
    bytes.push(b'\n');
    write_atomic(&path, &bytes).with_context(|| format!("write {}", path.display()))?;
    println!(
        "已合并 plugins.delta-update 到 {}（原文件备份：{}）",
        path.display(),
        backup.display()
    );
    Ok(true)
}

/// Replace `path` without ever exposing a truncated file: write a sibling
/// temporary file first, then rename over the target.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
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
