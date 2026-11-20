# 从官方 `tauri-plugin-updater` 迁移

先运行计划生成器，它读取 `src-tauri/tauri.conf.json` 并打印需要手工替换的四类内容：

```bash
delta-update migrate --dir .
```

下面的 diff 就是它输出的完整版，方便对照修改。

## 1. `src-tauri/Cargo.toml`

```diff
- tauri-plugin-updater = "2"
+ tauri-plugin-delta-update = { version = "0.1", features = [] }
```

`delta-update-engine` 会在插件之后作为传递依赖出现，不需要单独声明。

## 2. `src-tauri/src/main.rs`

```diff
  use tauri::Manager;
+ use tauri_plugin_delta_update::BuilderExt;

  fn main() {
      tauri::Builder::default()
-         .plugin(tauri_plugin_updater::Builder::new().build())
+         .plugin(tauri_plugin_delta_update::init())
          .setup(|app| { /* ... */ Ok(()) })
-         .run(tauri::generate_context!())
+         .run_with_delta_update(tauri::generate_context!())
          .expect("error while running tauri application");
  }
```

`run_with_delta_update` 会在 GUI 启动前处理 `--delta-update-apply` / `--delta-update-recover`，替换进程不会再误开窗口。

## 3. `src-tauri/tauri.conf.json`

```diff
  "plugins": {
-   "updater": {
-     "endpoints": ["https://releases.example.com/{{target}}/{{arch}}/{{current_version}}"],
-     "pubkey": "RWQ..."
-   }
+   "delta-update": {
+     "config_version": 1,
+     "endpoint": "https://releases.example.com/manifests/latest.json",
+     "signing_public_keys": ["RWQ..."],
+     "channel": "stable"
+   }
  }
```

官方 updater 的 `{{target}}/{{arch}}/{{current_version}}` 模板与按平台分发的 `latest.json` **不能复用**：delta 的清单是单文件、跨平台共享同一份 `manifests/latest.json`，由 `channel` 区分通道。

## 4. `src-tauri/capabilities/default.json`

```diff
  "permissions": [
    "core:default",
-   "updater:default"
+   "delta-update:manage-confirm"
  ]
```

`manage-confirm` 是 `init` 的默认集合（`manage` + `confirm_healthy`），前端可直接调用健康确认；需要“前端不能确认健康”的严格划分时用 `delta-update:manage`，只读展示用 `delta-update:monitor`。也可以直接生成：

```bash
delta-update init --dir . --endpoint https://releases.example.com/manifests/latest.json \
  --public-key RWQ... --write
```

## 5. 前端调用对应关系

| 官方 updater | delta-update | 差异 |
| --- | --- | --- |
| `check()` | `check()` | 返回 `CheckResult`，`available`/`version`/`download_bytes` 等字段显式列出 |
| `update.downloadAndInstall()` | `prepare()` + `restartToApply()` | 下载与安装分离，安装前必须由应用确认状态 |
| `update.download(...)` | `prepare(...)` | 无进度回调参数，进度通过 `delta-update://state` 事件 |
| `update.install()` | `restartToApply()` | 只消费已校验的 `Prepared` 状态 |
| — | `confirmHealthy()` | 新版本首次启动后由应用确认健康，否则按崩溃循环处理 |
| — | `exportDiagnostics()` | 支持工单：导出脱敏诊断包 |

前端封装见 [`tauri-plugin-delta-update-api`](../../packages/plugin-api/README.md)；最小可运行示例见 [`minimal-tauri-app`](../minimal-tauri-app/README.md)。

`skipVersion()` / `clearSkippedVersion()` 与 `getUpdateHistory()` 没有官方 updater 的对应物：前者实现“稍后提醒”，后者给应用提供最近几次更新结果。`check()` 在用户跳过某版本后会同时返回 `available: false` 与 `skipped_version`，据此渲染“仍然更新”即可。

## 6. 发布侧

迁移完成后，官方 updater 的发布脚本（生成 `latest.json` + 平台包）整体替换为：

```bash
delta-update release --from build/1.0.0 --to build/1.1.0 \
  --version 1.1.0 --from-version 1.0.0 \
  --bucket cos://your-bucket --key release.key --verbose
delta-update verify --bucket cos://your-bucket --version 1.1.0 --public-key release.pub
delta-update inspect --from-release cos://your-bucket --version 1.1.0 --public-key release.pub
```

`release --self-test` 可以在发布前先演练一次 apply + rollback，避免把坏补丁推上去。
