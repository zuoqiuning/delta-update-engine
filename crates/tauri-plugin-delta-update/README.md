# tauri-plugin-delta-update

Tauri 2 插件：把引擎的生命周期接到应用启动/退出，暴露六个命令与状态事件。

> 0.1.0 目前尚未发布到 crates.io/npm；在仓库内请使用 path/git 依赖，正式发布后下面的安装命令才生效。

## 接入

```bash
cargo add tauri-plugin-delta-update
pnpm add tauri-plugin-delta-update-api
```

```rust
fn main() {
    // Worker mode: the same binary performs the replacement or startup
    // recovery without loading Tauri. Forgetting this line fails setup loudly.
    if let Some(code) = tauri_plugin_delta_update::run_applier_from_args() {
        std::process::exit(code);
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_delta_update::init())
        .run(tauri::generate_context!())
        .expect("error while running app");
}
```

在 `capabilities/default.json` 中授予 `delta-update:default`。配置写在 `tauri.conf.json` 的 `plugins.delta-update` 下，字段见 [协议文档 §9](../../docs/design/protocol.md)。

## 命令与状态

`check`、`prepare`、`cancel_prepare`、`status`、`confirm_healthy`、`restart_to_apply`；状态通过 `delta-update://state` 广播。

- `Downloading` 的 `progress` 是累计字节，`Preparing` 是已暂存文件数；插件以 100ms 窗口合并高频事件。
- 启动期自动回滚会发出 `RolledBack`，UI 可以提示“已回滚到 vX”。
- `check` 返回清单中的 `notes` 与 `mandatory`，可用于更新说明和强制更新提示。
- `auto_check` 只在启动时执行一次；需要长期运行时更新，配置 `check_interval_secs`（最小 60 秒）。
- 企业网络可配置 `proxy`（HTTP/SOCKS）和 `headers`（仅清单/签名请求，不会发送到对象下载主机）。

## 前端

推荐使用框架无关的控制器，避免自己维护快照和事件订阅生命周期：

```typescript
import { createUpdateController } from 'tauri-plugin-delta-update-api';

const updater = createUpdateController();
updater.subscribe((state) => {
  if (state?.state === 'Prepared') showUpdateReadyBanner(state.version);
});
await updater.start();
// updater.check() / prepare() / cancel() / confirm() / restart()

// React 18+：import { useUpdater } from 'tauri-plugin-delta-update-api/react';
```

完整步骤见 [开发者接入指南](../../docs/guides/getting-started.md)，权限模型见 [权限设计](../../docs/design/permission-model.md)。

## 已知边界

- 文件级更新不会改写 NSIS 写入的“应用和功能”`DisplayVersion`；需要同步时由应用在确认健康后自行更新，或用新安装包覆盖安装。
- `check` 会扫描并哈希安装根以计算差异，超大安装目录的首次检查有 I/O 成本；之后按 `size + mtime` 复用缓存的哈希，只重新哈希变化的文件。
- prepare、commit/recovery 共享跨进程 `update.lock`；第二个实例的 prepare 返回 `E_UPDATE_LOCKED`。仍建议使用 Tauri single-instance 插件保证单实例运行。
- 单个大文件按 Range 顺序下载，不做文件内多线程分块；多文件之间按 `max_concurrent_downloads` 并发。
- 差分按文件生成：超过发布端 `--patch-max-file-size` 的文件直接走全量，不做文件内分块差分。
