# tauri-plugin-delta-update

Tauri 2 插件：把引擎的生命周期接到应用启动/退出，暴露十一个命令与状态事件。

> **仅支持 Windows 10+ 桌面**（per-user / per-machine / 便携版）。macOS 签名 `.app`、Linux AppImage/deb/rpm/Flatpak/Snap 不在范围内；非 Windows 目标在编译期直接失败（`compile_error!`），运行期 `ensure_supported()` 再返回 `E_UNSUPPORTED_PLATFORM` 作为纵深。当前版本为 `1.1.0`。

## 接入

```bash
cargo add tauri-plugin-delta-update
pnpm add tauri-plugin-delta-update-api
```

```rust
use tauri_plugin_delta_update::BuilderExt;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_delta_update::init())
        // 替换 .run(context)：若是替换/恢复 worker，在创建窗口前处理并退出。
        .run_with_delta_update(tauri::generate_context!())
        .expect("error while running app");
}
```

入口必须用 `run_with_delta_update`（或自行在 builder 之前调用 `run_applier_from_context`）——漏接时更新能准备成功，但重启进 worker 的那一步永远不会执行；debug 构建会直接 panic 提醒。

在 `capabilities/default.json` 中授予 `delta-update:manage-confirm`（`init` 默认写入，含 `confirm_healthy`）。配置写在 `tauri.conf.json` 的 `plugins.delta-update` 下。需要只读时改用 `delta-update:monitor`（仅 `check`/`status`/`get_update_history`）；需要“前端不能确认健康”的严格划分时用 `delta-update:manage`。默认 `auto_confirm=false`：用 `manage` 且前端不调用 `confirm_healthy` 时，应设置 `auto_confirm: true` 作为兜底，或直接使用默认的 `manage-confirm`。

## 命令与状态

`check`、`prepare`、`cancel_prepare`、`status`、`confirm_healthy`、`restart_to_apply`、`skip_version`、`export_diagnostics`、`open_diagnostics_folder`、`get_update_history`、`clear_skipped_version`；状态通过 `delta-update://state` 广播。

- 默认 `auto_check=true`、`auto_apply=false`、`auto_confirm=false`：后台只检查/准备；静默替换与自动健康确认必须显式开启。
- 配置/平台错误会禁用更新功能并广播 `Failed`，**不会阻止主应用启动**。
- `Downloading` 的 `progress` 是累计字节，`Preparing` 是已暂存文件数；事件里的 `progress.unit` 会明确标注 `bytes`/`files`/`ops`；插件以 100ms 窗口合并高频事件。
- 启动期自动回滚会发出 `RolledBack`，UI 可以提示“已回滚到 vX”。
- `check` 返回清单中的 `notes` 与 `mandatory`，可用于更新说明和强制更新提示。
- `auto_check` 只在启动时执行一次；需要长期运行时更新，配置 `check_interval_secs`（最小 60 秒）。
- 企业网络可配置 `proxy`（HTTP/SOCKS）、`headers`（仅清单/签名请求）和 `signing_headers`（仅预签名服务）；两者都不会发送到对象下载主机。

## 前端

推荐使用框架无关的控制器，避免自己维护快照和事件订阅生命周期：

```typescript
import { createUpdateController, describeUpdate } from 'tauri-plugin-delta-update-api';

const updater = createUpdateController();
updater.subscribe((state) => {
  if (describeUpdate(state).isReady) showUpdateReadyBanner(state?.version ?? '');
});
await updater.start();
// updater.check() / prepare() / cancel() / confirm() / restart()

// React 18+：import { useUpdater } from 'tauri-plugin-delta-update-api/react';
```

完整步骤见 [开发者接入指南](../../docs/guides/getting-started.md)。

## 日志

插件只发 `tracing` 事件，默认不安装 subscriber。宿主没装日志时（例如尚未接入 `tauri-plugin-log`），启用 `logging` feature 即可看到插件日志：

```toml
tauri-plugin-delta-update = { version = "1.1.0", features = ["logging"] }
```

`init()` 与 `run_applier_from_context()` 会自动初始化；级别由 `DELTA_UPDATE_LOG` 控制（`error`/`warn`/`info`/`debug`/`trace`，默认 `info`）。如果宿主已经通过 `tracing-subscriber` 安装了 subscriber，这里不会覆盖。宿主用的是 `tauri-plugin-log`（管理 `log` crate）时，插件日志会输出到 stderr，应用日志文件不受影响；要把 `tracing` 事件写进同一个文件，仍需 `tracing` 的 `log` 特性或 `tracing-log` 桥接。

## 已知边界

- 文件级更新不会改写 NSIS 写入的“应用和功能”`DisplayVersion`；需要同步时由应用在确认健康后自行更新，或用新安装包覆盖安装。
- `check` 会扫描并哈希安装根以计算差异，超大安装目录的首次检查有 I/O 成本；之后按 `size + mtime` 复用缓存的哈希，只重新哈希变化的文件。
- prepare、commit/recovery 共享跨进程 `update.lock`；第二个实例的 prepare 返回 `E_UPDATE_LOCKED`。仍建议使用 Tauri single-instance 插件保证单实例运行。
- 单个大文件（≥ 8 MiB）按 8 MiB 分块、最多 4 路并发 Range 下载；服务端不支持 Range 时自动回退单流。多文件之间仍按 `max_concurrent_downloads` 并发。
- 差分按文件生成：超过发布端 `--patch-max-file-size` 的文件直接走全量，不做文件内分块差分。
