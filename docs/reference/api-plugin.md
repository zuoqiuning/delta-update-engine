# Tauri 插件 API

> 本文件是 [API 参考](api.md) 的插件部分：Tauri 命令、事件、权限集与前端 SDK。

## 3. tauri-plugin-delta-update 公开 API

### 3.1 插件注册与配置

```rust
pub fn init<R: Runtime>() -> TauriPlugin<R, Config>;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Config {
    pub config_version: u32,
    pub endpoint: String,
    pub signing_public_keys: Vec<String>,
    #[serde(default = "default_true")]
    pub auto_check: bool,
    #[serde(default = "default_true")]
    pub auto_apply: bool,
    #[serde(default = "default_true")]
    pub auto_confirm: bool,
    #[serde(default = "default_confirm_delay")]
    pub confirm_delay_ms: u64,
    #[serde(default = "default_channel")]
    pub channel: String,
    pub signing_endpoint: Option<String>,
    pub allowed_hosts: Option<Vec<String>>,
    #[serde(default = "default_crash_loop_threshold")]
    pub crash_loop_threshold: u8,
    #[serde(default = "default_exit_grace_period")]
    pub exit_grace_period_secs: u64,
    pub install_dir: Option<PathBuf>,
    pub staging_dir: Option<PathBuf>,
    pub journal_dir: Option<PathBuf>,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_downloads: usize,
    pub staging_size_limit: Option<u64>,
    pub max_patch_file_size: Option<u64>,
    pub patch_memory_factor: Option<f32>,
    pub manifest_max_bytes: Option<u64>,
    pub max_files: Option<usize>,
    #[serde(default)]
    pub allow_insecure_http: bool,
}
```

配置字段的完整语义见协议文档 §9。**Config 结构体不启用 `deny_unknown_fields`**，以便未来新增字段时旧客户端可以忽略；但未知字段会输出警告日志，帮助发现拼写错误。`config_version` 低于当前实现时插件启动失败并返回 `E_CONFIG_MIGRATION_REQUIRED`；配置非法返回 `E_CONFIG_INVALID`。

**信任根**（三选一，可叠加收窄）：内嵌 `signing_public_keys`（默认）→ 构建期 `pinned-keys`（`config ∩ DELTA_UPDATE_PINNED_PUBLIC_KEYS`，为空拒绝启动）→ 运行期 `set_trusted_keys(keys, mode)`：

```rust
pub enum TrustMode { Intersect, PinnedOnly }
pub fn set_trusted_keys(keys: Vec<String>, mode: TrustMode) -> Result<(), EngineError>;
```

`Intersect` 只保留内嵌（或已 pin）集合里也存在的公钥，交集为空即拒绝启动；`PinnedOnly` 用宿主集合替代前面的结果，是唯一可能放宽信任根的模式。两者都作用于 `pinned::effective_keys` 这一处解析，因此 GUI 与替换 worker 拿到同一集合；必须在 builder 运行前调用，且信任根一旦解析就冻结——第二次调用或解析之后的“迟到调用”都返回 `E_CONFIG_INVALID`，不会只改到一半。

### 3.2 Tauri 命令

插件暴露 11 个命令。**没有任何命令接收文件路径，也没有“替换任意文件”的命令**——Commit 由替换进程在应用退出后执行，`restart_to_apply` 只能消费已通过全部校验的 `Prepared` 状态。唯一接收参数的是 `get_update_history` 的 `limit`；`skip_version` 的版本由插件从 `check` 结果取，不接受前端输入。

**`check`**

```typescript
invoke<CheckResult>('plugin:delta-update|check')
```

```rust
pub struct CheckResult {
    pub available: bool,
    pub version: Option<String>,
    pub current_version: String,
    pub files_changed: u32,
    pub files_staged: u32,
    pub download_bytes: u64,
    pub total_bytes: u64,
    pub notes: Option<String>,
    pub mandatory: bool,
    pub update_required: bool,
    pub min_supported_version: Option<String>,
    pub cloud_placeholders_skipped: u32,
    /// 用户跳过该版本时返回被跳过的版本号，`available` 仍为 false。
    pub skipped_version: Option<String>,
}
```

拉取清单、验签、比对。**不下载补丁**。`available` 为 `false` 时其余字段仍填充（`version` 为 `None`），便于 UI 展示“已是最新”。`skipped_version` 在用户调用过 `skip_version` 且远程仍是同一版本时给出被跳过的版本号（`version` 仍为 `None`、`available` 为 `false`），UI 可据此提供“仍要更新”并调用 `clearSkippedVersion()`。`cloud_placeholders_skipped` 是本次扫描中被跳过的云占位文件数（OneDrive/Files On-Demand）：它们不参与 diff，若清单仍列出则按全量下载，UI 可用它解释“为什么下载量比预期大”。`update_required` 为 `true` 时表示本地版本低于同一通道清单的 `min_supported_version`：插件只上报事实，**不阻止应用运行**；应用可以选择显示模态框或禁用功能，直到更新完成。与 `prepare` 互斥：`prepare` 进行中调用返回 `E_INVALID_TRANSITION`。检查失败时若已有 `Prepared` 或未确认的 `Committed`，保留原状态，不覆盖为 `Failed`。

**`prepare`**

```typescript
invoke<PrepareResult>('plugin:delta-update|prepare')
```

```rust
pub struct PrepareResult {
    pub state: String,      // 正常为 "Prepared"；已有 prepare 进行中时为当前状态名
    pub version: String,
}
```

执行完整的 prepare 阶段，目标版本由最近一次 `check` 验签通过的清单确定；若此前没有验签清单，先执行一次 `check`。**不接收版本参数**——前端无法让客户端准备一个未经校验的任意版本。已有同版本 `Prepared` 时直接复用；正在 prepare 时返回当前状态而不是启动第二个下载。等待健康确认的 `Committed` 状态下拒绝新的 prepare，返回 `E_INVALID_TRANSITION`。失败返回协议文档 §7.2 的错误对象。

**`cancel_prepare`**

```typescript
invoke<{ state: 'Cancelled' }>('plugin:delta-update|cancel_prepare')
```

中止正在进行的下载。已写入的临时文件保留，供下次断点续传；已完成的 `Prepared` 不受影响。

**`status`**

```typescript
invoke<StatusPayload>('plugin:delta-update|status')
```

返回当前状态快照，结构同事件 payload（§3.3）。

**`confirm_healthy`**

```typescript
invoke<{ acknowledged: boolean }>('plugin:delta-update|confirm_healthy')
```

标记当前运行版本健康。重置启动尝试计数。触发旧备份清理。**只有它被调用，`.old` / `.trash` 才允许物理删除**。`auto_confirm` 默认 `false`；只有显式开启时，插件层才会在主窗口页面加载后延迟调用一次作为兜底。成功确认后状态回到 `Idle`。

**`restart_to_apply`**

```typescript
invoke<{ state: 'Committing'; version: string }>('plugin:delta-update|restart_to_apply')
```

仅当状态为 `Prepared` 时有效。插件 spawn 替换进程并退出应用；替换进程在应用退出后执行 Journal，然后拉起新版本。无 `Prepared` 更新时返回 `E_NOT_PREPARED`，**不退出应用**。

**`export_diagnostics`**

```typescript
invoke<{ path: string }>('plugin:delta-update|export_diagnostics')
```

把诊断包写到应用日志目录（`app_log_dir()/delta-update-diagnostics-<unix>.json`）并返回路径。内容与 `delta-update inspect --export` 相同：状态报告、`worker.log` 尾部 64 KiB、journal 目录文件清单、系统信息与安装形态。**目标路径由插件决定，前端不能指定**，因此该命令无法用于写任意文件。该命令**始终脱敏**：`install_root`、`staging_root`、`journal_root` 与用户主目录前缀会替换为 `<install_root>` 等单位置，`redacted` 字段为 `true`，可直接附到工单。

**`open_diagnostics_folder`**

```typescript
invoke<void>('plugin:delta-update|open_diagnostics_folder')
```

在资源管理器中打开 `app_log_dir()`，方便用户直接选中刚导出的诊断包。路径由插件决定、不接受参数，因此无法用于打开任意目录。

**`clear_skipped_version`**

```typescript
invoke<{ cleared: boolean }>('plugin:delta-update|clear_skipped_version')
```

清除回滚后写入的版本跳过标记。发布者修复问题并**重新发布同一个版本号**时，`check` 会一直跳过它（除非清单带 `rollback: true`）；这个命令是唯一的手动出口。`cleared` 表示此前是否存在标记。该命令只在 `manage`/`default` 权限集中，`monitor` 不包含。它不是“跳过所有更新”的开关：只清除回滚写入的那一个版本标记，`check` 仍会正常拉取其他版本。

**`skip_version`**

```typescript
invoke<{ skipped: string }>('plugin:delta-update|skip_version')
```

把最近一次 `check` 找到的版本标记为“稍后提醒”：写入与回滚共用的 `skipped_version`，`check` 在下一次更高版本出现前返回 `available: false`，并在 `skipped_version` 中回传被跳过的版本号。命令不接受版本参数（版本由插件从 `check` 结果中取，前端不能指定），因此不能用来跳过任意版本。

拒绝两种情况并返回 `E_INVALID_TRANSITION`：`min_supported_version` 生效的强制更新（不允许跳过），以及已经 `Prepared` 的更新（应先 `cancel_prepare`，否则 UI 会同时显示“已跳过”和“可安装”）。成功后状态回到 `Idle`，`get_target_version` 清空。该命令在 `manage`/`default` 中，`monitor` 不包含。

**`get_update_history`**

```typescript
invoke<UpdateRecord[]>('plugin:delta-update|get_update_history', { limit: 50 })
```

读取 `journal/update_history.jsonl` 最近的终态记录（`completed` / `rolled_back` / `failed`），默认 50 条、上限 500。记录只在本地追加，**插件不上报任何数据**；应用可自行转发到自己的监控系统。`limit` 省略时按 50 处理。

```typescript
interface UpdateRecord {
  v: number;
  version: string;
  from_version?: string | null;
  started_at_unix?: number | null;
  finished_at_unix: number;
  result: 'completed' | 'rolled_back' | 'failed';
  duration_ms?: number | null;
  reason?: string | null;     // rolled_back 的触发原因
  error_code?: string | null; // failed 的 EngineError::code()
}
```

该命令只读，包含在 `monitor`/`manage`/`default` 三个权限集中。

### 3.3 事件

事件名：`delta-update://state`，通过 Tauri 标准事件系统广播。

```typescript
import { listen } from '@tauri-apps/api/event';

interface StatePayload {
  v: number;             // 事件协议版本，当前 1
  state: string;         // 状态枚举名
  version: string | null; // 目标/相关版本；下载、准备、提交与 Failed 时为本次目标版本
  progress: { total: number; completed: number; unit?: 'bytes' | 'files' | 'ops'; file?: string | null } | null;
  error: ErrorPayload | null;
  min_supported_version?: string | null; // 仅 UpdateRequired 状态
  timestamp: string;     // RFC 3339 UTC
}
```

`ErrorPayload` 结构：

```typescript
interface ErrorPayload {
  code: string;          // 稳定码值，如 "E_NETWORK_TIMEOUT"
  category: string;      // "network" | "signature" | "disk" | "permission" | ...
  message: string;       // 开发者可读
  retryable: boolean;
  action: ErrorAction;   // "retry" | "wait" | "check_update" | "contact_support" | "grant_permission"
                         // | "free_space" | "update_client" | "report_security" | "recover_backup"
                         // | "fix_config" | "unsupported" | "restart"
  user_action: string;   // 面向用户的建议（中文，可直接展示）
  detail: string;        // 结构化上下文 `key=value; key=value`，无结构化值时为空
}
```

前端只读事件，**不参与决策**。事件中的 `error` 仅在 `state === 'Failed'` 时非空；`progress` 仅在 `Downloading` / `Preparing` / `Committing` 时非空；这些状态的 `version` 是本次目标版本，UI 可直接用于进度文案。`UpdateRequired` 是稳定状态：应用低于发布者最低支持版本时会停在这里，`min_supported_version` 与 `version` 同时给出，直到更新已准备或下一次检查判定不再需要。

### 3.4 Rust 侧访问

当前版本**没有**独立的 `DeltaUpdaterExt` / `DeltaUpdater` Rust facade；插件对外稳定接口是 `check` / `prepare` / `cancel_prepare` / `status` / `confirm_healthy` / `restart_to_apply` / 诊断与历史命令，以及 `delta-update://state` 事件。应用 Rust 代码如需在内部触发更新，推荐通过前端调用命令，或在宿主中自行封装 `AppHandle` 与命令调用。不要把 `PluginState` 的内部字段当作稳定 API——它没有 SemVer 承诺。

> 文档早期版本曾描述 `DeltaUpdaterExt`，该 API 从未实现；这里明确移除，避免二次开发者按不存在的接口编写代码。

### 3.5 默认权限标识

```toml
# permissions/default.toml
[default]
description = "Default permissions for delta-update"
permissions = [
    "allow-check",
    "allow-prepare",
    "allow-cancel-prepare",
    "allow-status",
    "allow-confirm-healthy",
    "allow-restart-to-apply",
    "allow-export-diagnostics",
    "allow-open-diagnostics-folder",
    "allow-get-update-history",
    "allow-clear-skipped-version",
]
```

使用 `build.rs` 的 `tauri_plugin::Builder` 自动生成 `allow-<command>` 权限标识。**scope 不需要复杂定义**——命令不接收文件路径参数，路径由引擎内部从当前 exe 位置解析，前端无法传入任意路径。

除兼容集 `default` 外还有三个预定义集合，可用于收窄前端能力面：

| 权限标识 | 包含命令 | 适用场景 |
| -------- | -------- | -------- |
| `delta-update:manage-confirm` | `manage` 的全部命令 + `confirm_healthy` | **`init` 默认写入**；前端可直接调用 `confirm_healthy()` |
| `delta-update:monitor` | `check`、`status`、`get_update_history` | 只展示更新状态与历史，不触发下载或重启 |
| `delta-update:manage`  | `check`、`prepare`、`cancel_prepare`、`status`、`restart_to_apply`、`export_diagnostics`、`open_diagnostics_folder`、`get_update_history`、`clear_skipped_version`、`skip_version` | 能触发更新但**不能**代替应用确认健康 |
| `delta-update:default` | 全部 11 个命令（与 `manage-confirm` 当前等价） | 兼容旧接入；生产环境建议显式使用 `manage-confirm` |

`delta-update init --permission-set monitor|manage|manage-confirm|default` 可直接生成对应的 capability；**默认是 `manage-confirm`**（`manage` + `confirm_healthy`），所以前端按文档调用健康确认不会被 ACL 拒绝。需要“前端不能确认健康”的严格划分时显式使用 `manage`，此时由 `auto_confirm: true` 或 Rust 侧负责确认。

### 3.6 前端 SDK（`tauri-plugin-delta-update-api`）

npm 包是命令与状态的薄封装，无额外运行时依赖（React 仅作为 `react` 子路径的可选 peer）：

| 导出 | 用途 |
| --- | --- |
| `check` / `prepare` / `cancelPrepare` / `status` / `confirmHealthy` / `restartToApply` / `skipVersion` / `exportDiagnostics` / `openDiagnosticsFolder` / `clearSkippedVersion` / `getUpdateHistory` | 与命令一一对应，返回的类型见 §3.2 |
| `createUpdateController()` | 启动快照 + 事件订阅（引用计数）+ 生命周期命令（`check` / `prepare` / `cancel` / `confirm` / `restart` / `skip` / `clearSkip`），框架无关 |
| `describeUpdate(state)` | 派生 `isReady` / `canPrepare` / `canRestart` / `canCancel` / `isBusy` / `isUpdateRequired` / `error`，与 React hook 共用同一实现 |
| `isPrepared` / `isCommitted` / `isRolledBack` / `isFailed` / `isUpdateRequired` / `isBusy` | 单状态类型守卫（带谓词收窄） |
| `STATE_EVENT` / `STATE_NAMES` | `delta-update://state` 与全部状态名 |
| `useUpdater()`（`react` 子路径） | `useSyncExternalStore` + controller，返回值包含 `describeUpdate` 的全部字段 |

`prepare()` 返回 `state: 'Prepared'` 才代表产物可用；`restartToApply()` 成功后进程会被替换并重启，因此不要在它之后继续依赖当前进程的状态。UI 判断优先用 `describeUpdate`，不要在业务代码里散落字符串比较——规则变更时只需升级这个包。`skip()` 后的“仍然更新”由 `check()` 的 `skipped_version` 与 `clearSkip()` 组成，与命令一一对应，不需要额外状态。

