//! Offline validation of `plugins.delta-update` and the capability files.

use std::path::Path;

use anyhow::{bail, Context};

use crate::cli::DoctorArgs;
use crate::output;

/// Permission sets shipped by the plugin (`permissions/*.toml`).
const PLUGIN_PERMISSION_SETS: &[&str] = &["default", "monitor", "manage", "manage-confirm"];
/// Commands with generated `allow-<name>` / `deny-<name>` permissions.
const PLUGIN_COMMANDS: &[&str] = &[
    "check",
    "prepare",
    "cancel-prepare",
    "status",
    "confirm-healthy",
    "restart-to-apply",
    "export-diagnostics",
    "open-diagnostics-folder",
    "get-update-history",
    "clear-skipped-version",
    "skip-version",
];

/// Capability files are parsed rather than substring-matched so a malformed
/// file is reported instead of silently ignored. Unknown `delta-update:*`
/// identifiers are errors too: Tauri rejects them at startup, which looks like
/// "the plugin is broken" if `doctor` stays quiet.
fn inspect_capabilities(dir: &Path) -> (bool, Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (false, Vec::new());
    };
    let mut granted = false;
    let mut errors = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let value: serde_json::Value = match std::fs::read_to_string(&path)
            .map_err(|error| error.to_string())
            .and_then(|text| serde_json::from_str(&text).map_err(|error| error.to_string()))
        {
            Ok(value) => value,
            Err(error) => {
                errors.push(format!("{}: {error}", path.display()));
                continue;
            }
        };
        if value_grants_confirm(&value) {
            granted = true;
        }
        let mut unknown = Vec::new();
        collect_unknown_permissions(&value, &mut unknown);
        for name in unknown {
            errors.push(format!(
                "{}: unknown permission `{name}` (Tauri will reject it at startup)",
                path.display()
            ));
        }
    }
    (granted, errors)
}

fn collect_unknown_permissions(value: &serde_json::Value, unknown: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => {
            if !known_plugin_permission(text) {
                unknown.push(text.clone());
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_unknown_permissions(item, unknown);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values() {
                collect_unknown_permissions(item, unknown);
            }
        }
        _ => {}
    }
}

/// Strings outside the `delta-update:` namespace are not ours to validate.
fn known_plugin_permission(name: &str) -> bool {
    let Some(suffix) = name.strip_prefix("delta-update:") else {
        return true;
    };
    if suffix == "*" || PLUGIN_PERMISSION_SETS.contains(&suffix) {
        return true;
    }
    suffix
        .strip_prefix("allow-")
        .or_else(|| suffix.strip_prefix("deny-"))
        .is_some_and(|command| PLUGIN_COMMANDS.contains(&command))
}

fn value_grants_confirm(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(text) => matches!(
            text.as_str(),
            "delta-update:default"
                | "delta-update:manage-confirm"
                | "delta-update:allow-confirm-healthy"
                | "delta-update:*"
        ),
        serde_json::Value::Array(items) => items.iter().any(value_grants_confirm),
        serde_json::Value::Object(map) => map.values().any(value_grants_confirm),
        _ => false,
    }
}

/// Validate `plugins.delta-update` from a project's `tauri.conf.json`.
///
/// This uses the engine's schema, so CI catches the same config errors the
/// plugin would report at runtime. It does not touch the network or require
/// `install_dir` to exist.
pub(crate) fn check_plugin_config(args: &DoctorArgs) -> anyhow::Result<()> {
    let path = ["src-tauri/tauri.conf.json", "tauri.conf.json"]
        .into_iter()
        .map(|name| args.dir.join(name))
        .find(|path| path.is_file())
        .with_context(|| {
            format!(
                "tauri.conf.json not found under {} (pass --dir <project root>)",
                args.dir.display()
            )
        })?;
    let raw = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let value: serde_json::Value =
        serde_json::from_slice(&raw).with_context(|| format!("parse {}", path.display()))?;
    let section = value
        .get("plugins")
        .and_then(|plugins| plugins.get("delta-update"))
        .with_context(|| {
            format!(
                "{} has no plugins.delta-update section; run `delta-update init` for a snippet",
                path.display()
            )
        })?;
    let config = delta_update_engine::config::PluginConfig::from_json(section)?;
    let install_mode = value
        .get("bundle")
        .and_then(|bundle| bundle.get("windows"))
        .and_then(|windows| windows.get("nsis"))
        .and_then(|nsis| nsis.get("installMode"))
        .and_then(|mode| mode.as_str())
        .map(str::to_string);
    let mut warnings = Vec::new();
    if let Some(mode) = install_mode.as_deref() {
        if matches!(mode, "perMachine" | "both") && config.auto_apply {
            warnings.push(format!(
                "installMode `{mode}` 需要管理员权限：auto_apply 会在用户未预期时触发 UAC，建议改用 restart_to_apply 或关闭 auto_apply"
            ));
            if mode == "both" {
                warnings.push(
                    "installMode `both`：worker 必须同时覆盖 per-user 与 per-machine 两种安装目录"
                        .into(),
                );
            }
        }
    }
    let (confirm_granted, capability_errors) =
        inspect_capabilities(&args.dir.join("src-tauri/capabilities"));
    if !config.auto_confirm && !confirm_granted {
        warnings.push(
            "auto_confirm=false 且 capability 未授予 confirm_healthy：更新能安装但永远不会确认健康，回滚保护实际关闭。改用 delta-update:manage-confirm、设置 auto_confirm: true，或运行 init --auto"
                .into(),
        );
    }
    let ok = capability_errors.is_empty();

    if args.json {
        let value = serde_json::json!({
            "v": 1,
            "tool": "delta-update-cli",
            "mode": "config",
            "ok": ok,
            "config": path,
            "endpoint": config.endpoint,
            "channel": config.channel,
            "config_version": config.config_version,
            "install_mode": install_mode,
            "confirm_health_granted": confirm_granted,
            "errors": capability_errors,
            "warnings": warnings,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        output::info(&format!("config ok: {}", path.display()));
        output::info(&format!(
            "endpoint: {}  channel: {}  keys: {}  install_mode: {}",
            config.endpoint,
            config.channel,
            config.signing_public_keys.len(),
            install_mode.as_deref().unwrap_or("unknown")
        ));
        for error in &capability_errors {
            output::error(&format!("capability: {error}"));
        }
        for warning in &warnings {
            output::warn(warning);
        }
    }
    if !ok {
        bail!(
            "{} capability problem(s) under src-tauri/capabilities (invalid JSON or unknown delta-update permission)",
            capability_errors.len()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_confirm_health_grants_in_capabilities() {
        let dir = tempfile::tempdir().unwrap();
        let caps = dir.path().join("capabilities");
        std::fs::create_dir_all(&caps).unwrap();
        assert_eq!(inspect_capabilities(&caps), (false, Vec::new()));

        std::fs::write(
            caps.join("monitor.json"),
            r#"{"permissions":["delta-update:monitor"]}"#,
        )
        .unwrap();
        assert_eq!(inspect_capabilities(&caps), (false, Vec::new()));

        std::fs::write(
            caps.join("default.json"),
            r#"{"permissions":["core:default","delta-update:manage-confirm"]}"#,
        )
        .unwrap();
        assert_eq!(inspect_capabilities(&caps), (true, Vec::new()));

        // A malformed capability is reported instead of being skipped.
        std::fs::write(caps.join("broken.json"), "{not json").unwrap();
        let (granted, errors) = inspect_capabilities(&caps);
        assert!(granted);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("broken.json"));
    }
}
