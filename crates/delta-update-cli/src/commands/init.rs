use std::path::{Path, PathBuf};

use anyhow::Context;
use delta_update_engine::crypto::validate_public_key;
use serde_json::Value;
use url::Url;

use crate::cli::InitArgs;

const PLACEHOLDER_ENDPOINT: &str =
    "https://your-bucket.cos.ap-guangzhou.myqcloud.com/manifests/latest.json";

struct Existing {
    endpoint: Option<String>,
    keys: Vec<String>,
    official_key: Option<String>,
    source: PathBuf,
}

pub fn run(args: &InitArgs) -> anyhow::Result<()> {
    let existing = detect(&args.dir);
    let mut notes = Vec::new();
    let mut endpoint = args.endpoint.clone();
    let mut keys = args.public_keys.clone();

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
        let from_args = !args.public_keys.is_empty();
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
             2) 取出公钥：delta-update key-info --key release.key\n\
             3) 重跑：delta-update init --public-key <公钥>"
        );
    }

    if let Some(existing) = &existing {
        println!(
            "已从 {} 读取默认值（{}）。\n",
            existing.source.display(),
            notes.join("、")
        );
        if existing.official_key.is_some() {
            println!(
                "检测到官方 updater 配置：迁移后请移除 tauri-plugin-updater 依赖、plugins.updater 配置与 updater 权限；同一应用不要同时启用两套更新通道。\n"
            );
        }
    }

    println!("下一步（4 项）：");
    println!("1. 生成密钥：minisign -G -p release.pub -s release.key；用 `delta-update key-info --key release.key` 取出公钥填入 signing_public_keys，私钥离线保管。");
    println!("2. 用 `BuilderExt` 接入：`.plugin(tauri_plugin_delta_update::init()).run_with_delta_update(context)`；它会先处理 worker 模式，避免顺序漏接。");
    println!("   fn main() {{");
    println!("       let context = tauri::generate_context!();");
    println!("       tauri::Builder::default()");
    println!("           .plugin(tauri_plugin_delta_update::init())");
    println!("           .run_with_delta_update(context)");
    println!("           .expect(\"failed to run the app\");");
    println!("   }}");
    println!("3. endpoint 必须是 HTTPS；只有本地调试才用 allow_insecure_http（仅放行 loopback）。");
    println!(
        "4. 合并下面的配置后运行 `delta-update doctor --check-config --dir .`，确认插件配置能被引擎接受。\n"
    );

    let config = serde_json::json!({
        "plugins": {
            "delta-update": {
                "config_version": 1,
                "endpoint": endpoint,
                "signing_public_keys": keys,
                "channel": args.channel.clone(),
                "auto_check": true,
                "auto_apply": false,
                "auto_confirm": false
            }
        }
    });
    let capabilities = serde_json::json!({
        "identifier": "default",
        "windows": ["main"],
        "permissions": [
            "core:default",
            format!("delta-update:{}", args.permission_set)
        ]
    });
    if args.write {
        crate::scaffold::write_capability(&args.dir, &capabilities)?;
        report_capability_coverage(&args.dir, &args.permission_set);
    }

    println!(
        "将以下 `plugins.delta-update` 子对象合并到 src-tauri/tauri.conf.json 的顶层 `plugins`：\n"
    );
    println!("{}\n", serde_json::to_string_pretty(&config)?);
    println!("将以下权限项追加到 src-tauri/capabilities/default.json 的 `permissions` 数组：\n");
    println!("{}\n", serde_json::to_string_pretty(&capabilities)?);
    if args.permission_set == "default" {
        println!(
            "注：default 是兼容集，含 confirm_healthy 等全部命令；生产环境建议改用 manage 或 monitor。\n"
        );
    } else if args.permission_set == "manage" {
        println!(
            "注：manage 不含 confirm_healthy；若前端需要调用 confirm_healthy，请显式使用 --permission-set default，或在关键初始化完成后由 Rust 调用。\n"
        );
    }
    println!("其余：");
    println!("- 前端安装 tauri-plugin-delta-update-api，并订阅 delta-update://state。");
    println!("- 用 delta-update release 发布清单、签名与对象。");
    println!("- 当前插件仅支持 Windows 桌面；其他平台请使用官方安装器/包管理器。");
    println!("- auto_confirm 默认关闭；复杂应用应在关键初始化完成后调用 confirm_healthy()。");
    if endpoint_is_placeholder {
        println!("\n提示：endpoint 仍是占位值，请用 --endpoint 重新生成。");
    }
    Ok(())
}

/// `init --write` owns `capabilities/delta-update.json`. If that file was left
/// untouched by an earlier run (or only covers some windows), the commands are
/// denied at runtime with no hint, so say exactly what to edit.
fn report_capability_coverage(dir: &Path, permission_set: &str) {
    let path = dir.join("src-tauri/capabilities/delta-update.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return;
    };
    let grants = value["permissions"].as_array().is_some_and(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .any(|item| item.starts_with("delta-update:"))
    });
    if !grants {
        println!(
            "警告：{} 的 `permissions` 缺少 delta-update 权限，前端调用会被拒绝；把 \"delta-update:{permission_set}\" 加入该数组。",
            path.display()
        );
        return;
    }
    let covered: Vec<&str> = value["windows"]
        .as_array()
        .map(|windows| windows.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let missing: Vec<String> = window_labels(dir)
        .into_iter()
        .filter(|label| !covered.contains(&label.as_str()))
        .collect();
    if !missing.is_empty() {
        println!(
            "警告：{} 只覆盖窗口 {covered:?}；窗口 {missing:?} 的更新命令会被拒绝。把缺失的标签加入该文件的 `windows`，或把 \"delta-update:{permission_set}\" 加到覆盖这些窗口的 capability。",
            path.display()
        );
    }
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
