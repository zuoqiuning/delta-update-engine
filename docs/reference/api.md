# Delta Update Engine API 参考

> 本文是引擎、插件与 CLI 的公开接口查阅手册。所有公开类型、trait、命令、配置、事件和错误码在此集中定义。任何实现与本文不一致时，以本文为准；任何接口变更必须先更新本文，再改代码。本文不解释“为什么这样设计”——设计理由见架构文档与功能机制文档。

## 1. Crate 结构与 API 稳定性

系统由三个 crate 构成，公开接口的稳定性承诺不同：

| Crate                       | 类型               | 稳定性                                                       | 面向读者                                  |
| --------------------------- | ------------------ | ------------------------------------------------------------ | ----------------------------------------- |
| `delta-update-engine`       | 纯 Rust 库         | 遵循 SemVer，但**默认不承诺跨 MINOR 版本的 trait 兼容**（见 §1.2） | 二次开发者、需要脱离 Tauri 使用引擎的场景 |
| `tauri-plugin-delta-update` | Tauri 2 插件 crate | 遵循 SemVer，命令/配置/事件受协议文档约束                    | Tauri 应用开发者                          |
| `delta-update-cli`          | 构建侧二进制       | 命令行参数遵循 SemVer；输出格式**不承诺稳定**                | 发布工程师                                |

### 1.1 命名规范

遵循 Tauri 2 官方约定：Rust crate 名为 `tauri-plugin-delta-update`，NPM 包名固定为 `tauri-plugin-delta-update-api`，与 crate 版本严格同步。插件标识符 `delta-update` 符合 Tauri 的 identifier 语法（小写 ASCII + 连字符）。**仓库与核心 crate 同名 `delta-update-engine`，但 identifier 采用短名 `delta-update`**——Tauri 的权限字符串、事件名和配置键都以 identifier 为准，短名避免出现 `updater:default` 这种容易被误读为官方插件的形态。

### 1.2 引擎 trait 的兼容性策略

`Fetcher`、`FileSystem` 等核心 trait 标记为 `#[non_exhaustive]` 或通过 sealed trait 模式限制外部实现。**新增方法不破坏 MINOR 兼容**，但**修改方法签名属于 MAJOR 变更**。如果二次开发者需要自定义实现，应通过引擎提供的构造器注入，而非直接实现 trait——除非显式启用了 `unstable-impl` feature。

## 2. delta-update-engine 公开 API

### 2.1 顶层入口类型

```rust
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct EngineConfig {
    pub endpoint: Url,
    pub signing_public_keys: Vec<String>,
    pub channel: String,
    /// 运行中二进制的编译期版本（Tauri: `app.package_info().version`）。
    pub current_version: String,
    pub install_root: PathBuf,
    pub staging_root: PathBuf,
    pub journal_root: PathBuf,
    pub limits: Limits,
    pub fetcher: Arc<dyn Fetcher>,
    /// 仅本地联调：允许回环地址使用明文 HTTP，默认 false。
    pub allow_insecure_http: bool,
    /// 连续未确认启动多少次后回滚，默认 2，必须 ≥ 1。
    pub crash_loop_threshold: u32,
}

pub struct Engine { /* private */ }

impl Engine {
    /// 构造引擎实例。校验配置与路径，不执行网络 I/O。
    pub fn new(config: EngineConfig) -> Result<Self, EngineError>;

    /// 拉取清单、验签、校验通道/版本/资源上限，返回是否有更新。
    /// 不下载补丁。
    pub async fn check(&self) -> Result<CheckOutcome, EngineError>;

    /// 从已解析的本地根目录和远程清单计算差异。
    /// 纯计算，无副作用。
    pub fn diff(
        &self,
        local_root: &Path,
        remote_manifest: &Manifest,
    ) -> Result<DiffResult, EngineError>;

    /// 执行 prepare 阶段。写入暂存区，不触碰根目录。
    /// 目标版本来自最近一次通过验签的 `check`；没有缓存时先执行一次 `check`。
    /// 暂存与 PREPARED 持久化期间持有跨进程 `update.lock`。
    pub async fn prepare(
        &self,
        progress: Box<dyn Fn(Progress) + Send + Sync>,
    ) -> Result<PreparedUpdate, EngineError>;

    /// 取消进行中的 prepare；已写入的临时文件保留供续传。
    pub fn cancel_prepare(&self) -> Result<(), EngineError>;

    /// 主进程在退出前调用：写入 apply.json 与 Journal 并 flush（提交点）。
    /// 文件操作只由替换进程执行。
    pub fn begin_commit(&self, prepared: &PreparedUpdate) -> Result<PendingCommit, EngineError>;

    /// 撤销一个替换进程尚未启动的提交：删除 Journal 与 apply.json，
    /// 保留 PREPARED 供下次启动复用。已有 commit.status 时不做任何事。
    pub fn discard_pending_commit(&self) -> Result<(), EngineError>;

    /// 健康确认：重置启动尝试计数，清理 .old/.trash、Journal、PREPARED。
    pub fn confirm_healthy(&self) -> Result<(), EngineError>;

    /// 启动时的恢复入口：重放未完成 Journal、崩溃回滚、清理残留、复用 PREPARED。
    /// 不承担正常提交。每次调用计入一次启动尝试。
    pub fn recover_and_cleanup(&self) -> Result<StartupOutcome, EngineError>;

    /// 本次启动是否需要写安装根：存在待重放/待回滚 Journal，或已完成的提交
    /// 超过崩溃循环阈值。正常的“已提交待确认”启动返回 `false`。
    pub fn recovery_needs_install_write(&self) -> Result<bool, EngineError>;

    /// 提权 worker 已替本次启动完成恢复后，复用同一次启动计数检查结果，
    /// 不再增加尝试次数。
    pub fn recover_after_elevation(&self) -> Result<StartupOutcome, EngineError>;
}
```

`Engine` 是引擎的主要句柄。它**不持有 Tauri 的任何类型**，也不感知窗口或事件系统。`prepare` 是异步的；其余方法要么很快完成，要么把文件操作委托给替换进程。

`check` 的安装树扫描、`prepare` 的未变化文件校验、暂存文件哈希与全量哈希校验都在阻塞线程池执行，不会占住异步运行时。暂存与 PREPARED 持久化期间持有跨进程 `update.lock`，第二个实例的 prepare 立即返回 `E_UPDATE_LOCKED`。

`Progress::Downloading { total, completed }` 是**累计字节数**：`total` 是本次计划下载总量，`completed` 随完成的文件单调递增；补丁回退到全量时可能超过计划值，引擎会截断到 `total`，保证进度单调且不超过 100%。`Progress::Verifying` 按已暂存文件计数，插件会把它映射为 `Preparing`，并以 100ms 为窗口合并事件，避免大文件集产生 IPC 风暴。

**`begin_commit` 的调用者**：插件只在退出/重启路径调用它，写入 Journal 后启动替换进程并退出。替换进程通过 `run_applier` 读取 `apply.json` 与 Journal 执行，不加载 WebView 或业务线程；`restart_to_apply` 路径额外传入 `--relaunch`，替换完成后拉起新版本。若替换进程未能启动（例如 UAC 被取消），插件调用 `discard_pending_commit` 撤销提交点并保留 PREPARED，下次启动直接复用。

**启动期提权恢复**：per-machine 安装的启动恢复可能需要写安装根。插件在调用 `recover_and_cleanup` 前先检查 `recovery_needs_install_write`；需要写且安装根不可写时，用同一二进制的 `--delta-update-recover <journal_dir> <crash_loop_threshold>` 模式提权执行恢复并等待其退出，再调用 `recover_after_elevation` 读取结果。worker 侧通过 `run_recovery` 入口校验 `apply.json` 的绝对路径、`install_root` 包含自身、staging/journal 不在安装根内。

**`EngineConfig` 是公开类型**：二次开发者可以脱离 Tauri 直接构造引擎。`Limits` 包含清单大小、文件数、补丁/全量上限、内存预算等，全部有安全默认值。`crash_loop_threshold` 控制连续未确认启动多少次后回滚，默认 `2`。

### 2.2 Manifest 相关类型

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct Manifest {
    pub schema: u32,
    pub version: String,
    pub channel: Option<String>,
    pub published_at: Option<String>,
    pub min_client_schema: u32,
    pub files: HashMap<String, FileEntry>,
    #[serde(default)]
    pub rollback: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct FileEntry {
    pub sha256: String,
    pub size: u64,
    pub strategy: Strategy,
    pub delta_key: Option<String>,
    pub delta_size: Option<u64>,
    pub full_key: Option<String>,
    pub executable: Option<bool>,
}

/// 策略在协议层是开放字符串。
/// 必须实现自定义 `Deserialize`：未知值进入 `Unknown`，而不是让整份清单解析失败。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub enum Strategy {
    Full,
    QbsdiffZstd,
    QbsdiffLzma2,
    HdiffZstd,
    HdiffLzma2,
    ZucchiniZstd,
    ZstdDict,
    Unknown(String),
}
```

所有结构体标记 `#[non_exhaustive]`，新增字段不破坏 SemVer。

**`Strategy` 的关键实现约束**：**不能**用普通 `#[derive(Deserialize)]` 的封闭枚举。serde 遇到未知字符串会返回错误，整份清单解析失败，与协议文档承诺的“未知策略回退全量”矛盾。正确做法是手写 `Deserialize`，把无法识别的值放入 `Unknown(String)`。

**没有 `KeyRotation` 类型**：信任集只存在于二进制内，运行期不新增公钥。清单中出现 `key_rotation` 时按未知字段忽略。

### 2.3 差异与 prepare 相关类型

```rust
#[derive(Debug)]
#[non_exhaustive]
pub struct CheckOutcome {
    pub available: bool,
    pub version: Option<String>,
    pub current_version: String,
    pub files_changed: u32,   // 包含删除
    pub files_staged: u32,    // 需要下载/应用的文件
    pub download_bytes: u64,
    pub total_bytes: u64,
    pub notes: Option<String>,
    pub mandatory: bool,
}

#[derive(Debug)]
#[non_exhaustive]
pub struct DiffResult {
    pub unchanged: Vec<String>,          // 相对路径
    pub modified: Vec<ModifiedFile>,
    pub added: Vec<String>,              // 直接走全量
    pub removed: Vec<String>,            // 移入 .trash
    pub untracked: Vec<String>,          // 不处理
    pub download_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug)]
#[non_exhaustive]
pub struct ModifiedFile {
    pub path: String,
    pub entry: FileEntry,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum Progress {
    Downloading { file: String, total: u64, completed: u64 },
    Applying { file: String },
    Verifying { file: String },
    Done,
}

#[derive(Debug)]
#[non_exhaustive]
pub struct PreparedUpdate {
    pub version: String,
    pub staging_root: PathBuf,
    pub manifest_hash: String,   // 用于 PREPARED 计划持久化
    /* private */
}

#[derive(Debug)]
#[non_exhaustive]
pub struct PendingCommit {
    pub commit_id: String,
    pub version: String,
    pub journal_path: PathBuf,
    /* private */
}

#[derive(Debug)]
#[non_exhaustive]
pub enum StartupOutcome {
    /// 没有需要恢复的状态。
    Clean,
    /// 重放了一份未完成的提交日志。
    Recovered { ops_completed: usize },
    /// 替换已完成，等待 `confirm_healthy`。
    Committed { version: String },
    /// 因崩溃循环执行了回滚。
    RolledBack { to_version: String, reason: String },
    /// 复用了上次已就绪的 PREPARED 计划。
    ReusedPrepared { version: String },
    /// 日志损坏或重放失败，需要人工介入。
    NeedsAttention { error: EngineError },
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Limits {
    pub manifest_max_bytes: u64,   // 默认 8 MiB
    pub max_files: usize,          // 默认 20 000
    pub max_patch_bytes: u64,      // 默认 1 GiB
    pub max_full_bytes: u64,       // 默认 4 GiB
    pub max_patch_file_size: u64,  // 默认 256 MiB
    pub patch_memory_factor: f32,  // 默认 2.5
    pub staging_size_limit: u64,   // 默认 2 GiB
    pub max_concurrent_downloads: usize, // 默认 4
}
```

`signing_public_keys` 只接受公钥文本（原始 base64 或 `.pub` 文件内容）。引擎公开 API 不出现 `minisign_verify` 类型，也不接受任何私钥材料。

`PreparedUpdate` 是 `commit` 的输入。它持有暂存区的根路径和清单哈希，**不持有远程连接或未完成的下载任务**。如果用户本次不退出，`PreparedUpdate` 的信息被持久化为 `PREPARED` 计划，下次启动时 `recover_and_cleanup()` 可以直接重建它，无需重新 prepare。

`StartupOutcome` 取代了把 Journal 结果直接暴露给上层的做法：启动阶段的恢复可能同时涉及日志重放、回滚、清理与缓存复用，单一枚举让插件层能够准确决定要广播哪个状态。

### 2.4 Fetcher trait

```rust
#[async_trait]
pub trait Fetcher: Send + Sync {
    /// 获取清单原始字节。实现内部负责大小上限。
    async fn fetch_manifest(&self) -> Result<Vec<u8>, EngineError>;

    /// 获取 detached 签名原始字节。
    async fn fetch_signature(&self) -> Result<Vec<u8>, EngineError>;

    /// 下载对象到本地临时文件，支持断点续传。
    /// `resume_from` 为当前临时文件已有字节数；`max_bytes` 是硬上限。
    async fn download(
        &self,
        key: &str,
        dest: &Path,
        resume_from: u64,
        max_bytes: u64,
    ) -> Result<DownloadReceipt, EngineError>;
}

pub struct DownloadReceipt {
    pub bytes_written: u64,
    pub resumed: bool,
}

pub struct FetchLimits {
    pub manifest_max_bytes: u64,
    pub signature_max_bytes: u64,
    pub object_max_bytes: u64,
}
```

`Fetcher` 是 trait，引擎不绑定具体 HTTP 客户端。生产实现使用 `reqwest`；测试实现使用内存 mock。**预签名 URL 的刷新与主机白名单在 `Fetcher` 实现内部处理**——当下载收到 403 过期响应时，实现者按 `signing_endpoint` 契约刷新 URL，并从 `resume_from` 续传，引擎不感知这一细节。

### 2.5 FileSystem 与 PrivilegedWriter trait

```rust
pub trait FileSystem: Send + Sync {
    fn create_dir_all(&self, path: &Path) -> std::io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()>;
    fn write_atomic(&self, path: &Path, data: &[u8]) -> std::io::Result<()>;
    fn remove(&self, path: &Path) -> std::io::Result<()>;
    fn read(&self, path: &Path) -> std::io::Result<Vec<u8>>;
    fn sync_dir(&self, path: &Path) -> std::io::Result<()>;
}

pub trait PrivilegedWriter {
    /// 从磁盘读取 Journal 并执行其中的操作。
    /// **不接受内存中的操作列表**：worker 必须独立读取并校验日志，
    /// 这样即使 supervisor 的进程被注入，也无法改变已固化的 intent。
    fn apply_journal(&self, root: &Path, journal: &Path) -> Result<(), EngineError>;
}
```

`FileSystem` 是故障注入的抽象点。生产代码使用 `RealFs`，测试使用 `FaultyFs` 按**目标路径**一次性注入失败（可指定 `io::ErrorKind`）或让重命名成功后损坏目标。**故障注入不修改生产代码路径**。

`DirectWriter::apply_journal_with_fs(config, journal_path, fs)` 接受注入的 `FileSystem`，供测试复用真实提交逻辑；生产路径的 `DirectWriter::apply_journal` 使用 `RealFs`。

`PrivilegedWriter` 是 worker 执行 Journal 的抽象，当前实现是 `DirectWriter`（直接文件操作，含 `apply_journal_with_fs` 测试入口）。替换进程的启动由插件的 `applier::spawn_replacement` 负责：先探测安装根是否可写，可写则普通 spawn；不可写（典型是 `Program Files`）则通过 `ShellExecuteW("runas")` 启动同一已签名二进制，UAC 提示出现在应用退出后。提权 worker 完成后通过 Explorer 降权拉起新版本。`FileOp` 是 Journal 操作类型，见 §2.7。

### 2.6 Journal API

```rust
pub struct CommitJournal { /* private */ }

impl CommitJournal {
    /// 写前日志：生成操作列表并 flush。
    /// flush 成功即提交点。`commit_id` 由调用方生成并写入日志。
    pub fn begin(
        root: &Path,
        version: &str,
        commit_id: Uuid,
        ops: Vec<FileOp>,
        fs: &dyn FileSystem,
    ) -> Result<Self, EngineError>;

    /// 标记完成。成功后 Journal 被删除。
    pub fn mark_completed(self, fs: &dyn FileSystem) -> Result<(), EngineError>;

    /// 恢复：检测并重放未完成的 Journal。
    /// 幂等：同一份 Journal 可被安全重放多次。
    pub fn recover(
        root: &Path,
        fs: &dyn FileSystem,
    ) -> Result<JournalOutcome, EngineError>;
}

#[derive(Debug)]
#[non_exhaustive]
pub enum JournalOutcome {
    NoJournal,
    Replayed { ops_completed: usize },
    Corrupt { reason: String },          // 不尝试重放
    ReplayFailed { op_index: usize, reason: String },
    AlreadyCompleted,
}
```

`recover()` 返回 `Corrupt` 或 `ReplayFailed` 时**不尝试“尽力恢复”**——损坏或与目录状态不一致的 Journal 重放可能导致比不恢复更糟的状态。调用方应记录诊断信息并转交回滚流程。

**Journal 与 `StartupOutcome` 的分工**：`JournalOutcome` 只描述日志层面的结果；`recover_and_cleanup()` 把它与崩溃循环、清理、缓存复用合并为 `StartupOutcome`，供插件层消费。

### 2.7 FileOp 枚举

```rust
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum FileOp {
    Backup { src: String, dst: String },
    Place { staging_src: String, dst: String, sha256: String, size: u64 },
    Trash { src: String, trash_dst: String },
    Restore { backup_src: String, dst: String, sha256: String, size: u64 },
    RemoveTrash { trash_path: String },
}
```

所有路径字段是**相对路径**。`WorkerWriter` 在 worker 侧对每个路径重新执行逃逸检查，不信任 Journal 中的路径未经检查。操作类型是编译期封闭枚举，**没有 `WriteArbitrary` 或 `Execute`**。

**`Place` 与 `Restore` 必须携带目标哈希**：执行前校验来源、执行后校验目标。没有它，重放只能判断“目标是否存在”，本地攻击者篡改暂存区并重算 checksum 后即可写入任意字节。

**`RemoveTrash` 只在 `confirm_healthy` 之后生成**：健康确认前的物理删除会让回滚失去素材。

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

### 3.2 Tauri 命令

插件暴露六个命令。**没有任何命令接收文件路径，也没有“替换任意文件”的命令**——Commit 由替换进程在应用退出后执行，`restart_to_apply` 只能消费已通过全部校验的 `Prepared` 状态。

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
}
```

拉取清单、验签、比对。**不下载补丁**。`available` 为 `false` 时其余字段仍填充（`version` 为 `None`），便于 UI 展示“已是最新”。与 `prepare` 互斥：`prepare` 进行中调用返回 `E_INVALID_TRANSITION`。检查失败时若已有 `Prepared` 或未确认的 `Committed`，保留原状态，不覆盖为 `Failed`。

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

标记当前运行版本健康。重置启动尝试计数。触发旧备份清理。**只有它被调用，`.old` / `.trash` 才允许物理删除**。`auto_confirm: true` 时插件层会在主窗口页面加载后延迟调用一次作为兜底；显式调用总是覆盖自动调用。成功确认后状态回到 `Idle`。

**`restart_to_apply`**

```typescript
invoke<{ state: 'Committing'; version: string }>('plugin:delta-update|restart_to_apply')
```

仅当状态为 `Prepared` 时有效。插件 spawn 替换进程并退出应用；替换进程在应用退出后执行 Journal，然后拉起新版本。无 `Prepared` 更新时返回 `E_NOT_PREPARED`，**不退出应用**。

### 3.3 事件

事件名：`delta-update://state`，通过 Tauri 标准事件系统广播。

```typescript
import { listen } from '@tauri-apps/api/event';

interface StatePayload {
  v: number;             // 事件协议版本，当前 1
  state: string;         // 状态枚举名
  version: string | null; // 目标/相关版本；下载、准备、提交与 Failed 时为本次目标版本
  progress: { total: number; completed: number; file?: string | null } | null;
  error: ErrorPayload | null;
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
  action: ErrorAction;   // "retry" | "wait" | "grant_permission" | ...
  user_action: string;   // 面向用户的建议（中文）
  detail: string;        // 保留字段，当前为空；诊断信息写日志
}
```

前端只读事件，**不参与决策**。事件中的 `error` 仅在 `state === 'Failed'` 时非空；`progress` 仅在 `Downloading` / `Preparing` / `Committing` 时非空；这些状态的 `version` 是本次目标版本，UI 可直接用于进度文案。

### 3.4 Rust 侧扩展 trait

```rust
pub trait DeltaUpdaterExt<R: Runtime> {
    fn delta_updater(&self) -> DeltaUpdater<R>;
}

impl<R: Runtime, T: Manager<R>> DeltaUpdaterExt<R> for T { /* ... */ }

#[derive(Clone)]
pub struct DeltaUpdater<R: Runtime> { /* private */ }

impl<R: Runtime> DeltaUpdater<R> {
    pub fn status(&self) -> EngineState;
    pub async fn check(&self) -> Result<CheckResult, EngineError>;
    pub async fn prepare(&self) -> Result<PrepareResult, EngineError>;
    pub fn cancel_prepare(&self) -> Result<(), EngineError>;
    pub fn confirm_healthy(&self) -> Result<(), EngineError>;
    pub async fn restart_to_apply(&self) -> Result<(), EngineError>;
}
```

为 Rust 侧其他插件或应用代码提供**可克隆句柄**（不是引用），方便在异步任务与事件回调中持有。`status()` 是同步的——它只读取内存中的状态快照，不执行 I/O。`check()` 和 `prepare()` 是异步的。

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
]
```

使用 `build.rs` 的 `tauri_plugin::Builder` 自动生成 `allow-<command>` 权限标识。**scope 不需要复杂定义**——命令不接收文件路径参数，路径由引擎内部从当前 exe 位置解析，前端无法传入任意路径。

## 4. 错误类型

### 4.1 引擎错误枚举

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EngineError {
    #[error("network error: {0}")]
    Network(#[from] NetworkError),

    #[error("signature verification failed: {0}")]
    Signature(#[from] SignatureError),

    #[error("manifest schema too new: client supports {client}, manifest requires {required}")]
    ManifestSchemaTooNew { client: u32, required: u32 },

    #[error("version rollback rejected: remote {remote}, local {local}")]
    VersionRollback { remote: String, local: String },

    #[error("invalid version: {0}")]
    VersionInvalid(String),

    #[error("path escape detected: {0}")]
    PathEscape(String),

    #[error("invalid path: {0}")]
    PathInvalid(String),

    #[error("path collision: {0}")]
    PathCollision(String),

    #[error("manifest too large: {0}")]
    ManifestTooLarge(String),

    #[error("insufficient disk space: required {required}, available {available}")]
    InsufficientDiskSpace { required: u64, available: u64 },

    #[error("staging area exceeds the configured limit of {limit} bytes")]
    StagingLimit { limit: u64 },

    #[error("permission denied: {0}")]
    PermissionDenied(String),

    #[error("user cancelled the elevation prompt")]
    UacCancelled,

    #[error("patch failed: {0}")]
    PatchFailed(#[from] PatchError),

    #[error("journal corrupt: {0}")]
    JournalCorrupt(String),

    #[error("update locked by another process")]
    UpdateLocked,

    #[error("rollback failed: {0}")]
    RollbackFailed(String),

    #[error("invalid config: {0}")]
    ConfigInvalid(String),

    #[error("config migration required: from {from} to {to}")]
    ConfigMigrationRequired { from: u32, to: u32 },

    #[error("insecure endpoint: {0}")]
    EndpointInsecure(String),

    #[error("unsupported platform: {0}")]
    UnsupportedPlatform(String),

    #[error("unsupported install format: {0}")]
    UnsupportedFormat(String),

    #[error("apply helper failed: {0}")]
    ApplyHelperFailed(String),

    #[error("not prepared")]
    NotPrepared,

    #[error("invalid state transition: {from} -> {to}")]
    InvalidTransition { from: String, to: String },

    #[error("cancelled")]
    Cancelled,

    #[error("internal error: {0}")]
    Internal(String),
}
```

遵循 Rust 生态惯例：**库 crate 使用 `thiserror` 定义具体枚举，调用方可匹配变体**。标记 `#[non_exhaustive]` 使新增变体不破坏 SemVer。**不在此枚举中使用 `anyhow`**——`anyhow` 仅用于 CLI 的顶层错误聚合。

三个子枚举持有细分码值，`EngineError` 只负责分组与转发：

```rust
pub enum NetworkError {
    Connect(String), Timeout(String),
    Http5xx { status: u16, detail: String },
    Http4xx { status: u16, detail: String },
    PresignExpired, RangeUnsupported, SigningService(String),
}

pub enum SignatureError { Invalid(String), NoTrustedKey, Missing }

pub enum PatchError {
    SourceMismatch(String), ApplyFailed(String), TargetHashMismatch(String),
    MemoryBudget { file_size: u64, budget: u64 }, FormatMismatch(String),
    DecompressionLimit(String), TooLarge(String), FullTooLarge(String),
    UnknownStrategy(String),
}
```

### 4.2 错误码映射

引擎错误变体与协议文档 §7.3 的字符串码值有一一映射。**`EngineError::code()` 是码值的唯一来源**；`NetworkError`、`SignatureError`、`PatchError` 持有细分码值，`EngineError` 只做转发。插件层把结果复制进 `ErrorPayload`，不维护第二份映射表。

| `EngineError` 变体                            | 协议码值示例                   |
| --------------------------------------------- | ------------------------------ |
| `Network(NetworkError::Timeout)`              | `E_NETWORK_TIMEOUT`            |
| `Signature(SignatureError::NoTrustedKey)`     | `E_SIGNATURE_NO_TRUSTED_KEY`   |
| `VersionRollback { .. }`                      | `E_VERSION_ROLLBACK`           |
| `VersionInvalid(_)`                           | `E_VERSION_INVALID`            |
| `PathEscape(_)`                               | `E_PATH_ESCAPE`                |
| `InsufficientDiskSpace { .. }`                | `E_DISK_INSUFFICIENT`          |
| `StagingLimit { .. }`                         | `E_DISK_STAGING_LIMIT`         |
| `PermissionDenied(_)`                         | `E_PERMISSION_DENIED`          |
| `UacCancelled`                                | `E_PERMISSION_UAC_CANCELLED`   |
| `PatchFailed(PatchError::TargetHashMismatch)` | `E_PATCH_TARGET_HASH_MISMATCH` |
| `JournalCorrupt(_)`                           | `E_JOURNAL_CORRUPT`            |
| `UpdateLocked`                                | `E_UPDATE_LOCKED`              |
| `RollbackFailed(_)`                           | `E_ROLLBACK_FAILED`            |
| `PathInvalid(_)`                              | `E_PATH_INVALID`               |
| `PathCollision(_)`                            | `E_PATH_COLLISION`             |
| `ManifestTooLarge(_)`                         | `E_MANIFEST_TOO_LARGE`         |
| `ConfigInvalid(_)`                            | `E_CONFIG_INVALID`             |
| `ConfigMigrationRequired { .. }`              | `E_CONFIG_MIGRATION_REQUIRED`  |
| `EndpointInsecure(_)`                         | `E_ENDPOINT_INSECURE`          |
| `UnsupportedPlatform(_)`                      | `E_UNSUPPORTED_PLATFORM`       |
| `UnsupportedFormat(_)`                        | `E_UNSUPPORTED_FORMAT`         |
| `ApplyHelperFailed(_)`                        | `E_APPLY_HELPER_FAILED`        |
| `NotPrepared`                                 | `E_NOT_PREPARED`               |
| `InvalidTransition { .. }`                    | `E_INVALID_TRANSITION`         |
| `Cancelled`                                   | `E_CANCELLED`                  |
| `Internal(_)`                                 | `E_INTERNAL`                   |

映射表是**唯一**的码值来源，由 `EngineError::code()` 实现；插件层只负责把 `ErrorPayload` 原样序列化给前端。

## 5. delta-update-cli 公开接口

CLI 的命令行参数是公开接口，输出格式不承诺稳定。

### 5.1 子命令

```bash
delta-update release    # 生成清单、补丁、签名、发布
delta-update verify     # 从对象存储验证已发布内容
delta-update verify-key # 验证私钥与公钥配对
delta-update doctor     # 线上体检：清单、签名、补丁链、资源可访问性
delta-update inspect    # 离线解码本地 journal 目录，输出更新诊断
```

### 5.2 `release` 参数

```
--to <DIR>              必填
--version <SEMVER>      必填，新版本号
--bucket <URI>          必填；cos://bucket、s3://bucket 或本地目录
--key <PATH>            必填（私钥文件路径）
--from <DIR>            增量发布的基准目录；省略时等价于 --initial
--from-version <SEMVER> 基准版本号；提供 --from 时必填
--initial               首次发布，所有文件走全量
--rollback              生成 rollback: true 的降级清单（必须提供 --from）
--channel <NAME>        默认 stable
--region <REGION>       COS/S3 区域，默认从 COS_REGION / S3_REGION 读取
--prefix <PREFIX>       对象键前缀，默认空
--dry-run               不上传
--require-delta         本应差分的文件未能产出低于阈值的补丁时失败
--delta-max-ratio <F>   默认 0.60
--patch-max-file-size <BYTES> 超过此大小不做差分，默认 64 MiB
--verify                发布后自动执行 verify
--jobs <N>              默认 CPU 核数
--verbose
```

完整语义见构建发布操作手册 §3。`--key` 接收**私钥文件路径**，而非私钥内容——私钥内容不应出现在命令行参数中。

**`--require-delta` 的精确定义**：对每个 `Modified` 且未被标记为“不可压缩格式”的文件，如果最终策略是 `full`，或补丁体积 ≥ `--delta-max-ratio`，发布失败。显式标记为不可压缩（png/jpg/zip 等）的文件不参与该判定。

### 5.3 `verify`、`doctor` 与 `inspect` 参数

```
verify:
--bucket <URI>          必填；cos://bucket、s3://bucket 或本地目录
--version <VERSION>     必填
--public-key <PATH>     必填；受信公钥文件（可含多把）
--sample <N>            抽样文件数，默认 3
--region <REGION>       默认从 COS_REGION / S3_REGION 读取
--prefix <PREFIX>       与 release 使用的对象键前缀一致

doctor:
--endpoint <URL>        必填，可重复传入多个备用端点
--public-key <PATH>     可选；提供时验签，缺省时只做结构检查
--json                  机器可读输出

inspect:
--journal-dir <DIR>     必填；应用本地数据目录下的 delta-update/journal
--json                  输出机器可读 JSON
```

`verify` 从对象存储重新下载清单、验签、抽样下载补丁、应用补丁、校验目标哈希。`doctor` 额外校验：所有备用端点的版本一致性、清单字段合法性、对象可访问性与字节数。`inspect` 离线读取 journal 目录，输出待处理提交、PREPARED、回滚与损坏状态，不需要启动应用。三者都是发布与排障的标准步骤。

## 6. Feature Flags

```toml
[features]
default = []
test-faults = []             # 仅供测试构建启用故障注入，生产路径不含任何分支
```

**TLS 固定使用 rustls**：跨平台一致、无需系统 OpenSSL，不提供 `native-tls` 切换，避免两套证书栈与构建差异。需要接入企业证书库时，通过 `reqwest` 的 root certificate 配置注入，而不是替换 TLS 后端。

## 7. 平台支持矩阵

| 平台 / 安装形态         | 引擎 | 插件 | CLI | 文件级替换 | 原因                                                       |
| ----------------------- | ---- | ---- | --- | ---------- | ---------------------------------------------------------- |
| Windows 10+ x64         | ✅   | ✅   | ✅  | ✅         | 支持 per-user 与 per-machine；后者走 UAC + 受限 worker      |
| Linux AppImage / tar    | ✅   | ✅   | ✅  | ✅         | 用户可写目录，AppImage 是单文件，替换语义成立               |
| Linux deb / rpm         | ✅   | ✅   | ✅  | ❌         | 安装目录 root-owned，需包管理器；返回 `E_UNSUPPORTED_FORMAT` |
| Linux Flatpak / Snap    | ✅   | ✅   | ✅  | ❌         | 只读沙箱，禁止应用自更新                                    |
| macOS（签名/公证 .app） | ✅   | ✅   | ✅  | ❌         | 修改 bundle 内部文件会使代码签名失效，必须走 PKG/全量更新   |
| macOS（未签名开发包）   | ✅   | ✅   | ✅  | ✅         | 仅开发与内测；不承诺分发                                    |
| iOS / Android           | ✅   | ❌   | ✅  | ❌         | 移动端不允许自更新；注册期返回 `E_UNSUPPORTED_PLATFORM`     |

**平台判断发生在注册期或下载前**：不支持的形态直接返回 `E_UNSUPPORTED_PLATFORM` / `E_UNSUPPORTED_FORMAT`，**绝不先下载几十 MB 再告诉用户不能更新**。引擎本身是平台无关的，可以在任何 Rust 支持的平台上独立使用；限制只存在于插件的替换适配层。

## 8. API 参考总结

这份 API 参考的核心判断是：**公开接口的“窄”比“全”更重要**。引擎暴露 `Engine` 一个主句柄和几个 trait，插件只暴露六个命令和一个事件，CLI 只暴露四个子命令。每一个公开名称都是一份维护承诺——新增容易，退役难。`#[non_exhaustive]` 和 `thiserror` 枚举的组合，使新增变体和字段不破坏 SemVer，同时保持调用方可匹配的精确性。`Fetcher` 和 `FileSystem` 的 trait 抽象使引擎可以脱离 Tauri 和网络独立测试，这是故障注入验证能够成立的前提。

二次开发者最可能用到的入口是 `Engine::new` + `Engine::check` + `Engine::diff` + `Engine::prepare`；`Engine::commit` 只应由替换进程调用。如果只需要 Tauri 集成，`tauri_plugin_delta_update::init()` 和六个命令已经覆盖全部场景。CLI 的 `release`、`verify`、`doctor` 是发布流水线的三个标准步骤，参数应通过环境变量和 CI secret 注入，而非硬编码。