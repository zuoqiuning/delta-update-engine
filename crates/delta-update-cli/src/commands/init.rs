use std::path::{Path, PathBuf};

use anyhow::Context;
use delta_update_engine::crypto::validate_public_key;
use serde_json::Value;
use url::Url;

use crate::cli::InitArgs;

const PLACEHOLDER_ENDPOINT: &str =
    "https://your-bucket.cos.ap-guangzhou.myqcloud.com/manifests/latest.json";
const PLACEHOLDER_KEY: &str = "RWQ...your-minisign-public-key...";

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
        keys = vec![PLACEHOLDER_KEY.to_string()];
    }
    let keys_are_placeholder = keys.len() == 1 && keys[0] == PLACEHOLDER_KEY;

    if let Some(existing) = &existing {
        println!(
            "已从 {} 读取默认值（{}）。\n",
            existing.source.display(),
            notes.join("、")
        );
    }

    let config = serde_json::json!({
        "plugins": {
            "delta-update": {
                "config_version": 1,
                "endpoint": endpoint,
                "signing_public_keys": keys,
                "channel": args.channel.clone(),
                "auto_check": true,
                "auto_apply": true,
                "auto_confirm": true
            }
        }
    });
    let capabilities = serde_json::json!({
        "identifier": "default",
        "windows": ["main"],
        "permissions": ["core:default", "delta-update:default"]
    });

    println!("将以下配置合并到 src-tauri/tauri.conf.json：\n");
    println!("{}\n", serde_json::to_string_pretty(&config)?);
    println!("将以下权限合并到 src-tauri/capabilities/default.json：\n");
    println!("{}\n", serde_json::to_string_pretty(&capabilities)?);
    println!("接入检查：");
    println!("1. 在 main 开头调用 run_applier_from_args()，worker 模式不加载 WebView。");
    println!("2. 前端安装 tauri-plugin-delta-update-api，并订阅 delta-update://state。");
    println!("3. 用 delta-update release 发布清单、签名与对象。");
    if endpoint_is_placeholder || keys_are_placeholder {
        println!("\n提示：当前输出包含占位值，请用 --endpoint 与 --public-key 重新生成。");
    }
    Ok(())
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
