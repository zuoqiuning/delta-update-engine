//! `migrate`: print a migration plan from the official `tauri-plugin-updater`.

use std::path::Path;

use anyhow::{bail, Context};

use crate::cli::MigrateArgs;
use crate::output::{self, Console};

pub fn run(args: &MigrateArgs) -> anyhow::Result<()> {
    migrate(args)
}

/// Print a migration plan from the official `tauri-plugin-updater`.
///
/// `--write` additionally creates `capabilities/delta-update.json` and
/// `delta-update-migration.md`; it never rewrites `tauri.conf.json`, because
/// silently reformatting a user's config would hide the one difference that
/// matters (the official `latest.json` cannot be reused).
pub fn migrate(args: &MigrateArgs) -> anyhow::Result<()> {
    let path = ["src-tauri/tauri.conf.json", "tauri.conf.json"]
        .iter()
        .map(|relative| args.dir.join(relative))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no tauri.conf.json under {}; run from a Tauri project root or pass --dir",
                args.dir.display()
            )
        })?;
    let json: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
    )
    .with_context(|| format!("parse {}", path.display()))?;
    let Some(updater) = json.pointer("/plugins/updater") else {
        bail!(
            "no plugins.updater block in {}; nothing to migrate",
            path.display()
        );
    };
    if json.pointer("/plugins/delta-update").is_some() {
        bail!(
            "plugins.delta-update already exists in {}; remove it first for a fresh migration plan",
            path.display()
        );
    }

    let endpoint = updater
        .get("endpoints")
        .and_then(|value| value.as_array())
        .and_then(|items| items.iter().find_map(|item| item.as_str()))
        .or_else(|| updater.get("endpoint").and_then(|value| value.as_str()));
    let pubkey = updater.get("pubkey").and_then(|value| value.as_str());

    let out = Console::new(args.json);
    out.say(format!("检测到 {} 的 plugins.updater。\n", path.display()));
    if let Some(endpoint) = endpoint {
        if endpoint.contains("{{") {
            output::warn(
                "官方 endpoint 含 {{target}}/{{arch}} 模板；delta 清单是单平台的，请改为固定的 manifests/latest.json",
            );
        }
    }
    output::warn(
        "官方 updater 的 latest.json 不能复用：必须用 `delta-update release` 生成 delta 清单与签名",
    );

    let mut config = serde_json::json!({
        "plugins": {
            "delta-update": {
                "config_version": 1,
                "endpoint": endpoint.unwrap_or("<https://.../manifests/latest.json>"),
                "signing_public_keys": [pubkey.unwrap_or("<your minisign public key>")],
                "channel": "stable",
                "auto_check": true,
                "auto_apply": false,
                "auto_confirm": false
            }
        }
    });
    if args.write {
        // The guide is what the user pastes into tauri.conf.json, so it points
        // at the schema file written next to it in the same pass.
        config["plugins"]["delta-update"]["$schema"] =
            serde_json::Value::String("./delta-update.schema.json".into());
    }
    let capabilities = serde_json::json!({
        "identifier": "delta-update",
        "windows": ["main"],
        "permissions": ["core:default", "delta-update:manage-confirm"]
    });
    let guide = migration_guide(&path, &config, &capabilities)?;

    let mut schema_written = None;
    let mut capability_written = None;
    let mut guide_written = None;
    if args.write {
        schema_written = Some(crate::scaffold::write_schema(&args.dir, args.json)?);
        capability_written = Some(crate::scaffold::write_capability(
            &args.dir,
            &capabilities,
            args.json,
        )?);
        guide_written = Some(crate::scaffold::write_new_file(
            &args.dir.join("delta-update-migration.md"),
            &guide,
            args.json,
        )?);
        out.say("\ntauri.conf.json、Cargo.toml 与 main.rs 保持原样，请按指南手动修改。");
    } else {
        out.raw(&guide);
        out.say("\n提示：加 --write 可生成 capability、编辑器 schema 与 delta-update-migration.md（不会修改现有文件）。");
    }

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "v": 1,
                "tool": "delta-update-cli",
                "config": config["plugins"]["delta-update"],
                "capability": capabilities,
                "written": args.write,
                "schema_written": schema_written,
                "schema_path": args.write.then(|| {
                    args.dir
                        .join("src-tauri/delta-update.schema.json")
                        .display()
                        .to_string()
                }),
                "capability_written": capability_written,
                "guide_written": guide_written,
                "guide_path": args
                    .write
                    .then(|| args.dir.join("delta-update-migration.md").display().to_string()),
                "guide": guide,
            }))?
        );
    }
    Ok(())
}

/// Migration guide written by `migrate --write`. Everything here is also
/// printed, so the file and stdout never drift apart.
fn migration_guide(
    conf: &Path,
    config: &serde_json::Value,
    capabilities: &serde_json::Value,
) -> anyhow::Result<String> {
    let config = serde_json::to_string_pretty(config)?;
    let capabilities = serde_json::to_string_pretty(capabilities)?;
    let version = env!("CARGO_PKG_VERSION");
    Ok(format!(
        "# 从 tauri-plugin-updater 迁移到 delta-update\n\n\
         检测到 `{}` 的 `plugins.updater`。\n\n\
         ## 1. tauri.conf.json\n\n用以下配置替换 `plugins.updater`：\n\n```json\n{config}\n```\n\n\
         ## 2. capability\n\n写入 `src-tauri/capabilities/delta-update.json`：\n\n```json\n{capabilities}\n```\n\n\
         ## 3. Cargo.toml\n\n```diff\n- tauri-plugin-updater = \"2\"\n+ tauri-plugin-delta-update = \"{version}\"\n```\n\n\
         ## 4. src-tauri/src/main.rs\n\n```diff\n  use tauri_plugin_delta_update::BuilderExt;\n\n  tauri::Builder::default()\n\
         -     .plugin(tauri_plugin_updater::Builder::new().build())\n\
         +     .plugin(tauri_plugin_delta_update::init())\n\
         -     .run(tauri::generate_context!())\n\
         +     .run_with_delta_update(tauri::generate_context!())\n```\n\n\
         ## 5. 前端 API 对照\n\n\
         | `@tauri-apps/plugin-updater` | delta-update |\n\
         | --- | --- |\n\
         | `check()` → `Update \\| null` | `check()` → `CheckResult`（`available` / `version` / `download_bytes`） |\n\
         | `update.downloadAndInstall(cb)` | `prepare()`；进度由 `delta-update://state` 事件推送 |\n\
         | `update.install()` / `relaunch()` | `restartToApply()`：worker 替换文件后重启 |\n\
         | `onUpdaterEvent(cb)` | `listen(STATE_EVENT)` 或 `createUpdateController()` |\n\
         | `update.close()` | 无对应：prepare 产物由 journal 与 staging 管理 |\n\
         | （无） | `cancelPrepare()` 在下载/准备阶段取消 |\n\
         | （无） | `confirmHealthy()` 新版本首次启动自检，失败自动回滚 |\n\n\
         最小接入（框架无关）：\n\n\
         ```ts\n\
         import {{ createUpdateController }} from 'tauri-plugin-delta-update-api';\n\n\
         const updater = createUpdateController();\n\
         await updater.start();              // 订阅 delta-update://state 并读取快照\n\n\
         const result = await updater.check();\n\
         if (result.available) {{\n\
         \x20\x20await updater.prepare();\n\
         \x20\x20await updater.restart();       // 需要用户确认时先弹窗，再调用\n\
         }}\n\
         ```\n\n\
         React 项目可直接用 `useUpdater()`（`tauri-plugin-delta-update-api/react`），\n\
         它返回同一份状态与命令，无需自己订阅事件。\n\n\
         ## 检查清单\n\n\
         - 删除 `plugins.updater` 配置块与 `tauri-plugin-updater` 依赖\n\
         - 删除默认 capability 里的 `updater:default` 权限\n\
         - 用 `delta-update release` 重新发布清单；官方 `latest.json` 不兼容\n\
         - capability 使用 `delta-update:manage-confirm`（含 `confirm_healthy`）；\n\
           不想写前端确认逻辑时，把 `auto_confirm` 设为 `true`\n",
        conf.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guide is printed and written to disk from this one string, so a
    /// missing row here would silently disappear from both.
    #[test]
    fn migration_guide_maps_the_frontend_api() {
        let guide = migration_guide(
            Path::new("src-tauri/tauri.conf.json"),
            &serde_json::json!({"plugins": {"delta-update": {}}}),
            &serde_json::json!({"identifier": "delta-update"}),
        )
        .unwrap();
        for expected in [
            "## 5. 前端 API 对照",
            "`update.downloadAndInstall(cb)`",
            "`restartToApply()`",
            "`createUpdateController()`",
            "`confirmHealthy()`",
            "`useUpdater()`",
            "tauri-plugin-delta-update-api",
        ] {
            assert!(guide.contains(expected), "guide is missing {expected}");
        }
        // The dependency line must track the crate version, otherwise the
        // generated guide tells users to install a version that no longer
        // matches the plugin they just added.
        assert!(guide.contains(env!("CARGO_PKG_VERSION")));
        // An unescaped brace would mean a `format!` placeholder leaked into the
        // generated TypeScript snippet.
        assert!(!guide.contains("{{"), "guide has an unrendered brace");
    }
}
