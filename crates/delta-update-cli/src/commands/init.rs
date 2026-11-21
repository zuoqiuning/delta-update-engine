use std::path::{Path, PathBuf};

use anyhow::Context;
use delta_update_engine::crypto::validate_public_key;
use serde_json::Value;
use url::Url;

use crate::cli::InitArgs;
use crate::output::Console;

const PLACEHOLDER_ENDPOINT: &str =
    "https://your-bucket.cos.ap-guangzhou.myqcloud.com/manifests/latest.json";

struct Existing {
    endpoint: Option<String>,
    keys: Vec<String>,
    official_key: Option<String>,
    source: PathBuf,
}

pub fn run(args: &InitArgs) -> anyhow::Result<()> {
    let out = Console::new(args.json);
    let write = args.write || args.merge_config;
    let existing = detect(&args.dir);
    let mut notes = Vec::new();
    let mut endpoint = args.endpoint.clone();
    let mut keys = args.public_keys.clone();
    for path in &args.signing_keys {
        keys.push(derive_public_key(path)?);
    }

    if let Some(existing) = &existing {
        if endpoint.is_none() {
            endpoint = existing.endpoint.clone();
            if endpoint.is_some() {
                notes.push("endpoint");
            }
        }
        if keys.is_empty() {
            if !existing.keys.is_empty() {
                keys = existing.keys.clone();
                notes.push("signing_public_keys");
            } else if let Some(key) = &existing.official_key {
                keys = vec![key.clone()];
                notes.push("官方 updater 公钥");
            }
        }
    }

    let endpoint = match endpoint {
        Some(value) => {
            let parsed = Url::parse(&value).context("endpoint is not a valid URL")?;
            if parsed.scheme() != "https" {
                if args.endpoint.is_some() {
                    anyhow::bail!(
                        "endpoint must use HTTPS (plain HTTP is only for local development)"
                    );
                }
                notes.push("检测到的 endpoint 不是 HTTPS，已使用占位值");
                PLACEHOLDER_ENDPOINT.to_string()
            } else {
                value
            }
        }
        None => PLACEHOLDER_ENDPOINT.to_string(),
    };
    let endpoint_is_placeholder = endpoint == PLACEHOLDER_ENDPOINT;

    if !keys.is_empty() {
        let from_args = !args.public_keys.is_empty() || !args.signing_keys.is_empty();
        for key in &keys {
            if validate_public_key(key).is_err() {
                if from_args {
                    anyhow::bail!("invalid minisign public key: {key}");
                }
                notes.push("检测到的公钥无效，已忽略");
                keys.clear();
                break;
            }
        }
    }
    if keys.is_empty() {
        anyhow::bail!(
            "at least one --public-key is required; a placeholder key would make the app fail to start.\n\
             1) 生成密钥：minisign -G -p release.pub -s release.key\n\
             2) 重跑：delta-update init --signing-key release.key（自动派生公钥）\n\
             或先 `delta-update key-info --key release.key`，再用 --public-key <公钥>"
        );
    }

    if let Some(existing) = &existing {
        out.say(format!(
            "已从 {} 读取默认值（{}）。\n",
            existing.source.display(),
            notes.join("、")
        ));
        if existing.official_key.is_some() {
            out.say("检测到官方 updater 配置：迁移后请移除 tauri-plugin-updater 依赖、plugins.updater 配置与 updater 权限；同一应用不要同时启用两套更新通道。\n");
        }
    }

    out.say("下一步（5 项）：");
    out.say(format!(
        "依赖：`tauri-plugin-delta-update = \"{}\"`（需要控制台日志时加 features = [\"logging\"]）；前端 SDK：`pnpm add tauri-plugin-delta-update-api`。",
        env!("CARGO_PKG_VERSION")
    ));
    out.say("1. 生成密钥：minisign -G -p release.pub -s release.key；再执行 `delta-update init --signing-key release.key` 自动派生公钥，私钥离线保管。");
    out.say("2. 用 `BuilderExt` 接入：`.plugin(tauri_plugin_delta_update::init()).run_with_delta_update(context)`；它会先处理 worker 模式，避免顺序漏接。");
    out.say("   fn main() {");
    out.say("       let context = tauri::generate_context!();");
    out.say("       tauri::Builder::default()");
    out.say("           .plugin(tauri_plugin_delta_update::init())");
    out.say("           .run_with_delta_update(context)");
    out.say("           .expect(\"failed to run the app\");");
    out.say("   }");
    out.say("3. endpoint 必须是 HTTPS；只有本地调试才用 allow_insecure_http（仅放行 loopback）。");
    out.say("4. 运行 `delta-update doctor --check-config --dir .`，确认插件配置能被引擎接受。");
    out.say("5. 不想写任何前端代码：加 `--auto` 重新生成（auto_apply/auto_confirm = true），启动检查、退出替换、下次启动自动确认全部由插件完成。\n");

    let mut config = serde_json::json!({
        "plugins": {
            "delta-update": {
                "config_version": 1,
                "endpoint": endpoint,
                "signing_public_keys": keys,
                "channel": args.channel.clone(),
                "auto_check": true,
                "auto_apply": args.auto,
                "auto_confirm": args.auto
            }
        }
    });
    if write {
        // Only when the schema file is actually written; a printed-only config
        // must not reference a file the user does not have.
        config["plugins"]["delta-update"]["$schema"] =
            Value::String("./delta-update.schema.json".into());
    }
    let capabilities = serde_json::json!({
        "identifier": "default",
        "windows": ["main"],
        "permissions": [
            "core:default",
            format!("delta-update:{}", args.permission_set)
        ]
    });
    let mut schema_written = None;
    if write {
        // Written before the merge so the `$schema` the config references
        // already exists; if the merge fails, this editor-only file is harmless.
        schema_written = Some(crate::scaffold::write_schema(&args.dir, args.json)?);
    }
    // Merge next: if the project structure is wrong, fail before writing any
    // capability file, so a failed run leaves nothing behind.
    let mut config_merged = false;
    if args.merge_config {
        config_merged = crate::scaffold::merge_plugin_config(&args.dir, &config, args.json)?;
    } else {
        out.say(
            "将以下 `plugins.delta-update` 子对象合并到 src-tauri/tauri.conf.json 的顶层 `plugins`（下次可用 `--merge-config` 自动完成）：\n",
        );
        out.say(format!("{}\n", serde_json::to_string_pretty(&config)?));
    }
    let mut capability_written = None;
    let mut warnings = Vec::new();
    if write {
        capability_written = Some(crate::scaffold::write_capability(
            &args.dir,
            &capabilities,
            args.json,
        )?);
        warnings = capability_warnings(&args.dir, &args.permission_set);
        if !args.json {
            for warning in &warnings {
                out.warn(warning);
            }
        }
    }
    if write {
        out.say(format!(
            "已写入 src-tauri/capabilities/delta-update.json（identifier=delta-update，权限 delta-update:{}）。\n",
            args.permission_set
        ));
    } else {
        out.say("把下面一行加入 src-tauri/capabilities/default.json 的 `permissions` 数组：\n");
        out.say(format!("\"delta-update:{}\"\n", args.permission_set));
    }
    if args.auto {
        out.say(
            "注：已写入 auto_apply / auto_confirm = true。应用无需前端代码：启动后自动检查并准备，退出时替换，下次启动自动确认健康（仍需保留回滚保护）。\n",
        );
    } else if args.permission_set == "default" {
        out.say(
            "注：default 是兼容集，含 confirm_healthy 等全部命令；新接入建议使用 manage-confirm 或 monitor。\n",
        );
    } else if args.permission_set == "manage" {
        out.say(
            "注：manage 不含 confirm_healthy。二选一：前端需要调用 confirm_healthy 时改用 --permission-set manage-confirm（默认）；保持 manage 时请设置 auto_confirm: true 由插件兜底。\n",
        );
    } else if args.permission_set == "manage-confirm" {
        out.say(
            "注：manage-confirm = manage + confirm_healthy，前端可以直接调用 confirm_healthy()；这是默认集。\n",
        );
    }
    out.say("其余：");
    if args.auto {
        out.say("- 无需前端代码即可完成更新；需要展示进度或“稍后提醒”时再接入 tauri-plugin-delta-update-api。");
    } else {
        out.say("- 前端安装 tauri-plugin-delta-update-api，并订阅 delta-update://state。");
    }
    out.say("- 用 delta-update release 发布清单、签名与对象。");
    out.say("- 当前插件仅支持 Windows 桌面；其他平台请使用官方安装器/包管理器。");
    if !args.auto {
        out.say("- auto_confirm 默认关闭；复杂应用应在关键初始化完成后调用 confirm_healthy()。");
    }
    if endpoint_is_placeholder {
        out.say("\n提示：endpoint 仍是占位值，请用 --endpoint 重新生成。");
    }

    if args.json {
        let capability = write.then(|| {
            args.dir
                .join("src-tauri/capabilities/delta-update.json")
                .display()
                .to_string()
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "v": 1,
                "tool": "delta-update-cli",
                "endpoint": config["plugins"]["delta-update"]["endpoint"],
                "channel": args.channel,
                "signing_public_keys": config["plugins"]["delta-update"]["signing_public_keys"],
                "permission_set": args.permission_set,
                "auto": args.auto,
                "reused_from_config": notes,
                "config_merged": config_merged,
                "schema_written": schema_written,
                "schema_path": write.then(|| {
                    args.dir
                        .join("src-tauri/delta-update.schema.json")
                        .display()
                        .to_string()
                }),
                "capability_written": capability_written,
                "capability_path": capability,
                "config": config["plugins"]["delta-update"],
                "warnings": warnings,
            }))?
        );
    }
    Ok(())
}

/// `--signing-key` exists so onboarding is "generate a key, point init at it"
/// instead of copy-pasting a base64 line out of `key-info`.
fn derive_public_key(path: &Path) -> anyhow::Result<String> {
    let secret = crate::pipeline::sign::load_secret_key(path)?;
    let public = minisign::PublicKey::from_secret_key(&secret)
        .with_context(|| format!("derive a public key from {}", path.display()))?;
    Ok(public.to_base64())
}

/// `init --write` owns `capabilities/delta-update.json`. If that file was left
/// untouched by an earlier run (or only covers some windows), the commands are
/// denied at runtime with no hint, so report exactly what to edit. Returned
/// instead of printed so `--json` can put the same text in the envelope.
fn capability_warnings(dir: &Path, permission_set: &str) -> Vec<String> {
    let path = dir.join("src-tauri/capabilities/delta-update.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return Vec::new();
    };
    let grants = value["permissions"].as_array().is_some_and(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .any(|item| item.starts_with("delta-update:"))
    });
    if !grants {
        return vec![format!(
            "警告：{} 的 `permissions` 缺少 delta-update 权限，前端调用会被拒绝；把 \"delta-update:{permission_set}\" 加入该数组。",
            path.display()
        )];
    }
    let covered: Vec<&str> = value["windows"]
        .as_array()
        .map(|windows| windows.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let missing: Vec<String> = window_labels(dir)
        .into_iter()
        .filter(|label| !covered.contains(&label.as_str()))
        .collect();
    if missing.is_empty() {
        return Vec::new();
    }
    vec![format!(
        "警告：{} 只覆盖窗口 {covered:?}；窗口 {missing:?} 的更新命令会被拒绝。把缺失的标签加入该文件的 `windows`，或把 \"delta-update:{permission_set}\" 加到覆盖这些窗口的 capability。",
        path.display()
    )]
}

/// Window labels from `tauri.conf.json`. Tauri creates a `main` window when the
/// config lists none.
fn window_labels(dir: &Path) -> Vec<String> {
    let Some(mut windows) = std::fs::read(dir.join("src-tauri/tauri.conf.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|json| json.pointer("/app/windows").cloned())
        .and_then(|windows| windows.as_array().cloned())
    else {
        return vec!["main".to_string()];
    };
    let mut labels: Vec<String> = Vec::new();
    for label in windows
        .iter_mut()
        .filter_map(|window| window.get("label"))
        .filter_map(Value::as_str)
    {
        if !labels.iter().any(|existing| existing == label) {
            labels.push(label.to_string());
        }
    }
    if labels.is_empty() {
        labels.push("main".to_string());
    }
    labels
}

fn detect(dir: &Path) -> Option<Existing> {
    for relative in ["src-tauri/tauri.conf.json", "tauri.conf.json"] {
        let path = dir.join(relative);
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(json) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        let endpoint = json
            .pointer("/plugins/delta-update/endpoint")
            .and_then(Value::as_str)
            .map(str::to_string);
        let keys: Vec<String> = json
            .pointer("/plugins/delta-update/signing_public_keys")
            .and_then(Value::as_array)
            .map(|keys| {
                keys.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let official_key = json
            .pointer("/plugins/updater/pubkey")
            .and_then(Value::as_str)
            .map(str::to_string);
        if endpoint.is_none() && keys.is_empty() && official_key.is_none() {
            continue;
        }
        return Some(Existing {
            endpoint,
            keys,
            official_key,
            source: path,
        });
    }
    None
}
