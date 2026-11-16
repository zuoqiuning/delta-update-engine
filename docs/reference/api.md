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
    /// 连续未确认启动多少次后回滚，默认 3，必须 ≥ 1。
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
    /// 扫描本地根目录并计算差异；会读写 journal 目录下的哈希缓存。
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

`check` 的安装树扫描、`prepare` 的未变化文件校验、暂存文件哈希与全量哈希校验都在阻塞线程池执行，不会占住异步运行时。暂存与 PREPARED 持久化期间持有跨进程 `update.lock`，第二个实例的 prepare 立即返回 `E_UPDATE_LOCKED`。未变化文件在 `prepare` 时重新校验；如果本地内容已变化或缺失，会改用 `full_key` 全量修复，而不是直接报 `E_PATCH_SOURCE_MISMATCH`。

`Progress::Downloading { total, completed }` 是**累计字节数**：`total` 是本次计划下载总量，`completed` 随完成的文件单调递增；补丁回退到全量时可能超过计划值，引擎会截断到 `total`，保证进度单调且不超过 100%。`Progress::Verifying` 按已暂存文件计数，插件会把它映射为 `Preparing`，并以 100ms 为窗口合并事件，避免大文件集产生 IPC 风暴。状态事件里的 `progress.unit` 明确标注单位：`Downloading` 为 `bytes`、`Preparing` 为 `files`、`Committing` 为 `ops`，前端不需要按状态猜。

**`begin_commit` 的调用者**：插件只在退出/重启路径调用它，写入 Journal 后启动替换进程并退出。替换进程通过 `run_applier_from_context`（或引擎层的 `run_applier_with_keys`）读取 `apply.json` 与 Journal 执行，不加载 WebView 或业务线程；`restart_to_apply` 路径额外传入 `--relaunch`，替换完成后拉起新版本。worker 用内嵌配置中的 `signing_public_keys` 重新验签 `verified_manifest.json`，并校验 Journal 中的每个写入哈希都来自该签名清单；`commit.meta.json` 把 Journal 与清单哈希、版本、安装根和 `commit_id` 绑定。若替换进程未能启动（例如 UAC 被取消），插件调用 `discard_pending_commit` 撤销提交点并保留 PREPARED，下次启动直接复用。无密钥的 `apply_commit` 是不验签的低层 legacy API，只用于测试/迁移；`run_applier` 与 `run_applier_from_context` 在无有效公钥时失败关闭，迁移请使用 `run_applier_with_keys`。

**启动期提权恢复**：per-machine 安装的启动恢复可能需要写安装根。插件在调用 `recover_and_cleanup` 前先检查 `recovery_needs_install_write`；需要写且安装根不可写时，用同一二进制的 `--delta-update-recover <journal_dir> <crash_loop_threshold>` 模式提权执行恢复并等待其退出，再调用 `recover_after_elevation` 读取结果。worker 侧通过 `run_recovery_with_keys`（插件入口 `run_applier_from_context` 自动传入内嵌公钥）校验 `apply.json` 的绝对路径、`install_root` 精确等于自身目录、staging/journal 不在安装根内，并在重放 Journal 前重新验签清单。

**`EngineConfig` 是公开类型**：二次开发者可以脱离 Tauri 直接构造引擎。`Limits` 包含清单大小、文件数、补丁/全量上限、内存预算等，全部有安全默认值。`crash_loop_threshold` 控制连续未确认启动多少次后回滚，默认 `3`。

**无签名恢复不是公开选项**：`Engine::new` 始终要求 Journal 绑定到已验签清单。测试/迁移需要的无签名路径只在 `Engine::new_legacy(config, true)`（`#[doc(hidden)]`）中可用，插件与 CLI 从不调用它。

**正常退出减一**：插件在 `RunEvent::Exit` 时，如果更新已提交但未确认、且本次会话已运行超过 `exit_grace_period_secs`（默认 10 秒），调用 `Engine::discount_clean_exit()` 把计数减一。崩溃不会走到这个钩子，因此“打开看一眼就关掉”不再累计成回滚，而真正的崩溃循环仍然计数。

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

**`executable` 当前是保留字段**：Windows 文件替换不依赖 Unix 可执行位，引擎不读取它；保留是为了清单审计与未来非 Windows 场景，不应作为行为依据。

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
    /// 本地版本低于同通道清单的 `min_supported_version`。
    pub update_required: bool,
    /// 发布者仍支持的最旧版本；清单未设置时为 `None`。
    pub min_supported_version: Option<String>,
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
    /// 下载前的未变化文件校验；completed/total 是该阶段的文件数。
    Scanning { file: String, completed: u64, total: u64 },
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
    /// Objects at least this large use parallel Range chunks.
    pub range_chunk_threshold: u64,
    pub range_chunk_size: u64,
    pub range_concurrency: usize,
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
    /// 生产 worker 先用 `apply_commit_verified` 验签，再把同一份解码结果
    /// 交给 `DirectWriter::apply_decoded_journal_with_fs`，避免验证/执行之间
    /// 被替换；这个 trait 保留给需要按路径执行的自定义 writer。
    fn apply_journal(
        &self,
        config: &ApplyConfig,
        journal: &Path,
    ) -> Result<CommitStatus, EngineError>;
}
```

`FileSystem` 是故障注入的抽象点。生产代码使用 `RealFs`，测试使用 `FaultyFs` 按**目标路径**一次性注入失败（可指定 `io::ErrorKind`）或让重命名成功后损坏目标。**故障注入不修改生产代码路径**。

`DirectWriter::apply_journal_with_fs(config, journal_path, fs)` 从磁盘读取并执行；`DirectWriter::apply_decoded_journal_with_fs(config, journal, fs)` 直接执行已解码的 Journal，是 `apply_commit_verified` 使用的生产路径。两者都接受注入的 `FileSystem`，测试可以复用真实提交逻辑。

`PrivilegedWriter` 是 worker 执行 Journal 的抽象，当前实现是 `DirectWriter`。替换进程的启动由插件的 `applier::spawn_replacement` 负责：先探测安装根是否可写，可写则普通 spawn；不可写（典型是 `Program Files`）则通过 `ShellExecuteW("runas")` 启动同一已签名二进制，UAC 提示出现在应用退出后。提权 worker 完成后通过 Explorer 降权拉起新版本。`FileOp` 是 Journal 操作类型，见 §2.7。

### 2.6 Journal API

```rust
pub struct Journal {
    pub format: u16,
    pub commit_id: String,
    pub version: String,
    pub from_version: String,
    pub ops: Vec<FileOp>,
}

impl Journal {
    pub fn new(commit_id: impl Into<String>, version: impl Into<String>,
               from_version: impl Into<String>, ops: Vec<FileOp>) -> Self;
    pub fn encode(&self) -> Result<Vec<u8>, EngineError>;
    pub fn decode(bytes: &[u8]) -> Result<Self, EngineError>;
    pub fn write_atomic(&self, path: &Path) -> Result<(), EngineError>;
}

pub fn replay(context: &ReplayContext<'_>, journal: &Journal)
    -> Result<JournalOutcome, EngineError>;

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

`replay()` 返回 `Corrupt` 或 `ReplayFailed` 时**不尝试“尽力恢复”**——损坏或与目录状态不一致的 Journal 重放可能导致比不恢复更糟的状态。调用方应记录诊断信息并转交回滚流程。

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

### 2.8 worker.log 读取

```rust
pub mod worker_log {
    pub const FILE: &str = "worker.log";

    pub struct Event { pub name: String, /* 字段私有 */ }

    impl Event {
        pub fn get(&self, key: &str) -> Option<&str>;
    }

    /// 解析 `event=<name> key=value ...`；旧版纯文本行与手写日志返回 None。
    pub fn parse(line: &str) -> Option<Event>;
}
```

`worker.log` 由 worker 与 `prepare` 追加，每行一条事件，形如 `event=<name> ts=<unix> key=value ...`；值含空格或引号时用双引号包裹并转义。公开的只有读取侧：`filter_map(worker_log::parse)` 可以安全跳过任何非结构化行。写入侧不公开，避免外部代码绕过“单行、有界体积”的约束。

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

插件暴露九个命令。**没有任何命令接收文件路径，也没有“替换任意文件”的命令**——Commit 由替换进程在应用退出后执行，`restart_to_apply` 只能消费已通过全部校验的 `Prepared` 状态。

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
}
```

拉取清单、验签、比对。**不下载补丁**。`available` 为 `false` 时其余字段仍填充（`version` 为 `None`），便于 UI 展示“已是最新”。`cloud_placeholders_skipped` 是本次扫描中被跳过的云占位文件数（OneDrive/Files On-Demand）：它们不参与 diff，若清单仍列出则按全量下载，UI 可用它解释“为什么下载量比预期大”。`update_required` 为 `true` 时表示本地版本低于同一通道清单的 `min_supported_version`：插件只上报事实，**不阻止应用运行**；应用可以选择显示模态框或禁用功能，直到更新完成。与 `prepare` 互斥：`prepare` 进行中调用返回 `E_INVALID_TRANSITION`。检查失败时若已有 `Prepared` 或未确认的 `Committed`，保留原状态，不覆盖为 `Failed`。

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
  detail: string;        // 保留字段，当前为空；文件/哈希等诊断在 message 与日志中
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

除 `default` 外还有两个最小权限集，可用于收窄前端能力面：

| 权限标识 | 包含命令 | 适用场景 |
| -------- | -------- | -------- |
| `delta-update:monitor` | `check`、`status`、`get_update_history` | 只展示更新状态与历史，不触发下载或重启 |
| `delta-update:manage`  | `check`、`prepare`、`cancel_prepare`、`status`、`restart_to_apply`、`export_diagnostics`、`open_diagnostics_folder`、`clear_skipped_version` | 能触发更新但**不能**代替应用确认健康 |
| `delta-update:default` | 全部命令（含 `confirm_healthy`） | 兼容旧接入；生产环境建议改用 `manage` |

`delta-update init --permission-set monitor|manage|default` 可直接生成对应的 capability；**默认是 `manage`**（不含 `confirm_healthy`），需要前端确认健康时显式使用 `default`。`confirm_healthy` 只应由应用自身在关键初始化完成后调用，因此不进入 `manage`。

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
| `JournalFormatTooNew { .. }`                  | `E_JOURNAL_FORMAT_TOO_NEW`     |
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
| `ElevatedRecoveryTimeout { .. }`              | `E_APPLY_RECOVERY_TIMEOUT`     |
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
delta-update key-info   # 从私钥导出 base64 公钥与 key ID
delta-update doctor     # 线上体检：清单、签名、补丁链、资源可访问性
delta-update inspect    # 离线解码本地 journal 目录，或审计已发布版本 / 对照诊断包
delta-update init       # 打印插件配置与 capability 片段；`--write` 生成 capabilities/delta-update.json
delta-update migrate    # 从官方 tauri-plugin-updater 打印迁移计划；--write 生成 capability 与迁移指南
delta-update report     # 汇总 release --pipeline 的历史报告，查看增量覆盖率趋势
```

### 5.2 `release` 参数

```
--to <DIR>              必填
--version <SEMVER>      必填，新版本号
--bucket <URI>          必填；cos://bucket、s3://bucket 或本地目录
--key <PATH>            必填（私钥文件路径）
--additional-signing-key <PATH> 附加私钥，可重复；为每把签名并写入 signing_key_ids
--from <DIR>            增量发布的基准目录；省略时等价于 --initial
--from-version <SEMVER> 基准版本号；提供 --from 时必填
--initial               首次发布，所有文件走全量
--rollback              生成 rollback: true 的降级清单（必须提供 --from）
--channel <NAME>        默认 stable
--notes-file <PATH>     发布说明，写入清单 notes（UTF-8，≤ 64 KiB）
--mandatory             标记为强制更新
--min-supported <SEMVER> 最低支持版本；更旧的客户端 check 返回 update_required
--region <REGION>       COS/S3 区域，默认从 COS_REGION / S3_REGION 读取
--prefix <PREFIX>       对象键前缀，默认空
--dry-run               不上传
--require-delta         本应差分的文件未能产出低于阈值的补丁时失败
--delta-max-ratio <F>   默认 0.60
--patch-max-file-size <BYTES> 超过此大小不做差分，默认 64 MiB
--verify                发布后自动执行 verify
--self-test             在 --from 的临时副本上执行 apply + rollback
--pipeline              生成 → 自测 → 上传 → 审计，一步完成并写发布报告
--report <PATH>         --pipeline 的报告路径，默认 release-report.json
--watch <SECONDS>       --pipeline 发布后继续审计该时长（任何一轮缺对象即失败）
--interval <SECONDS>    --watch 的审计间隔，默认 60
--jobs <N>              扫描与补丁生成的并行度，默认 min(max(核数/2, 1), 4)
--verbose
```

`--pipeline` 在首个失败步骤停止；成功后才写报告。首次发布（`--initial`）会跳过自测并在报告中记录原因。`--report` 指向目录（或没有 `.json` 后缀）时按 `<version>.json` 归档，供 `delta-update report --history` 汇总。`--watch N` 在审计后继续按 `--interval` 重跑审计，覆盖 CDN 传播与对象存储一致性延迟；报告中的 `watch` 字段记录轮次。报告字段与退出码见 [CLI 契约](../cli-contract.md)。

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
--endpoint <URL>        体检端点，可重复传入多个备用端点（--check-config 时省略）
--public-key <PATH>     可选；提供时验签，缺省时只做结构检查
--check-config          离线校验 <dir>/src-tauri/tauri.conf.json 的 plugins.delta-update
--dir <DIR>             项目根目录，默认 .（配合 --check-config）
--watch <SECONDS>       持续检查该时长；任一轮失败即失败（0 表示只跑一轮）
--interval <SECONDS>    --watch 的检查间隔，默认 60
--json                  机器可读输出（不能与 --watch 同时使用）
--simulate-failure <MODE> lock / patch / disk：用真实代码路径产出对应错误，不写状态
--reason <REASON>       仅配合 patch：source-mismatch / format / target-hash / memory

inspect:
--journal-dir <DIR>     本地 journal 目录（--from-release / --bundle 时省略）
--bundle <PATH>         对照诊断包：用户 journal × 远端 manifest × worker.log
--from-release <URI>    发布后审计：cos://bucket、s3://bucket 或本地目录
--version <VERSION>     审计的版本（配合 --from-release）
--public-key <PATH>     受信公钥文件（配合 --from-release）
--prefix <PREFIX>       与 release 使用的对象键前缀一致
--verbose               逐条列出 Journal 操作及落地/备份状态
--json                  输出机器可读 JSON
--export <PATH>         写出单文件诊断包
--redact                诊断包中的绝对路径替换为占位符

report:
--history <DIR>         release --pipeline 报告目录
--json                  机器可读输出

init:
--dir <DIR>             项目根目录，默认 .
--endpoint <URL>        清单地址；省略时打印占位符
--public-key <KEY>      受信公钥，可重复（轮换用）；至少一把
--signing-key <PATH>    从私钥派生公钥，可重复；免去手工复制 base64
--channel <NAME>        默认 stable
--write                 生成 src-tauri/capabilities/delta-update.json（不改 tauri.conf.json）
--permission-set <SET>  manage（默认）/ monitor / default

migrate:
--dir <DIR>             项目根目录，默认 .
--write                 生成 src-tauri/capabilities/delta-update.json 与 delta-update-migration.md

serve:
--dir <DIR>             `release --bucket <DIR>` 写出的目录
--port <PORT>           默认 8123；0 由系统分配
--host <HOST>           默认 127.0.0.1；非回环地址时客户端不接受明文 HTTP
```

`serve` 启动后直接打印可粘贴进 `plugins.delta-update.endpoint` 的 URL，并提示本地联调需要 `allow_insecure_http: true`；它是无 TLS 的诊断/演示服务器，不是生产 CDN。

`verify` 从对象存储重新下载清单、验签、抽样下载补丁、应用补丁、校验目标哈希。`doctor` 额外校验：所有备用端点的版本一致性、清单字段合法性、对象可访问性与字节数；`doctor --check-config` 不访问网络，直接使用引擎的配置 schema 校验 `plugins.delta-update`，因此 CI 无需安装应用。`doctor --watch <SECONDS>` 按 `--interval` 反复执行同一套检查，适合发布后 5–10 分钟的 CDN 传播观察窗口；任何一轮失败都会让整个命令失败。

`migrate --write` 只创建两个新文件（capability 与 `delta-update-migration.md`），**不修改** `tauri.conf.json`、`Cargo.toml` 或 `main.rs`；已存在的文件一律跳过，因此不需要备份或冲突合并。迁移指南包含五节：配置替换、capability、Cargo 依赖、`main.rs` diff，以及官方前端 API（`check()` / `downloadAndInstall` / `relaunch` / `onUpdaterEvent`）到 `check()` / `prepare()` / `restartToApply()` / `listen(STATE_EVENT)` 的对照表与一段可直接粘贴的 `createUpdateController()` 片段。

`inspect --journal-dir` 离线读取 journal 目录，输出待处理提交、PREPARED、回滚与损坏状态，并在末尾打印 `worker.log` 里最新一条结构化事件（`event=` 与其 `msg`）；退出码 `1` 表示未完成 Journal（可重启恢复），`2` 表示状态损坏（需人工介入）。

`inspect --from-release` 是发布后审计：下载并验签版本化清单，逐文件输出与 `release --verbose` 同构的表格（`FILE / STRATEGY / FULL / DELTA / RATIO`），对每个 `delta_key`/`full_key` 做 `HEAD` 大小校验（不下载对象），最后汇总增量覆盖率。它与 `release --verbose` 的差异就是“构建意图”与“远端实际”的差异，可直接 diff：

```bash
delta-update release --from build/1.9.0 --to build/2.0.0 --version 2.0.0 \
  --from-version 1.9.0 --bucket cos://app-releases --key release.key \
  --verbose > before.txt

delta-update inspect --from-release cos://app-releases --version 2.0.0 \
  --public-key release.pub --prefix releases > after.txt

diff before.txt after.txt
```

`inspect --bundle <bundle.json>` 把支持人员最常做的三处对照合并成一张表：`worker.log` 里的每文件补丁回退（`prepare` 写入 `event=patch_fallback path=<path> code=<code>`，旧格式 `patch fallback: <path>: <code>` 同样识别）、journal 的 `ops_detail`（需要 `--export --verbose`），以及 `--from-release` 提供的远端清单策略。四列分别是文件、manifest 策略/体积、journal 实际操作、worker.log 证据；结尾汇总 `N patched / M fell back / K full by design / R removed`，并给出来自 `worker.log` 的最新一条结构化事件（`--json` 中的 `worker_event`，含 `name`/`ts`/`msg`）。不带 `--from-release` 时只对照本地证据。

`delta-update report --history <DIR>` 读取 `release --pipeline` 的历史报告（`--report` 指向目录时按 `<version>.json` 归档），按版本排序输出覆盖率趋势表；当覆盖率相比上一版本下降超过 20 个百分点时，额外列出该版本新增的回退文件与原因。

**退出码契约**（所有子命令一致，由 `EngineError::exit_code` 定义）：

| 退出码 | 含义 | 典型场景 |
| --- | --- | --- |
| 0 | 成功 | — |
| 1 | 暂时性失败，重试可能成功 | 网络、文件占用、更新锁 |
| 2 | 需要人工处理 | 配置、签名、清单、磁盘、权限 |
| 3 | 内部错误 | 引擎不变量被破坏，需要报告 |

`inspect --journal-dir` 的 `1`/`2` 语义与上表对齐：`1` 是“重启即可恢复”，`2` 是“需要人工介入”。任何 `--json` 输出的顶层都包含 `"v": 1` 与 `"tool": "delta-update-cli"`，调用方应据此判断格式版本。

## 6. Feature Flags

```toml
[features]
default = []
```

故障注入通过 `delta-update-testkit` 提供，不使用 Cargo feature；生产路径不出现 `if cfg!(test)` 分支。

**TLS 固定使用 rustls**：跨平台一致、无需系统 OpenSSL，不提供 `native-tls` 切换，避免两套证书栈与构建差异。需要接入企业证书库时，通过 `reqwest` 的 root certificate 配置注入，而不是替换 TLS 后端。

## 7. 平台支持矩阵

| 平台 / 安装形态         | 引擎 | 插件 | CLI | 文件级替换 | 说明                                                       |
| ----------------------- | ---- | ---- | --- | ---------- | ---------------------------------------------------------- |
| Windows 10+ x64         | ✅   | ✅   | ✅  | ✅         | 唯一支持的交付形态；per-user / per-machine / 便携版          |
| Windows ARM64           | ✅   | ⚠️   | ✅  | ⚠️         | 未作为门禁验证；需自行在目标机器验收替换与 UAC              |
| macOS（签名/公证 .app） | ✅   | ❌   | ✅  | ❌         | 修改 bundle 会使代码签名失效；走官方 updater/PKG/Sparkle     |
| macOS（未签名开发包）   | ✅   | ❌   | ✅  | ❌         | 不作为交付形态；资源在 `Contents/Resources`，不在 exe 父目录 |
| Linux AppImage / tar    | ✅   | ❌   | ✅  | ❌         | AppImage 只读挂载；tar 不保留可执行位；走 AppImageUpdate/包管理器 |
| Linux deb / rpm         | ✅   | ❌   | ✅  | ❌         | 属于包管理器；应用不应直接改                               |
| Linux Flatpak / Snap    | ✅   | ❌   | ✅  | ❌         | 只读沙箱；禁止应用自更新                                   |
| iOS / Android           | ✅   | ❌   | ✅  | ❌         | 移动端不允许自更新；插件注册期返回 `E_UNSUPPORTED_PLATFORM` |

**平台判断发生在插件 setup 最早期**：非 Windows 平台直接进入 `Disabled`/`Failed` 状态并返回 `E_UNSUPPORTED_PLATFORM`，主应用仍可启动，**不会下载任何字节**。引擎本身是平台无关的 Rust crate，可以在其他平台独立使用；限制只存在于插件的文件替换适配层。

## 8. API 参考总结

这份 API 参考的核心判断是：**公开接口的“窄”比“全”更重要**。引擎暴露 `Engine` 一个主句柄和几个 trait，插件只暴露九个命令和一个事件，CLI 只暴露九个子命令。每一个公开名称都是一份维护承诺——新增容易，退役难。`#[non_exhaustive]` 和 `thiserror` 枚举的组合，使新增变体和字段不破坏 SemVer，同时保持调用方可匹配的精确性。`Fetcher` 和 `FileSystem` 的 trait 抽象使引擎可以脱离 Tauri 和网络独立测试，这是故障注入验证能够成立的前提。

二次开发者最可能用到的入口是 `Engine::new` + `Engine::check` + `Engine::diff` + `Engine::prepare`；`Engine::commit` 只应由替换进程调用。如果只需要 Tauri 集成，`tauri_plugin_delta_update::init()` 和九个命令已经覆盖全部场景。CLI 的 `release`、`verify`、`doctor` 是发布流水线的三个标准步骤，参数应通过环境变量和 CI secret 注入，而非硬编码。