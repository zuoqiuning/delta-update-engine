# Delta Update Engine API 参考

> 本文件是 [API 参考](api.md) 的引擎部分：`delta-update-engine` 的公开类型、trait、错误与稳定性策略。实现与本文不一致时以本文为准；接口变更必须先更新本文，再改代码。

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

    /// 健康确认：重置启动尝试计数，清理 .old/.trash、Journal、PREPARED，
    /// 并丢弃本进程上一次 `check` 的快照（此时安装树已是新版本）。
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

    /// 跳过 `version`：`check` 在下个更高版本（或无 `rollback` 的同版本重发）
    /// 出现前返回不可用。只接受合法 semver。
    pub fn skip_version(&self, version: &str) -> Result<(), EngineError>;

    /// 当前被跳过（用户选择或崩溃回滚写入）的版本。
    pub fn skipped_version(&self) -> Option<String>;
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

**Rust 侧的确认健康**：没有前端（托盘/无 WebView）或希望在原生启动流程里确认时，直接调用 `tauri_plugin_delta_update::confirm_healthy(app.handle())`；它与命令同名同语义（重置启动计数、清理 Journal/PREPARED、状态回到 `Idle`），失败返回 `ErrorPayload`。`auto_confirm` 仍然是“窗口可见即确认”的粗略信号，二者择一即可。

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
    /// 云占位文件（OneDrive/Files On-Demand）跳过数量。
    pub cloud_placeholders_skipped: u32,
    /// 用户跳过该版本时给出被跳过的版本号；`available` 仍为 `false`。
    pub skipped_version: Option<String>,
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
    /// 累计下载字节。`completed` 在单个文件内也会持续增长（约 100ms 一次，
    /// 由 `Fetcher::download_with_progress` 驱动），结束时严格等于
    /// `CheckOutcome::download_bytes`。
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

    /// 与 `download` 相同，但通过 `on_progress` 报告 `dest` 已写入的字节数。
    /// 默认实现忽略进度并转调 `download`，未实现进度的 fetcher 无需改动。
    async fn download_with_progress(
        &self,
        key: &str,
        dest: &Path,
        resume_from: u64,
        max_bytes: u64,
        on_progress: &(dyn Fn(u64) + Send + Sync),
    ) -> Result<DownloadReceipt, EngineError> {
        self.download(key, dest, resume_from, max_bytes).await
    }
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
    /// 单个 fetcher 内并发 HTTP 请求上限（文件级与 Range 级合计），默认 8。
    pub request_concurrency: usize,
}
```

`Fetcher` 是 trait，引擎不绑定具体 HTTP 客户端。生产实现使用 `reqwest`；测试实现使用内存 mock。**预签名 URL 的刷新与主机白名单在 `Fetcher` 实现内部处理**——当下载收到 403 过期响应时，实现者按 `signing_endpoint` 契约刷新 URL，并从 `resume_from` 续传，引擎不感知这一细节。

`HttpFetcher` 对清单使用 `ETag`/`If-None-Match` 重新验证：清单未变化时服务端返回 `304`，`check` 不再重复传输整个文档（签名对象仍每次获取，通常只有几百字节）。`request_concurrency` 用信号量限制同一个 fetcher 内的总连接数，默认 8；`prepare` 的文件级并发与单文件 Range 并发之和不会超过该值。

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


