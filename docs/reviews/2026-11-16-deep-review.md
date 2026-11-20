# Delta Update Engine 深度评审报告（1.0.0-rc.4）

- 评审对象：`delta-update-engine`、`tauri-plugin-delta-update`、`delta-update-cli`、`tauri-plugin-delta-update-api`
- 评审日期：2026-11-16
- 代码状态：workspace `1.0.0-rc.4`，公共 API 冻结，本地 4 个未推送提交
- 评审范围：Rust/TS 源码、Cargo/npm 配置、权限定义、CI/发布流水线、示例、e2e、xtask、全部文档
- 评审方法：逐文件阅读 + 契约/故障注入用例核对 + 本地执行 + 真实接入与升级路径推演
- 对照基线：[2026-11-12 深度评审](2026-11-12-deep-review.md)

> **跟进状态（2026-11-16 同日）**：批次 0 文档纠偏、默认权限/健康确认说明、示例固定延时确认、签名服务 64 KiB 上限与 `signing_headers`、journal 路径链接拒绝、`confirm_healthy` 计数加锁、`check` 扫描并行哈希已落地，详见 `CHANGELOG.md` 的 `[未发布]`。下文保留评审当时的证据与结论，作为基线记录。

---

## 0. 一页结论

### 0.1 总体判断

**这是一个已经完成安全信任链收敛、恢复状态机收口和 Windows-only 平台收敛的 rc 级增量更新内核。核心机制（签名先于落盘、journal 绑定签名清单、内容寻址 blob 修复、崩溃循环回滚、提权 worker 作用域绑定）已经闭环；当前主要风险从“实现有硬伤”转移到了“文档与默认配置会让新接入者走错路”。**

与 11-12 基线相比，7 个 P0 全部关闭：

| 基线 P0 | 当前实现证据 | 状态 |
| --- | --- | --- |
| journal/apply.json 未认证 | `journal/verify.rs` 验签清单并逐 op 绑定哈希；`apply/writer.rs` 验证与执行同一份内存 Journal | 已修复 |
| 回滚中断不清理 commit 状态 | `recovery.rs::finish_resumed_rollback` 原子收尾 | 已修复 |
| 未变化文件无全量回退 | 所有条目发布 `blobs/<sha256>`；`prepare` 把本地变化/缺失文件改走全量修复 | 已修复 |
| 平台声明与实现不符 | 插件 `compile_error!` + `ensure_supported`；文档统一 Windows 10+ | 已修复 |
| 配置错误阻止应用启动 | `setup_disabled` 降级，主窗口照常创建 | 已修复 |
| `auto_confirm` 默认 true + 示例 1s 确认 | 默认 `auto_apply=false`、`auto_confirm=false` | 已修复（示例仍有隐患，见 N-09） |
| 回滚信任备份自身哈希 | `old_hashes.json` 快照 + worker 用受信旧哈希校验 `Restore` | 已修复 |

当前不能称为“可以公开宣传为跨平台生产级产品”，但它作为 **Windows 10+ 桌面、自托管发布通道的 rc 级更新内核**，工程质量与安全模型已经达到可发布门槛。

### 0.2 评分

| 维度 | 得分/10 | 结论 |
| --- | ---: | --- |
| 架构设计与模块边界 | 8.5 | 三层单向依赖、引擎零 Tauri、补丁策略注册表；公开面仍偏大 |
| 增量算法 / 差分与合并 | 7.5 | 单策略但闭环完整：补丁 + blob 回退 + 修复 + 跨版本；无多基线/流式 apply |
| 安全性与签名校验 | 8.5 | 信封 + 签名清单 + journal 绑定 + pinned keys + 逐跳重定向；缺链接防护 |
| 可靠性与回滚 / 失败恢复 | 8.5 | WAL、幂等重放、旧哈希快照、崩溃循环、跨卷、真实 worker 进程矩阵 |
| 跨平台兼容 | 6.0 | 主动收敛 Windows-only；Windows 内部矩阵仍有实机项未验证 |
| 性能与资源占用 | 7.0 | Range 并发、哈希缓存、并行校验；I/O 放大 3–5 次、无文件内进度 |
| Tauri 2.x 集成与权限 | 8.5 | 一行入口、漏接检测、降级启动、三档权限、参数来源契约 |
| 代码质量与可维护性 | 8.0 | 命名清晰、注释解释“为什么”、错误三表穷尽；状态机未接线 |
| 测试覆盖 | 8.0 | 341 项本地通过 + 8 e2e + 真实 worker；无覆盖率/fuzz/semver 门禁 |
| 文档完善度与准确性 | 6.5 | 体系完整、思考深；但多处可复制的具体错误 |
| 工程实践 / CI / 发布 | 7.5 | fmt/clippy/doc/test/contract/docs-check/version-sync/deny；缺 MSRV/覆盖率 |
| **开发者体验（DX）** | **7.0** | 入口与诊断优秀；默认权限闭环与文档版本号是硬伤 |
| 用户体验 / 产品完整度 | 7.0 | 静默准备体验好；确认语义、进度粒度、回滚提示仍有缺口 |
| 社区生态与长期演进 | 5.5 | rc 未正式发布、单维护者、中文为主、Windows-only |

**综合：7.6/10。作为 Windows 自托管更新内核的生产就绪度约 7.5/10；作为“公开跨平台产品”不适用（文档已明确不支持）。**

### 0.3 三视角评价（用户 / 开发者 / AI Agent）

| 视角 | 得分 | 最关键的一条 |
| --- | ---: | --- |
| 用户体验 | 7.0 | 更新准备完全后台化，但“已回滚”提示会被下一次 check 清掉，大文件长时间停在同一个百分比 |
| 开发体验 | 7.0 | `run_with_delta_update` 与 `doctor --simulate-failure` 很出色；但默认 `manage` 权限不含 `confirm_healthy`，文档却让前端调用它 |
| AI Agent 体验 | 7.5 | 错误码/JSON 契约/命名概念对 Agent 友好；但文档存在多份“事实来源”且互相漂移，Agent 照文档改会引入错误 |

---

## 1. 评审方法与证据

### 1.1 实际执行

| 命令 | 结果 |
| --- | --- |
| `cargo check --workspace --all-targets` | **通过**，14.98s |
| `cargo test -p delta-update-engine` | **203 passed**（lib 61 + 集成 142） |
| `cargo test -p tauri-plugin-delta-update` | **39 passed**（4 个 doctest ignore） |
| `cargo test -p delta-update-cli` | **99 passed** |
| 静态统计 | Rust `#[test]` 342 个；TS `it/test` 16 个；e2e 场景 8 个；bench 4 个 |
| 源码阅读 | engine 全模块、plugin 全部、CLI 全部、plugin-api 全部、e2e/xtask、CI/权限/示例 |
| 文档阅读 | 设计、参考、运维、测试、安全、RFC、CHANGELOG、README |

> 环境说明：本机在执行 `cargo test --workspace` 长时间编译时，工具链的构建产物会被运行环境回收，导致后续 test target 出现“trait 不在作用域 / crate 不在 rlib 形式”的伪错误（`cargo check` 与分 crate 运行均通过）。这属于评审环境问题，不是源码缺陷；但它也从侧面证明 `CONTRIBUTING` 中“发布必须 clean build、不要跨路径复用 target”的要求是必要的。

### 1.2 局限

- 未执行真实 UAC 提权交互与真实 COS/S3 发布（无凭据/交互环境）。
- 未在 Win10 22H2 / Win11 23H2 / LTSC / Server 实机验证；未在真实 EDR/Defender 下验证文件占用重试。
- 未做长时间大目录性能压测；性能结论来自代码路径、基准文件与文档口径。

---

## 2. 分维度评审

### 2.1 架构设计与模块边界（8.5/10）

**做得好的地方**

- 依赖方向严格单向：engine 无 Tauri，plugin 只做生命周期映射，CLI 复用容器/哈希/清单实现。
- 关键抽象可扩展：`PatchApplier` 注册表（`staging/prepare.rs:22-87`），新增策略只需实现 trait + 注册，不污染主流程。
- 状态与契约分层清晰：`EngineState` 是 wire 枚举，`ErrorPayload` 是 UI 契约，`journal::state` 是磁盘状态。
- `#![forbid(unsafe_code)]`（engine）与插件单点 `unsafe`（ShellExecute）边界明确。

**不足**

1. 公开面仍偏大：`lib.rs:8-28` 仍 `pub mod` 导出 `apply/journal/patch/staging/state/fs/path`，`ReplayContext`、`PreparedPlan`、`HashCache`、`RealFs` 等都成为 1.x 的 SemVer 维护对象。这与“1.0 公共 API 已冻结”的承诺形成长期维护成本。
2. 状态机没有成为唯一写入口：`state/machine.rs` 的 `can_transition_to/transition_to` 只在单元测试中使用，插件通过 `PluginState::set_state` 直接写入（`plugin/state.rs:126-130`）。因此 `E_INVALID_TRANSITION` 只是命令层的零散 busy 判断，不是全局不变量。
3. 少量文档/代码职责重叠：engine 文档说“CLI 与插件复用同一逻辑”，但 `prepare` busy 语义、诊断导出路径选择分别在插件与 CLI 各写一遍。

**建议**

- 1.1.0 把 `lib.rs` 收敛为 curated re-export（只留 `Engine`、`EngineConfig`、`Limits`、`CheckOutcome`、`PreparedUpdate`、`PendingCommit`、`StartupOutcome`、`Progress`、错误类型、`Fetcher`/`FileSystem`），其余 `pub(crate)`；已公开项用 `#[deprecated]` 过渡一个 minor。
- 把状态迁移做成唯一入口：`PluginState::transition(next) -> Result<()>` 内部校验并持久化，命令与恢复都只走它；`Committing/RollingBack` 要么给出真实进度，要么从 wire 状态中移除。

### 2.2 增量算法 / 差分与合并（7.5/10）

**做得好的地方**

- `.tdp` 容器头（magic/format/strategy/codec/target_size/source_sha/target_sha）在解压前即可拒绝错配（`patch/container.rs:32-84`）。
- 运行侧双哈希：应用前校验 source、应用后校验 target；补丁被视为不可信输入。
- 发布侧内容寻址：所有条目（含 Unchanged）都指向 `blobs/<sha256>`（`pipeline/mod.rs:279-369`），使跨版本升级、本地损坏修复、回滚校验同时成立。
- 补丁回退链完整：压缩扩展名/超大文件/比值超限 → 全量；补丁失败 → 全量（`staging/prepare.rs:406-428`）。
- 内存预算：apply 侧按 `source + payload × 2` 与 `patch_memory_factor` 拒绝（`patch/apply.rs:29-60`）；CLI 侧按“最大 jobs 个输入之和 × 6”估算（`pipeline/patches.rs:43-86`）。

**不足**

1. 只有单基线：CLI 只接受一个 `--from`，补丁键是 `<from>-to-<to>`；跨多个版本升级完全依赖 blob 全量。流量收益在“跳版本”场景丢失。
2. 无多策略候选：未实现 zstd-19 与 level-3 双候选取小，未使用 zstd 字典；对文本/JS/资源仍有可压缩空间。
3. apply 非流式：一次性读入 source + patch + 解压 payload（`patch/apply.rs:62-85`），大文件内存峰值高；`--patch-max-file-size` 默认 64 MiB 是发布侧阈值，客户端 `max_patch_file_size` 默认 256 MiB，两侧口径不同。
4. 补丁对象未压缩全量对象：blob 是原始字节，新增文件/回退全量没有 gzip/brotli/zstd 传输层优化（可交给 CDN，但文档未强制）。

**建议**

- 1.1.0 评估多基线：清单增加 `from` 索引或 `deltas/<from_sha>-<to_sha>`，客户端按本地文件哈希选补丁，找不到即 blob。
- 发布侧增加 zstd level 19 候选（在内存预算内），并给出“收益 > 成本”的开关；对文本类启用 zstd 字典。
- 大文件 apply 分块读写，或把客户端 `max_patch_file_size` 与发布侧 `--patch-max-file-size` 对齐，避免“发布端生成了补丁、客户端拒绝应用”。

### 2.3 安全性与签名校验（8.5/10）

**做得好的地方**

- 单对象信封优先：`latest.envelope.json` 把清单文本与签名放在同一对象，新客户端不再有“清单/签名错配窗口”（`engine.rs:297-348`、`pipeline/publish.rs:153-192`）；信封存在但验签失败直接拒绝，不回退旧路径。
- 先验签后解析：`verify_manifest` 在 `parse_manifest` 之前。
- journal 绑定签名清单：`commit.meta.json` + `verified_manifest.json` + `install_root` 精确等于 worker 目录 + 每个 `Place` 哈希来自签名清单（`journal/verify.rs:22-168`、`apply/applier.rs:275-336`）。
- 回滚使用受信旧哈希：`old_hashes.json` 在提交前捕获，`Restore` 不接受“备份自身现算哈希”（`rollback/inverse.rs:42-51`）。
- 重定向逐跳校验 scheme/host/allowed_hosts，最多 5 跳（`fetch/http.rs:126-152`）。
- 路径校验覆盖 `..`、绝对路径、Windows 非法字符、保留设备名、段尾空格/句点、大小写冲突、保留目录 `.old/.trash`（`path/rel_path.rs:40-114`）。
- 编译期 pin：`pinned-keys` + `set_trusted_keys(Intersect)`，交集为空拒绝启动；信任根解析后冻结（`plugin/pinned.rs`）。

**不足**

1. **无符号链接 / junction / reparse point 防护**：全仓无 `symlink_metadata`/`is_symlink`/reparse 检查；journal 重放直接 `root.join(validate(...))`（`journal/replay.rs:216-232`）。当前依赖“路径词法校验 + install_root 与 exe 精确相等”，对纯词法逃逸足够，但不构成纵深防御。
2. **签名服务响应无大小上限**：`response.json()` 直接解析（`fetch/signing.rs:80-83`），恶意/故障服务可返回巨大 JSON。基线 P2-07 仍在。
3. **自定义 headers 不到签名服务**：`headers` 只加在清单/签名请求（`fetch/http.rs:361-363`），私有桶的签名服务无法携带 `Authorization`，企业接入受限。
4. 文档安全承诺与实现不完全一致：`design/security.md` 声称“拼接后再次验证规范化结果仍在根目录之下”，但 `resolve_within` 未被 journal 写入路径使用；实际防护是词法校验 + worker scope。
5. 本地 DoS 面仍在：可反复触发 prepare 消耗带宽/磁盘；`staging_size_limit` 只是上限，不是配额。

**建议**

- 1.0.x：`resolve` 前对每个已存在路径段做 `symlink_metadata`/reparse 拒绝；`install_root`/staging 启动时 canonicalize 一次并缓存；新增“junction 指向外部目录时 Place 拒绝”的故障注入用例。
- 1.0.x：签名服务响应限 64 KiB；把 `headers` 透传给签名服务（或新增 `signing_headers`，避免把清单鉴权头发给第三方）。
- 文档改写：把“规范化后复核”改为“词法校验 + worker 作用域 + （计划中的）链接拒绝”，不要描述未实现防线。

### 2.4 可靠性与回滚 / 失败恢复（8.5/10）

**做得好的地方**

- write-ahead journal：先写 `apply.json`/`commit.meta.json`/`commit.journal` 并 fsync，再触碰安装根；提交点清晰。
- 幂等重放：`Backup/Place/Trash/Restore/RemoveTrash` 都有 `is_done` 判定；`Place/Restore` 校验来源与目标（`journal/replay.rs:84-194`）。
- 回滚恢复收尾完整：`finish_resumed_rollback` 先写 `skipped_version`，再清理 commit/rollback/status/prepared/apply 与备份（`recovery.rs:247-277`）。
- 崩溃循环 + 正常退出折扣：`discount_clean_exit` 只在 Completed 且超过宽限期时减一（`engine.rs:770-789`）。
- 下载韧性：同长度损坏先删再下（避免 416）；Range 403 刷新预签名后回退单流续传；staging 复用已下载补丁（`staging/prepare.rs:609-693`、`fetch/http.rs:895-938`）。
- 跨卷 copy+fsync+rename 回退，并有 `CrossesDevices` 注入用例。
- 真实 worker 进程测试：testkit 把 `worker_harness` 复制进临时安装根，用真实参数启动，覆盖 scope 校验、apply.json 交接、验签与提交（`crates/delta-update-testkit/tests/worker_process.rs`）。
- 子进程崩溃矩阵：`fault-injection` feature 在 op 前/后 `abort`，断言重启重放结果一致。

**不足**

1. `Committing { processed: 0, total: 0 }` 是固定值（`plugin/commands.rs:569-576`），UI 无法展示替换进度；`RollingBack` 在插件中从不发布。
2. `confirm_healthy` 在非 Completed 分支直接 `reset_attempts` 且不取锁（`engine.rs:667-675`），与 worker 的 `increment_attempts` 存在写竞态。
3. 单实例仍是“建议”而非强制：`begin_commit` 有 `commit.journal` 存在检查，但应用级双实例仍可能互相覆盖 PREPARED/check 缓存。
4. `move_path` 先删目标再 rename（Windows 语义），存在目标短暂缺失窗口；崩溃由 journal 恢复，但文档“任何时刻要么旧要么新”不严谨。
5. 清理是 best-effort：`remove_dir_all_best_effort` 失败只告警，历史 commit 的 `.old/.trash` 可能长期残留（`engine.rs:704-721`）。

**建议**

- worker 每处理 N 个 op 落一次进度（`commit.status` 增加 `processed/total`），或把 `Committing` 标为 indeterminate 并在 UI 明确；`RollingBack` 至少在启动回滚前发布一次。
- `confirm_healthy` 的 reset 也走 `acquire_update_lock_retrying`，或改成 CAS 写 attempts。
- 文档收紧“原子性”表述：Windows 上是“崩溃后可恢复”，不是“任何时刻原子可见”。

### 2.5 跨平台兼容（6.0/10）

**结论**：这是一次**正确的战略收敛**。Windows-only 在编译期强制（`plugin/lib.rs:73-76`），`ensure_supported` 返回 `E_UNSUPPORTED_PLATFORM`，文档/README/CHANGELOG/贡献指南一致。作为“跨平台产品”打 6.0 分是刻意的范围约束，不是实现缺陷；作为 Windows 更新器，内部矩阵仍有实机项未验证。

- 已验证：workspace 测试、e2e、跨卷、云占位检测、路径矩阵。
- 未验证：真实 UAC 交互、per-machine 提权恢复、Defender/EDR 占用重试、ReFS/网络共享、Win10 22H2/LTSC/Server、BitLocker。
- 残留：Windows 长路径已用 `long_path` 处理；PE Authenticode 在替换主 exe 后会失效，文档只提“签名应发生在扫描前”，未说明运行时替换后的签名保持策略。

**建议**：把 `docs/reference/compatibility.md` 的“未验证”列作为发布门禁，每项验收后回填；如果主 exe 有 Authenticode，明确“更新后签名由发布包保证，文件替换不会重签”的产品口径。

### 2.6 性能与资源占用（7.0/10）

**做得好的地方**

- 大对象 Range 分块（8 MiB × 4 路），失败回退单流；合并用临时文件 + 原子 rename。
- 哈希缓存按 size+mtime 复用；`prepare` 对未变化文件用 rayon 并行校验（`staging/prepare.rs:320-370`）。
- 未变化文件不复制到 staging，峰值磁盘从 `2×|R|` 降到 `|R| + 变化量`。
- 进度事件 100ms 合并；CLI 内存预算与告警。
- 基准：`hash_tree`、`patch_header`、`journal_verify`、`scan_tree` + `bench-check` 阈值。

**不足**

1. I/O 放大：变化文件在 `prepare` 写入时校验一次、`verify_plan` 一次、`begin_commit` 的 `verify_plan` 再一次、worker replay 来源 + 目标各一次，最坏 5 次全文件读。
2. `check` 的本地扫描哈希是串行的（`engine.rs:915-968`），只有 `verify_unchanged` 并行；大安装首次 check 慢。
3. 无文件内字节进度：`Progress::Downloading` 在文件完成后才累加整个文件大小（`staging/prepare.rs:545-560`）。
4. 连接峰值：`max_concurrent_downloads` × `range_concurrency` 可达 16；无全局信号量、无带宽限速。
5. 无 ETag/304：每次 check 完整拉取清单 + 签名；CDN 成本与首字节延迟。
6. CLI 上传串行（`pipeline/publish.rs:36-41`）；`Publisher::put_file` 默认实现把整文件读进内存（`publisher/mod.rs:13-16`），FsPublisher 也走该默认。
7. `sysinfo` 每 200ms 轮询父进程，最长 600s（`apply/applier.rs:367-390`），对本机等待偏重。

**建议**

- `VerifiedInventory`：staged 文件写入时记录 `(size, mtime, sha256)`，`verify_plan`/`begin_commit` 只校验 size+mtime，跨进程恢复才全量重哈希。
- `check` 复用同一 rayon 池并行哈希；增加 `--hash-jobs`/配置项。
- 下载层加全局 `Semaphore`（文件 × Range 总和）与可选带宽上限；`Progress::Downloading` 增加当前文件已下载字节。
- 清单请求加 `If-None-Match`/`If-Modified-Since`，304 时复用已验证清单。
- CLI 上传并行 + 重试 + 大对象 multipart；`FsPublisher::put_file` 用 `fs::copy` 而非整文件读。

### 2.7 Tauri 2.x 集成与权限（8.5/10）

**做得好的地方**

- 一行接入：`BuilderExt::run_with_delta_update` 在 GUI 之前处理 worker；漏接时 debug panic、release 降级（`plugin/lib.rs:176-215`、`lifecycle.rs:197-217`）。
- 启动失败降级：配置/平台错误 → `PluginState::disabled` + `Failed` 事件，主窗口照常启动（`lifecycle.rs:305-317`）。
- 权限分档：`default`（全量）、`manage`（除 confirm）、`monitor`（只读），并有 ACL 契约测试。
- 参数来源契约：`param_provenance_contract` 静态断言命令不接收路径/公钥/版本。
- 事件协议带版本号、进度单位、错误 action；TS SDK 提供 controller/hook/类型守卫。
- 生命周期完整：setup 恢复、auto_check、check_interval、退出 auto_apply、窗口就绪 auto_confirm。

**不足**

1. **默认权限与健康确认闭环冲突**（最重要的 DX 问题）：`init` 默认写 `manage`（`commands/init.rs:342-347`），`manage` 不含 `confirm_healthy`；接入指南要求前端在 UI 就绪后调用 `confirm_healthy()`（`getting-started.md:338-366`），而 API 文档明确“没有 Rust facade”（`api.md:639`）。按文档做，前端调用会被 ACL 拒绝；不调用则依赖 `auto_confirm=false` 的默认 + 正常退出折扣。
2. `prepare` 在 busy 时返回 `Ok(PrepareResult{state: 当前状态})`（`commands.rs:356-363`），调用方无法区分“已开始/未开始”，与 `check`/`restart_to_apply` 的 `E_INVALID_TRANSITION` 不一致。
3. `Committing` 固定 0/0、`RollingBack` 不发布（见 2.4）。
4. `cancel_prepare` 只能取消 prepare 的下载/校验，不能取消 check；`verify_plan` 阶段不可取消。
5. 插件不内置日志初始化；宿主不装 subscriber 时零日志（`troubleshooting.md` 已说明），但首次接入的可调试性依赖宿主自觉。

**建议**

- 1.0.x（文档）：`init` 输出明确二选一：`--permission-set default`（前端确认）或 `auto_confirm: true`（插件兜底）；在接入指南的“四步”里把这一选择写成必答项。
- 1.1.0：新增 `delta-update:manage-confirm` 权限集（manage + confirm_healthy），避免“要么全给 default，要么没确认”。
- `prepare` 返回 `started: bool` 或改为 `E_INVALID_TRANSITION`，TS 类型同步；契约测试锁定。
- 可选 `logging` feature 提供一行 `tracing_subscriber` 初始化，文档标注与 `tauri-plugin-log` 的桥接方式。

### 2.8 代码质量与可维护性（8.0/10）

**做得好的地方**

- Rust 风格统一，错误用 `thiserror`；`EngineError` 的 `code`/`action`/`user_action` 三张穷尽表，新增变体不补建议就编译失败（`error.rs:349-503`）。
- 注释普遍解释“为什么”（Windows rename 语义、重试策略、发布窗口、锁不可重入），而不是复述代码。
- 故障注入通过 `FileSystem` trait 注入，不污染生产控制流。
- CLI 输出统一 `output::info/warn/error`，`--json` 信封统一。
- 无 `utils.rs`/`helpers.rs` 万能收集器；目录规范有文档与 xtask 检查。

**不足**

1. 公开面过大（见 2.1），维护成本与 SemVer 风险。
2. `EngineError::Internal(String)` 仍承担大量上下文；`ErrorPayload.detail` 恒为空（`error.rs:505-515`），UI 与工单只能解析字符串。
3. Rust `StatePayload`/`CheckResult` 与 TS `types.ts` 手工维护两份，无生成机制，已经出现字段增减不同步的风险。
4. `state::machine` 死代码（仅测试）；`FileEntry.executable` 保留字段（文档已标注）。
5. `docs/reference/api.md` 的 `EngineError` 代码块缺 `ManifestInvalid`/`ManifestTooLarge`/`JournalFormatTooNew`/`JournalReplayFailed`/`ElevatedRecoveryTimeout` 等变体，与“以本文为准”冲突。

**建议**

- 用 `ts-rs`/`specta` 从 Rust 生成 TS 类型；用 `schemars` 生成插件配置 JSON Schema 并随包发布。
- `detail` 填充脱敏结构（path/op/status），或从协议中删除该字段，避免“永远为空”的误导。
- `api.md` 的枚举块改为“示例 + 链接 rustdoc”，以 rustdoc 为单一来源。

### 2.9 测试覆盖（8.0/10）

**做得好的地方**

- 341 项本地通过（本次评审实测），覆盖引擎、插件、CLI、契约、故障注入、property test、真实 worker 进程、CLI 发布管线、前端 controller。
- 契约测试先行：`*_contract.rs` 覆盖错误码、事件、清单、签名、权限、参数来源。
- 故障注入断言“注入确实命中”（`FaultyFs::fired()`、`MockServer::faults_served()`），避免假通过。
- e2e 8 个场景：fresh install、delta update、crash-loop rollback、UAC cancel、cross-volume、forced cross-volume、rollback release、http serve。
- `xtask ci` 包含 fmt、clippy -D warnings、rustdoc -D warnings、test、real-test 编译、contract-check。

**不足**

1. 无覆盖率门禁（`cargo-llvm-cov`）、无 fuzz（manifest/journal/patch header）、无 `cargo-semver-checks`。
2. 无真实 UAC / 真实 COS 的自动化 E2E（文档已承认是人工验收）。
3. `--no-verify` 发布依赖 Windows verify job 的“同一棵树”，但未在发布 job 内重新跑最小 smoke（如 `delta-update --version` + `doctor --check-config`）。
4. `examples/minimal-tauri-app` 不在 workspace，CI 只检查 `real-test`；示例代码漂移风险高。
5. 测试计划里仍有“L4 真实 Tauri 应用 + 真实 COS”的表述，与当前自动化证据不完全匹配（文档已部分修正，需逐条对齐）。

**建议**

- 加 `cargo-llvm-cov`（核心模块阈值）、`cargo-fuzz` smoke（manifest/journal/patch header）、`cargo-semver-checks`（1.x 每次 PR）。
- 把 `examples/minimal-tauri-app` 加入 xtask 的 `cargo check` 列表。
- 在发布 job 增加“安装 crates 后跑 `delta-update --version` 与 `doctor --check-config`”的 smoke。

### 2.10 文档完善度与准确性（6.5/10）

**做得好的地方**

- 文档体系完整、分层清晰（接入/参考/设计/运维/测试/安全/RFC），`docs/README.md` 有维护规则与阅读顺序。
- 安全威胁模型、密钥轮换、发布顺序、回滚语义的思考深度明显高于一般项目。
- `cli-contract.md` 把退出码与 JSON 信封固化为 CI 可依赖的契约；`troubleshooting.md` 给出物理证据目录树。
- `docs-check`、`version-sync`、契约检查进入 CI。

**不足（按“开发者会照抄”排序）**

1. `getting-started.md:83` 与 `migrate` 生成指南（`pipeline/mod.rs:995`）写 `tauri-plugin-delta-update = "0.1"`，实际是 `1.0.0-rc.4`。
2. `architecture.md:280` 写“默认阈值 2 / 第三次启动回滚”，实际默认 3 且判定是 `attempts > 3`（第四次启动）。
3. `architecture.md:311`、`modules.md:332-333`、插件 README 第 3 行写“六个/九个命令”，实际 10 个（含诊断与历史）。
4. `architecture.md:119-127`、`protocol.md:135/882` 的全量对象示例仍是 `artifacts/full/<version>/...`，实际 `blobs/<sha256>`。
5. `modules.md:178-191` 仍有 `CommitJournal`/`begin`/`mark_completed` 幻影 API；实际是 `Journal` + `journal::plan::build`。
6. `api.md:696-778` 的 `EngineError` 枚举块缺多个现有变体。
7. 插件 README 说“非 Windows 在 setup 阶段禁用并返回 `E_UNSUPPORTED_PLATFORM`”，实际非 Windows 直接 `compile_error!`，setup 分支不可达。

**建议**

- 把“文档中的版本号/命令数/对象键布局”做成 xtask 断言：从 `Cargo.toml` 读版本、从 `init()` 的 handler 数量读命令数、从 `pipeline::manifest` 读对象键前缀，文档不匹配即失败。
- 删除 `CommitJournal` 块，改为真实签名；`api.md` 的枚举改为 rustdoc 链接。
- 把“规划中”与“已实现”用统一标记（如 `> 状态：已实现 / 规划 / 不支持`）逐节标注。

### 2.11 工程实践 / CI / 发布（7.5/10）

**做得好的地方**

- CI：Windows 完整门禁（fmt/clippy/doc/test/contract）、docs-check、version-sync、pinned-keys 契约、e2e、前端 typecheck/build/test。
- `cargo-deny`（advisories/licenses/bans/sources）+ dependabot。
- 发布顺序正确：对象 → 附件签名 → 版本化清单 → latest 签名 → latest 清单 → 信封；每一步 HEAD 校验。
- 发布与验证解耦：publish 在 Ubuntu，`--no-verify`，Windows verify job 先跑完；crates.io 用 REST API 轮询；npm 预发布打 `next` tag。
- `release --pipeline` 自测 + 审计 + 报告；`report --history` 覆盖率趋势；`rollback --to-version` 只改 latest。
- 签名后自验，避免发布客户端必拒的签名。

**不足**

1. 无 MSRV job：`rust-version = "1.90"`，CI 用 stable 最新；依赖升级可能悄悄抬高 MSRV。
2. actions 未 pin commit SHA（供应链）。
3. 无覆盖率/fuzz/semver-checks/bench 回归阻断（bench 有 workflow，但未见 PR 阻断策略）。
4. 发布 workflow 未创建 GitHub Release / tag / checksums / SBOM 归档；回滚素材保留策略只写在文档。
5. 没有发布并发保护（两个 release 同时写 latest 仍是后写覆盖）；CLI 不检查远端当前版本是否更高。
6. `tauri = "2"` 这种宽版本写在 workspace（`Cargo.toml:48`），配合 `Cargo.lock` 可复现，但文档“已验证 Tauri 版本”与实际解析版本可能漂移。

**建议**

- 增加 MSRV job（1.90）与 `cargo-semver-checks`；actions pin SHA。
- release workflow 增加 GitHub Release + SHA256SUMS + SBOM 附件；发布前检查远端 `latest.json` 版本，拒绝对已发布更高版本做普通发布（除非 `--rollback`）。
- 文档明确“已验证的 Tauri 版本 = Cargo.lock 中的版本”，并提供升级 Tauri 的验证清单。

### 2.12 开发者体验（DX）（7.0/10）

| DX 子维度 | 得分/10 | 主要依据 |
| --- | ---: | --- |
| 便携性 / 可移植性 | 5.5 | 主动 Windows-only；无安装器集成；移动仓库复用 target 会嵌入旧 `CARGO_MANIFEST_DIR` |
| 开箱即用 | 7.5 | `BuilderExt` 一行 + `init` + `doctor` + `serve` + 5 分钟 real-test |
| 接入成本 | 7.5 | 四步；无自定义 trait；必须改 `main.rs`（有漏接检测兜底） |
| 配置复杂度 | 7.0 | 默认值安全、未知字段告警、有 JSON Schema；字段仍多 |
| API / CLI 易用性 | 7.5 | 命令窄、`--json`、退出码、`simulate-failure`；`prepare` busy 语义不一致 |
| 学习曲线 | 6.5 | 必须理解清单/journal/回滚/提权；文档长且部分过期 |
| 错误提示 / 日志 / 可观测性 | 7.0 | 错误码 + action + user_action 三表；worker.log 结构化；`detail` 空、默认无宿主日志 |
| 调试体验 | 7.0 | `inspect`/`--bundle`/`doctor`/`export_diagnostics`；无实时引擎日志 |
| 示例与文档质量 | 6.0 | 示例 3 秒无条件 confirm；依赖版本号错误 |
| 发布 / 升级流程 | 8.0 | 原子顺序、信封、自测、审计、回滚；上传串行、无缓存头 |
| CI/CD 集成 | 7.0 | CLI 契约适合 CI；无官方 Action/Docker；actions 未 pin |
| 依赖与版本兼容 | 7.0 | MSRV 1.90 偏高；tauri 宽版本；无 semver-checks |
| 迁移与维护成本 | 7.0 | `migrate` 有前端对照表；公开面大、文档漂移 |
| 社区生态 / 长期风险 | 5.5 | rc 未正式发布、单维护者、中文为主、Windows-only |

**关键 DX 断点**

1. 照接入指南写依赖 `0.1` → 解析失败。
2. 照 `init` 默认 `manage` + 文档“前端调用 confirm_healthy” → ACL 拒绝。
3. 示例 3 秒 confirm → 教学错误。
4. 默认没有日志落盘 → 第一次接入只能靠状态事件与 `inspect`。
5. 文档多处具体事实过期 → 开发者无法判断“这是现状还是规划”。

### 2.13 用户体验 / 产品完整度（7.0/10）

- 优秀：后台准备、退出替换、重启即新版本、无安装器；失败有 action 文案；诊断包可一键导出。
- 缺口：进度只有文件级/字节级累计，无文件内进度；大文件长时间停在同一百分比；`Committing` 无进度；回滚提示会被下一次成功 check 清掉（`commands.rs:104-124` 的 `resting` 只保留 Committed/Prepared）；`prepared` 的 UAC 恢复只弹一次中文 MessageBox，无品牌/说明链接；`mandatory`/`update_required` 只透传，引擎不强制。
- 建议：回滚状态持久到用户确认；下载/应用分阶段进度；`Committing` indeterminate；UAC 前给出“为什么需要管理员”的说明页；把“跳过此版本/稍后提醒”做成稳定 API（`skipped_version` 已有底层）。

### 2.14 社区生态与长期演进（5.5/10）

- 优势：仓库元数据齐全（repository/homepage/bugs）、crates/npm 发布流水线、RFC 流程、安全公告模板、贡献指南、issue 模板、dependabot。
- 风险：单维护者；中文为主；Windows-only 限制用户面；与官方 `tauri-plugin-updater` 竞争且接入成本更高；自定义清单/对象布局形成迁移锁定；无公开 roadmap/讨论区；MSRV 1.90 对企业 CI 不友好。
- 建议：发布 1.0 后补英文 README/quickstart 与 docs site；建 GitHub Discussions；给出 roadmap 与弃用政策；提供官方 Action + 模板仓库；用 `cargo-semver-checks` 降低下游维护成本。

---

## 3. 真实接入与升级场景演练

| # | 场景 | 当前实际 | 结论 |
| --- | --- | --- | --- |
| S1 | 新项目照接入指南 `cargo add tauri-plugin-delta-update = "0.1"` | crates.io 无 0.1，解析失败 | **P0 文档** |
| S2 | `init` 默认 `manage`，前端按文档调用 `confirm_healthy` | ACL 拒绝 `plugin:delta-update|confirm_healthy` | **P0 默认配置闭环** |
| S3 | 忘记 `run_with_delta_update` | debug panic / release 降级提示 | 可用（已改善） |
| S4 | `auto_check=true` + `auto_apply=false` + 应用不 confirm | 正常使用不累计（退出折扣）；真实崩溃 4 次回滚；removed 文件永不删除（known_files 不更新） | 可接受，需文档明确 |
| S5 | v1 → v3 跳版本 | 单基线无 v1→v3 补丁，走 `blobs/<sha>` 全量修复 | 可用，非最优流量 |
| S6 | 本地未变化文件被隔离/损坏 | `prepare` 检测不匹配 → 全量修复 | 已修复 |
| S7 | 回滚中途断电 | 重启继续回滚并清理 commit 状态 | 已修复 |
| S8 | per-machine 本地用户伪造 apply.json + journal | worker 验签清单、绑定根目录与哈希 | 已修复 |
| S9 | 私有桶 + Range 下载中预签名过期 | 403 → 刷新 → 单流续传 | 已修复 |
| S10 | 上次留下同 size 损坏全量对象 | 先删再下，不再 416 | 已修复 |
| S11 | `.old` 备份损坏后回滚 | 用 `old_hashes` 受信哈希拒绝 | 已修复 |
| S12 | 安装目录内存在 junction 指向外部 | 词法校验通过，写操作跟随链接 | **P1 纵深缺口** |
| S13 | 两个实例同时 prepare/退出 | `commit.journal` 存在检查；仍建议 single-instance | 部分缓解 |
| S14 | CI 发布 500MB 对象 | 单 PUT + 1800s 超时，无 multipart/断点 | P2 |
| S15 | 大文件补丁 apply，内存 256MB | 预算拒绝或全量回退；无流式 | P2 |
| S16 | 企业私有签名服务要求 Authorization | headers 不透传签名服务 | **P1 企业可用性** |
| S17 | 前端只监听事件，不读 status | `setup` 早期事件可能早于页面加载；controller 已处理 | 可用 |
| S18 | 应用长期驻留，配 `check_interval_secs` | 周期 check+prepare，跳过 busy/prepared | 可用 |

---

## 4. 问题清单（按优先级）

> 每条包含：判断依据 → 影响范围 → 改进建议。行号基于本次评审代码。

### P0（发布前必修，均为文档/默认配置，1 天内可完成）

**N-01 接入文档依赖版本错误**
- 依据：`docs/guides/getting-started.md:83`、`crates/delta-update-cli/src/pipeline/mod.rs:995` 写 `"0.1"`；workspace 实际 `1.0.0-rc.4`。
- 影响：新项目第一步就失败；rc 期间 npm 用 `@next` 也同样需要完整版本。
- 建议：改为 `1.0.0-rc.4`；在 `xtask contract` 增加“文档中的 crate 版本 == workspace 版本”断言。

**N-02 默认权限与健康确认闭环冲突**
- 依据：`commands/init.rs:342-347` 默认 `manage`；`permissions/manage.toml` 不含 `allow-confirm-healthy`；`getting-started.md:338-366` 让前端调用；`api.md:639` 明确无 Rust facade；`init.rs:152-155` 又说“由 Rust 调用”。
- 影响：按推荐默认接入的应用，前端 confirm 被拒；不 confirm 则健康语义缺失（真实崩溃 4 次回滚）。
- 建议：1.0.x 文档给出明确二选一；1.1.0 新增 `manage-confirm` 权限集（manage + confirm_healthy），或提供稳定 Rust facade。

**N-03 架构/安全文档与实现矛盾**
- 依据：阈值 `architecture.md:280`（2）vs `config.rs:366-368`（3）；命令数 `architecture.md:311`/`modules.md:332`/插件 README（6/9）vs `plugin/lib.rs:191-202`（10）；对象键 `architecture.md:119-127`/`protocol.md:135`（`artifacts/full`）vs `pipeline/mod.rs:279`（`blobs`）；`modules.md:178` `CommitJournal` 幻影；`security.md` 声称“规范化后复核”但 `resolve_within` 未被写入路径使用。
- 影响：安全评审、接入判断、故障排查都会基于错误事实；这是 1.0 发布可信度问题。
- 建议：逐条修正；把“安全防线”描述限制在已实现机制；用状态标记区分已实现/规划。

### P1（1.0.x 修复）

**N-04 签名服务响应无上限、headers 不透传**
- 依据：`fetch/signing.rs:62-99`；`fetch/http.rs:361-363` 仅清单/签名请求加 headers。
- 影响：恶意/故障签名服务可造成内存压力；企业私有签名服务无法带 Authorization。
- 建议：响应限 64 KiB；新增 `signing_headers` 或复用 `headers`；MockServer 增加超大 JSON 用例。

**N-05 无符号链接 / junction / reparse 防护**
- 依据：全仓无 `symlink_metadata`/reparse 检查；`journal/replay.rs:216-232` 直接 `root.join`。
- 影响：per-machine 提权 worker 的纵深防御缺口；文档“双重检查”表述过度。
- 建议：每个已存在路径段拒绝链接/reparse；canonicalize 根目录；补 junction 逃逸用例；或在文档明确接受该残余风险。

**N-06 `prepare` busy 返回 Ok**
- 依据：`plugin/commands.rs:356-363`。
- 影响：前端无法区分“已在准备/未开始”，与 `check`/`restart` 语义不一致。
- 建议：返回 `E_INVALID_TRANSITION`，或 `PrepareResult` 增加 `started: bool`；TS 类型与契约测试同步。

**N-07 状态机未接入**
- 依据：`state/machine.rs` 仅测试引用；`plugin/state.rs:126-130` 直接写。
- 影响：`E_INVALID_TRANSITION` 不是全局保证；非法状态组合只能靠命令层零散判断。
- 建议：`PluginState::transition` 唯一入口；契约测试断言非法迁移被拒。

**N-08 `confirm_healthy` 计数写竞态**
- 依据：`engine.rs:667-675` 非 Completed 分支直接 `reset_attempts`，不取锁；worker 同时 `increment_attempts`。
- 影响：崩溃循环计数可能被覆盖，回滚时机偏移。
- 建议：该分支也走 `acquire_update_lock_retrying`，或 CAS。

**N-09 示例 3 秒无条件 confirm**
- 依据：`examples/minimal-tauri-app/src/update.ts:32-38`。
- 影响：教学模板鼓励“窗口可见即健康”，与崩溃循环保护相悖。
- 建议：改为“业务 ready 后调用”的注释 + 默认不 confirm，或只在 `auto_confirm=true` 示例中演示。

### P2（性能/可维护性/工程）

**N-10 I/O 放大**：staged 文件最多 5 次全文件哈希（`staging/verify.rs:7-30`、`engine.rs:528/532`、`journal/replay.rs:146-153`）。建议 `VerifiedInventory` + size/mtime 复用。

**N-11 进度粒度**：`check` 串行哈希（`engine.rs:915-968`）；无文件内字节进度；`Committing` 固定 0/0（`commands.rs:569-576`）。建议并行哈希 + 阶段/字节进度。

**N-12 网络层**：无 ETag/304、无带宽限速、连接峰值 16（`fetch/http.rs:405-429` + `prepare` 的 `buffer_unordered`）。建议全局 semaphore + 条件请求。

**N-13 CLI 上传**：串行（`pipeline/publish.rs:36-41`）；`put_file` 默认整文件读内存（`publisher/mod.rs:13-16`）；无 multipart。建议并行 + 流式 + 分片。

**N-14 公开面过大**：`lib.rs:8-28`。建议 curated re-export + 弃用过渡。

**N-15 双份类型**：`plugin/commands.rs` 与 `packages/plugin-api/src/types.ts` 手工同步。建议 ts-rs/specta 生成。

**N-16 诊断上下文**：`ErrorPayload.detail` 恒空（`error.rs:505-515`）；`Internal(String)` 泛化。建议结构化 detail 或删除字段。

**N-17 CI 门禁**：无 MSRV、覆盖率、fuzz、semver-checks；actions 未 pin；无 GitHub Release/checksums/SBOM。建议补齐。

**N-18 hash cache 陈旧**：远程版本相同但本地文件损坏时，`check` 可能返回“已是最新”，不触发修复（`manifest/hash_cache.rs`）。建议远程版本相同时也做抽样校验或提供 `--rehash`。

**N-19 文档其余漂移（已修复）**：`api.md` 的 `EngineError` 枚举已与 `error.rs` 逐项对齐；插件 README 已改为“十个命令”并写明 `compile_error!` 的平台约束；`getting-started.md:164` 的权限说明已列全 10 个命令。

**N-20 生成物污染工作区**：`项目完整源码.md`（1.1 MB）与 `导出源码.bat` 在仓库根（已 gitignore）。建议移到 `target/` 或工具目录，避免搜索/评审噪音。

### P3

- `serve` 已改为按 `Range` 从磁盘流式发送，不再把对象整读进内存。
- `inspect` 内部 `process::exit`（`commands/inspect.rs:46-56`），难以库化/测试。
- `real-test` 与 `examples` 不在 workspace，示例靠手动检查。

---

## 5. 优化改进方案（按批次，兼顾用户 / 开发者 / AI Agent）

### 批次 0：发布前文档与默认值（阻断项）

1. 修 N-01/N-03/N-19：版本号、命令数、阈值、对象键、幻影 API、错误枚举。
2. 修 N-02：`init` 输出“前端确认 / auto_confirm”二选一说明；接入指南把该选择写进四步。
3. 修 N-09：示例改为业务 ready 后 confirm。
4. 新增 xtask 断言：文档版本号、命令数、对象键前缀与代码一致（对 AI Agent 尤其重要：减少“照文档改错”）。

### 批次 1：安全与可靠性收口（1.0.x）

1. N-04：签名服务 64 KiB 上限 + `signing_headers`。
2. N-05：链接/reparse 拒绝 + junction 逃逸测试；文档同步残余风险。
3. N-06：`prepare` busy 语义统一。
4. N-07：`PluginState::transition` 唯一入口。
5. N-08：`confirm_healthy` 计数加锁。
6. `Committing` 真实进度或 indeterminate；`RollingBack` 至少发布一次。

### 批次 2：性能与可观测性（1.0.x / 1.1.0）

1. `VerifiedInventory`，把 3–5 次哈希降到 1–2 次。
2. `check` 并行哈希 + 文件内/阶段进度。
3. 全局下载 semaphore + 带宽上限 + ETag/304。
4. CLI 上传并行 + 流式 + multipart。
5. `ErrorPayload.detail` 结构化或删除。
6. 诊断包增加“最近 7 天历史 + 关键事件时间线”。

### 批次 3：DX 与生态（1.1.0+）

1. 稳定 Rust facade（`engine()` / `check()/prepare()/confirm()`），解决 confirm 无门。
2. `manage-confirm` 权限集 + `init` 引导。
3. ts-rs/specta 生成 TS 类型；`schemars` 配置 Schema 随包发布。
4. 官方 GitHub Action + 模板仓库 + Docker 镜像。
5. 英文 README/quickstart + docs site + GitHub Discussions。
6. CI：MSRV、覆盖率、fuzz smoke、semver-checks、actions pin SHA、GitHub Release/checksums/SBOM。
7. 多基线 delta 与 zstd 候选（以真实流量数据决策）。

### 本轮已落地（2026-11-16 第二批）

| 条目 | 状态 | 实现要点 |
| --- | --- | --- |
| N-10 I/O 放大 | 已修复 | `VerifiedInventory` 记录 prepare 已哈希的暂存文件 `(size, mtime)`；`verify_plan`/`begin_commit` 命中即跳过，跨进程恢复仍全量重哈希。`begin_commit` 的旧文件快照同样复用同进程 `check` 扫描（size+mtime 校验，文件被改则重哈希）。重放阶段对源/目标哈希的校验不变 |
| N-11 进度粒度（文件内） | 已修复 | 新增 `Fetcher::download_with_progress`（默认转调旧方法）；`HttpFetcher` 在 Range 与单流写入时报告字节，`Progress::Downloading.completed` 100ms 节流连续增长 |
| N-11 `Committing` 0/0 | 保留 | worker 是独立进程，父进程拿不到真实 op 进度；协议里 `total = 0` 即 indeterminate，文档已标注 |
| N-12 条件请求 | 已修复 | `latest.json` 使用 `ETag`/`If-None-Match`，未变化返回 304；`MockServer` 支持 ETag 并有 `not_modified()` 断言 |
| N-12 连接峰值 | 已修复 | `FetchLimits::request_concurrency`（默认 8）+ 信号量，文件级与 Range 级合计受限；带宽限速仍未做 |
| N-07 状态机接线 | 已修复 | `can_transition_to` 与 `protocol.md` §6.2 及命令路径逐条对齐；`PluginState::set_state` 校验并告警未文档化迁移（不阻断 UI）；引擎/插件单测锁定全部边 |
| N-02 默认权限闭环 | 已修复 | 新增 `manage-confirm` 并作为 `init`/`migrate` 默认：前端 `confirm_healthy()` 开箱可用；`manage` 保留给严格划分，`default` 仅兼容 |
| DX：零前端代码 | 已修复 | `init --auto` 写入 `auto_check/auto_apply/auto_confirm=true`，不写任何前端代码即可完成“检查→退出替换→自动确认” |
| DX：接入路径与脚枪检测 | 已修复 | `getting-started` §1.1 给出三步零前端代码接入；`init` 输出依赖行；`doctor --check-config` 检测 `auto_confirm=false` 且 capability 未授予 `confirm_healthy` 时主动警告并输出 `confirm_health_granted` |
| 确认健康后的脏快照 | 已修复 | `confirm_healthy` 清理完成态后丢弃会话内的 `verified`/diff 与扫描哈希，下一次 `prepare` 重新 `check` |
| N-16 诊断上下文 | 已修复 | `ErrorPayload.detail` 输出结构化 `key=value`（状态码、字节数、schema/迁移版本），只含数值与枚举、不含路径 |
| N-15 双份类型 | 部分修复 | 新增 Rust↔TS 契约测试（状态名、事件名、错误/进度/状态 payload 字段）；类型仍手写两份，ts-rs 需新依赖，留待依赖可用时切换 |
| DX：默认零日志 | 部分修复 | 新增可选 `logging` feature，`init()`/`run_applier_from_context()` 自动装 subscriber 且不覆盖宿主；`DELTA_UPDATE_LOG` 控制级别 |
| 2.11 Tauri 宽版本 | 已修复（文档） | `getting-started` §11 以 `Cargo.lock` 解析出的 `tauri 2.12.5` 作为已验证版本，并列出升级时的重跑项 |
| N-18 陈旧 hash cache | 已修复 | `doctor --journal-dir --install-root` 用 journal 里的 installed/verified 清单离线校验安装树，缺失/哈希不符逐条列出 |
| 2.13 跳过此版本 | 已修复 | 新增 `skip_version` 命令（无版本参数，拒绝强制更新与已 Prepared），权限与 TS SDK 同步 |
| N-17 CI 门禁 | 部分修复 | 新增 coverage job（lcov artifact，阈值待基线）；semver-checks 无已发布基线、fuzz 需新依赖、actions pin 需真实 SHA，本环境未接入 |
| N-14 公开面过大 | 保留 | 收窄 `pub mod` 是破坏性变更，留到 2.0 并配弃用过渡 |
| N-13 CLI 上传 | 部分修复 | `upload_objects` 4 路并行、首个错误即停、HEAD 校验与发布顺序不变；`FsPublisher`/S3/COS 本就流式，multipart 与断点仍待做 |

### “最好的插件”目标形态（判断标准）

如果一个 Tauri Windows 应用要选增量更新插件，最好的形态应同时满足：

| 目标 | 当前差距 | 达成方式 |
| --- | --- | --- |
| 一行接入且不可能漏接 | 已达成（BuilderExt + 检测） | 保持 |
| 默认配置即可安全运行 | 部分（manage/confirm 闭环） | 批次 0/3 |
| 发布侧一条命令、CI 可审计 | 已达成（release --pipeline） | 补 Action/Docker |
| 失败可解释、可自愈、可回滚 | 已达成（错误三表 + journal + 回滚） | 补进度与 detail |
| 数据类型只有一份事实来源 | 未达成（Rust/TS 手写两份） | ts-rs/specta |
| 文档与代码不可能漂移 | 未达成 | xtask 断言 + rustdoc 单一来源 |
| 大目录/大文件性能可预期 | 部分（I/O 放大、无文件内进度） | 批次 2 |
| 企业网络可接入 | 部分（签名服务 headers/CA） | 批次 1 |

---

## 6. 发布门禁建议（1.0.0 最终版）

- [ ] 文档中的依赖版本、命令数、对象键、阈值与代码一致（xtask 断言）
- [ ] `init` 默认权限与 `confirm_healthy` 的使用路径无歧义（文档 + 1.1.0 权限集计划）
- [ ] 签名服务响应上限与 headers 透传（N-04）
- [ ] 链接/reparse 逃逸测试通过，或文档明确接受残余风险（N-05）
- [ ] `prepare` busy 语义与 TS 类型一致（N-06）
- [ ] `confirm_healthy` 计数竞态修复（N-08）
- [ ] 回滚中断 kill-point 测试通过，目录状态可判定（已具备）
- [ ] 伪造 journal + 宽 install_root 测试通过（已具备）
- [ ] 真实 worker 进程矩阵在 Windows CI 通过（已有 testkit 用例）
- [ ] `cargo check --workspace --all-targets`、分 crate 测试、`clippy -D warnings`、`docs-check` 通过
- [ ] 发布 dry-run：`cargo package --list` + npm pack + 安装后 smoke
- [ ] 人工验收：真实 UAC、per-machine 恢复、Defender 占用重试（compatibility.md 未验证列）

---

## 7. 总体评价

**优点（可长期依赖的部分）**

- 安全模型不是“加一个签名”，而是完整信任链：信封 → 签名清单 → journal 绑定 → 目标哈希 → worker 作用域；每层都有独立可验证约束。
- 可靠性不是“失败重试”，而是 write-ahead + 幂等重放 + 受信旧哈希 + 崩溃循环 + 可中断回滚；并且有真实子进程与故障注入证据。
- 数据模型已经跨过“只能相邻版本升级”的原型阶段：内容寻址 blob 让修复、跳版本、回滚共用一套素材。
- 工程化程度高：契约测试、穷尽错误表、docs-check、版本同步、发布前自验、诊断包。

**仍需警惕的部分**

- 文档是当前最大的“产品缺陷”：它写得足够好，以至于开发者会照抄，而其中若干具体事实已经过期。
- 默认权限与健康确认的闭环没有打通，是“按文档接入却跑不通”的真实断点。
- 纵深防御还差链接/reparse 一层；企业签名服务支持不完整。
- 大目录/大文件的性能与进度体验仍有明显优化空间。

**结论**：**作为 Windows 10+ 桌面、自托管发布通道的 1.0 候选，它已经具备发布条件，前提是先完成批次 0（文档与默认值）并补上 N-04/N-06/N-08。** 发布后建议把精力放在“让接入者 5 分钟成功”和“让故障可解释”，而不是继续扩展差分算法；差分已足够好，DX 与文档一致性才是当前最高的边际收益。
