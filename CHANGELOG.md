# Changelog

本项目遵循 [SemVer](https://semver.org/)。发布前必须同步更新本文件。

## [1.0.0-rc.1] - 2026-11-14

首个候选版本：公共 API 已冻结，`1.x` 只做兼容新增，breaking change 必须升 major。rc 阶段只接收修复；发现问题发 `rc.2`，不原地覆盖已推送的 tag。

### 接入

- 一行接入：`BuilderExt::run_with_delta_update(context)` 替换 `.run(context)`，在 GUI 启动前处理 worker 模式。
- `delta-update init` / `migrate` 生成配置、capability 与迁移清单，默认最小权限集 `manage`。
- 前端 `createUpdateController()` 与 `useUpdater()`：类型守卫（`isPrepared` / `isCommitted` / `isRolledBack` / `isFailed` / `isUpdateRequired`）与派生状态，接入方不再手写状态字符串比较。
- `update_required` / `UpdateRequired`：低于发布者最低支持版本时，应用可自行决定限制程度。

### 发布

- `release --pipeline`：生成 → 自测 → 上传 → 审计 → 报告，失败即停。
- `rollback --to-version`：复用已发布清单，只改变 `latest.json` 指向，不重建补丁。
- `report --history`：跨版本覆盖率趋势与回退文件。
- `--self-test`：在 `--from` 的临时副本上演练 apply + rollback。
- `release --min-supported`：把最低可接受版本写进签名清单。

### 诊断

- `inspect --bundle`：用户 journal × 远端 manifest × `worker.log` 三方对照。
- `doctor --watch`：发布后持续观察端点与对象。
- `export_diagnostics` / `open_diagnostics_folder`：应用内一键生成脱敏诊断包。
- `get_update_history`：本地读取 `completed` / `rolled_back` / `failed` 终态记录，插件不上报。

### 安全

- Journal 与签名清单绑定：worker 重新验签 `verified_manifest.json`，拒绝未绑定的重放。
- [信任模型](docs/security/trust-model.md)：逐文件信任边界与残余风险，结论为无 RCE 路径。
- `min_supported_version`：强制安全更新的协议基础。
- 无签名恢复收口到 `#[doc(hidden)] Engine::new_legacy`，生产路径不可达。

### 可靠性

- 崩溃循环分级：阈值 3 + 正常退出折扣（`exit_grace_period_secs`）。
- SIGKILL、并发 prepare、跨卷 copy 回退与多路径故障注入测试。
- 原子发布顺序、内容寻址 blob、断点复用与全量回退。

### 工具

- `key-info`、`clear_skipped_version`、`inspect` 退出码契约。
- `bench-check`、`contract-check`、`version-sync` 与 CI 模板。
- 发布 SBOM、`release-report.json`、`--watch` 观察窗口。

### 详细变更

#### 新增

- **事故回滚一键复用**：新增 `delta-update rollback --bucket <uri> --to-version <v> --key <key>`，读取并验签已发布的 `manifests/<v>.json`，加 `rollback: true` 后重新签名上传为 `latest.json`，不重建补丁、不改动版本化清单；`--dry-run` 只打印。与 `release --rollback` 的区别是“改变推荐版本”而非“发布更低版本”。
- **更新历史 API**：引擎在 `journal/update_history.jsonl` 追加终态记录（`completed` / `rolled_back` / `failed`，含 `duration_ms`、`from_version`、`reason` / `error_code`），超过 256 KiB 保留最后 200 行；新增 `Engine::update_history(limit)`、插件命令 `get_update_history`（`monitor`/`manage`/`default`）、`getUpdateHistory()` 与 `UpdateRecord` 类型。数据只留在本地，插件不上报。
- **强制安全更新**：清单新增可选 `min_supported_version`（SemVer，且 `<= version`）；`check` 返回 `update_required` 与 `min_supported_version`，插件新增稳定状态 `UpdateRequired`（payload 增加 `min_supported_version`），`useUpdater` 增加 `isUpdateRequired`；`release --min-supported` 负责写入。引擎不阻止应用运行，严格程度由应用决定。
- **发布后审计**：`delta-update inspect --from-release <bucket> --version <v> --public-key <pub>` 下载并验签版本化清单，输出与 `release --verbose` 同构的逐文件表格、`delta coverage` 汇总，并对每个 `delta_key`/`full_key` 做 `HEAD` 大小校验（不下载对象）；缺失或大小不符时以退出码 `2` 失败。
- **离线配置校验**：`delta-update doctor --check-config --dir <project>` 用引擎的配置 schema 校验 `plugins.delta-update`，不访问网络、不要求 `install_dir` 存在，可直接作为 CI 门禁。
- **稳定退出码**：`EngineError::exit_code()` 定义全命令一致契约（`1` 暂时性、`2` 需人工、`3` 内部错误），CLI `main` 透传，保留 `inspect` 既有 `0/1/2` 语义。
- **JSON 版本头**：所有 `--json` 输出顶层包含 `"v": 1` 与 `"tool": "delta-update-cli"`。
- **崩溃循环分级设计**：新增 [docs/design/crash-loop-grading.md](docs/design/crash-loop-grading.md)，区分 crash-on-start / healthy-start-unconfirmed / acknowledged-start，并给出“先调默认阈值、再看是否引入 `acknowledge_start`”的取舍。
- **诊断包一键定位**：新增 `open_diagnostics_folder` 命令（`manage`/`default`）与 `openDiagnosticsFolder()`，在资源管理器中打开 `app_log_dir()`；命令不接受路径参数，无法打开任意目录。
- **发布 SBOM**：`release` 额外上传 `manifests/<version>.sbom.json`，字段从已签名清单派生（路径、哈希、大小、策略、对象键），供企业/合规审计；客户端不读取。
- **一体化发布**：`release --pipeline` 依次执行生成、自测、上传、审计，成功后写出 `release-report.json`（`v`/`tool`/覆盖率/自测/审计）；任一步失败即停止，退出码沿用 `EngineError::exit_code()`。
- **CLI 契约**：新增 [docs/cli-contract.md](docs/cli-contract.md)（退出码表、`--json` 信封、发布报告 schema）与 `cargo xtask contract-check`（静态检查 JSON 信封与退出码表），并接入 `xtask ci`。
- **崩溃循环可观测**：启动时把阈值写入 `journal/crash_loop_threshold`，`inspect` 输出 `启动尝试 / 崩溃循环阈值`，并在下一次启动就会回滚时给出警告。
- **CI 模板**：`examples/ci/github-actions-release.yml` 与 `gitlab-ci.yml`，把 `release --pipeline` 与报告归档串成可复制的最小流水线。
- **`useUpdater` 重试**：新增 `retryPrepare()` / `retryRestart()`，按当前状态跳过无意义调用，接入方不再手写 `if/else`。
- **每文件补丁回退留痕**：`prepare` 在补丁失败回退全量时向 `worker.log` 追加 `patch fallback: <path>: <code>`；`worker.log` 的追加/裁剪下沉到 `delta_update_engine::worker_log`，worker 与 prepare 共用。
- **诊断包对照**：`inspect --bundle <bundle.json> [--from-release ...]` 输出“文件 × manifest 策略 × journal 操作 × 回退证据”对照表，汇总 `patched / fell back / full by design / removed`，支持 `--json`。
- **发布趋势**：`release --report <DIR>` 按 `<version>.json` 归档；`delta-update report --history <DIR>` 输出跨版本覆盖率趋势，覆盖率较上一版本下降超过 20 个百分点时列出新增回退文件。报告新增 `fallbacks` 字段（路径 + 原因）。
- **B5 退出路径验证**：新增 `lifecycle_contract` 用例覆盖“超过 grace 正常退出减一”“grace 内退出不减”“另一实例持锁时退出不阻塞且不折扣”。
- **性能回归门禁**：`cargo xtask bench-check [--threshold 0.10]` 读取 criterion 的 `change/estimates.json`，点估计与置信上界都超过阈值才失败；缺少基线时打印警告而不是静默通过。新增 `.github/workflows/bench.yml`（main 保存 `--save-baseline main`，PR 恢复后 `--baseline main` 对比）。
- **迁移脚手架**：`migrate --write` 只创建 `capabilities/delta-update.json` 与 `delta-update-migration.md`，已存在则跳过；`tauri.conf.json`、`Cargo.toml`、`main.rs` 保持原样。`init` 与 `migrate` 共用 `scaffold` 模块。
- **发布后观察窗口**：`doctor --watch <SECONDS> --interval <SECONDS>` 反复执行同一套检查，任一轮失败即失败；与 `--json` 互斥。
- **社区入场券**：新增 [SECURITY.md](SECURITY.md)（报告渠道、支持范围、已知信任边界）、[版本与兼容承诺](docs/reference/versioning.md)（crate SemVer、manifest schema 永久承诺、密钥轮换）与 `.github/pull_request_template.md`（兼容性检查清单）。
- **发布后观察串入流水线**：`release --pipeline --watch <SECONDS> --interval <SECONDS>` 在审计后持续重跑对象审计，任一缺失即失败；报告新增 `watch` 字段（秒数/间隔/轮次）。
- **信任模型文档**：[docs/security/trust-model.md](docs/security/trust-model.md) 固化攻击者能力、逐文件信任边界与残余风险（结论：无 RCE 路径，最坏是 DoS 或回滚到签名清单中存在过的旧字节）。
- **SIGKILL 恢复测试**：`worker_process` 用 `TerminateProcess` 在提交中途杀 worker，断言只留下 `apply started`、无 `commit.status`，下一次恢复完成剩余 ops。
- **并发 prepare 测试**：`staging_contract` 覆盖“第二个 prepare 复用暂存文件、不重复下载”与“prepare 持锁期间 check 仍能返回清单”。
- **GitHub Actions tag 触发**：`release.yml` 支持 `push: tags: ['v*']`，从 tag 提取版本并复用 `version-sync` 校验。
- **安全披露流程**：[SECURITY.md](SECURITY.md) 补充“确认 → 修复 → 私有 advisory → 公开 advisory → 补丁版本”的时间线与支持版本策略；新增 [安全公告](docs/security/advisories.md)（受影响版本 / 修复版本表格，当前为空）。
- **5 分钟首跑**：[开发者接入指南](docs/guides/getting-started.md) 新增“先看效果”一节，直接引用 `real-test/run.ps1`，说明依赖、脚本步骤与预期输出。
- **灰度发布设计**：新增 [docs/design/gray-release.md](docs/design/gray-release.md)，对比 channel 扩展、`client_id` 服务端路由与清单内 `percent` 三种方案；结论是近期用 channel、长期用服务端路由、不实现客户端自选比例。

#### 变更

- **文档交付入库**：`.gitignore` 不再整体忽略 `docs/design/` 与 `real-test/`。设计文档改为只忽略 `*.draft.md`；`real-test/` 只忽略 `work/`、`keys/` 与生成的 `demo-app/Cargo.lock`（`**/target` 已覆盖构建产物），脚本与演示源码随仓库提交，干净 checkout 中 `getting-started.md` 的引用不再 404。
- **README 选型对比**：新增“为什么选 delta-update”表，按更新单位、下载量、平台、签名模型、应用时机、回滚、失败恢复与维护成本对比官方 updater。
- **发布演练与 rc**：[release.md](docs/operations/release.md) 补充 `1.0.0-rc.1` 演练步骤与“tag 必须等于 workspace 版本”的约束；`release.yml` 在版本号含 `-` 时用 npm `next` tag 发布，并把 `wait_for_crate` 从 `cargo search` 改为 `cargo info <crate>@<version>`，确保能轮询到预发布版本；[versioning.md](docs/reference/versioning.md) 明确 `1.0` 的稳定范围（`EngineConfig`、`EngineState`、错误枚举、manifest schema、Journal format、`UpdateRecord`）。
- **rc 反馈入口**：README 顶部增加 rc 横幅；新增 `.github/ISSUE_TEMPLATE/rc-feedback.yml`，用三个具体问题收集接入卡点、`init`/`migrate` 可读性与 `worker.log` 异常。
- **公开 API 收口**：`EngineConfig` 删除 `allow_unsigned_journal_recovery`。无签名恢复只存在于 `#[doc(hidden)]` 的 `Engine::new_legacy(config, true)`，插件与 CLI 只走 `Engine::new`，因此生产路径不可能误开；`spawn_replacement` 的对应拒绝分支随之删除。
- **SECURITY.md 渠道收敛**：只保留 GitHub Security Advisories，移除占位邮箱。
- **崩溃循环默认放宽**：`crash_loop_threshold` 默认从 2 提到 3；新增 `exit_grace_period_secs`（默认 10 秒），更新已提交但未确认时，正常退出会把计数减一（`Engine::discount_clean_exit`），崩溃不会走退出钩子，因此仍正常累计。不引入 `acknowledge_start`，设计记录见 [crash-loop-grading.md](docs/design/crash-loop-grading.md)。
- **`release --verify` 覆盖 SBOM**：verify 阶段额外 `HEAD` 确认 `<version>.sbom.json` 存在。
- **`doctor --json` 模式字段**：`doctor` 与 `doctor --check-config` 的 JSON 都用 `mode`（`endpoints`/`config`）区分，公共字段形状一致。
- **配置 schema 下沉**：`PluginConfig` 与校验逻辑移入 `delta_update_engine::config`，插件 `Config` 变为 `pub use`，CLI 与插件零复制共享同一套校验。
- **一行接入**：新增 `BuilderExt::run_with_delta_update(context)`，替换 `.run(context)` 即可在 GUI 启动前处理 worker 模式，消除“GUI 正常启动但 worker 未执行”的顺序陷阱；底层 `run_applier_from_context` 保留。
- **发布前自测**：`release --self-test`（需 `--from`）在临时副本上执行 journal apply + inverse rollback，验证补丁能落到真实安装树、`.old` 备份与回滚能恢复原状。
- **迁移工具**：新增 `delta-update migrate --dir <PATH>`，读取 `plugins.updater` 的 endpoint/pubkey，打印 delta 配置、capability、Cargo.toml/main.rs 替换与手工清单；明确提示官方 `latest.json` **不能复用**。只打印，不写文件。
- **回滚逃生舱**：新增 `clear_skipped_version` 命令（`manage`/`default`）与 `clearSkippedVersion()`，发布者重发同一版本号后可解除 `skipped_version`。
- **前端类型守卫**：新增 `isCommitted`/`isFailed`/`isRolledBack`/`isBusy`；`useUpdater` 返回 `isChecking`/`isDownloading`/`isReady`/`canPrepare`/`canRestart`/`error` 等派生值。
- **接入默认值更安全**：`init` 的 `--permission-set` 默认改为 `manage`（不含 `confirm_healthy`）；`default` 标记为兼容集，需要前端确认健康时显式指定。`init` 输出开头新增“下一步（3 项）”，先讲 worker 入口、公钥与 HTTPS，再给 JSON 片段。
- **`key-info` 子命令**：`delta-update key-info --key release.key` 直接输出 base64 公钥与 key ID，不再依赖外部 minisign CLI 或手工解析 `.pub`。
- **发布可观测性**：`release` 默认输出新增 `delta coverage: N/M modified files patched (...)`，按 `too large`/`ratio`/`compressed` 拆分回退原因；patch 并行默认降为 `min(核数/2, 4)` 并打印 `patch jobs` 与预估峰值内存，`--jobs` 可覆盖。
- **worker 启动留痕**：`apply`/`recovery` 在进入任何可能失败的逻辑前先写 `apply started ...` / `recovery started ...`，进程级 abort 后诊断包仍能看到 worker 启动过。
- **错误可行动性**：`NetworkError` 的 `user_action` 按 DNS/TLS/proxy/timeout/4xx/5xx/限流/签名过期/签名服务分别给出建议，不再统一落到“检查网络”。
- **开发期告警**：debug 构建启用 `auto_check` 时输出警告（会每次启动访问 endpoint），与既有 `auto_apply` 警告对齐。
- **清理可观测**：`confirm_healthy` 清理 `.old`/`.trash` 失败时输出 `tracing::warn`，仍不阻塞确认。
- **下载短路**：Range 下载在 `chunk_count < 2` 时直接回退单流，避免 9–16 MiB 文件走“无并行收益 + 目录合并开销”的路径。
- **`doctor` 提示**：presigned endpoint 被拒绝时明确指向 `delta-update verify --bucket <uri>`。
- **Windows-only 编译期约束**：插件在非 Windows 目标直接 `compile_error!`；发布任务改到 `windows-latest`，并把 `sleep 60` 换成轮询 `cargo search` 直到新版本可见。
- **前端控制器**：`controller.stop()` 在最后一个引用释放时清空快照，`getState()` 返回 `null`，避免后续订阅者渲染已停止监听的旧状态。
- **测试与文档**：`replay` 新增注入 `CrossesDevices` 的单测，确保 copy 回退在任何卷上都被执行；`worker_process` 断言 `retries <= 3`；`doctor` 新增 presigned 拒绝用例；前端新增“停止后快照为空”的用例；安全文档明确 `commit.meta.json`/`old_hashes.json` 的信任边界；示例用命名常量并说明“按初始化完成确认，而不是定时器”。

- **平台范围收敛为 Windows 桌面**：插件在非 Windows 平台进入 Disabled/Failed 并返回 `E_UNSUPPORTED_PLATFORM`，主应用仍可启动；移除 Linux/macOS 文件级替换与 `E_UNSUPPORTED_FORMAT` 的错误承诺，文档、CI、示例统一为 Windows 10+。
- **发布端改为内容寻址 blob**：CLI 为所有条目（含 `Unchanged`）生成 `full_key = blobs/<sha256>`，相同内容只上传一份；客户端可在跨版本升级、本地文件损坏/缺失时直接修复，不再只支持 N-1 → N。
- **运行端未变化文件修复**：`prepare` 发现本地文件与清单不一致时，不再直接失败，而是用 `full_key` 全量修复；补丁源不可用时仍安全回退。
- **安全默认值**：`auto_apply` 与 `auto_confirm` 默认改为 `false`；配置/平台错误降级为 Disabled，不再阻止宿主应用启动；`init` 不再输出非法占位公钥，必须提供真实 `--public-key`。
- **安装根收紧**：worker 要求 `install_root` 精确等于自身可执行文件目录；插件生产构建拒绝更宽的 `install_dir`；清单/安装路径拒绝 `.old`/`.trash`（大小写不敏感）。
- **Journal 签名绑定**：`begin_commit` 写 `commit.meta.json`，worker 通过新增的 `run_applier_from_context()` 从内嵌配置读取公钥，验签 `verified_manifest.json` 并校验每个 `Place` 的目标哈希/大小来自签名清单；回滚 `Restore` 用 `installed_manifest` 或 `old_hashes` 校验。旧 `run_applier_from_args()` 会失败关闭，`run_applier_from_context()` 在 worker 模式下缺少公钥时直接失败退出，不会退化为不验签重放。
- **验签默认失败关闭**：`verify_commit_journal` 在无有效公钥时拒绝执行；只有引擎配置显式设置 `allow_unsigned_journal_recovery: true`（影响 `Engine` 提交/恢复路径，仅测试/迁移）才跳过并输出警告；worker 入口始终失败关闭，Tauri 插件为 `false`，且 `spawn_replacement` 会拒绝该 flag 与 worker 的组合。
- **恢复超时错误**：提权恢复等待超时改为 `E_APPLY_RECOVERY_TIMEOUT`，`action` 为 `restart`，明确引导用户重启应用让下一次启动幂等收敛。
- **权限集拆分**：新增 `delta-update:monitor`（仅 `check`/`status`）与 `delta-update:manage`（管理命令，不含 `confirm_healthy`）；`default` 保持全部权限以兼容。
- **验证与执行零窗口**：`apply_commit_verified` 验签后直接执行内存中的 `Journal`，不再按路径重读；`Backup.dst` 必须精确为 `.old/<commit_id>/<src>`，`Trash.trash_dst` 必须等于 `src`。
- **提交点完整性**：`begin_commit` 在缺少 `verified_manifest.json` 时返回 `E_NOT_PREPARED`（引导重新 `check()`），不再生成 worker 必然拒绝的 pending journal；执行前还校验 journal 与 `apply.json` 的 `version` 一致。
- **回滚备份校验**：`begin_commit` 记录 `old_hashes.json`；inverse `Restore` 用旧哈希快照校验备份，损坏备份返回 `E_JOURNAL_REPLAY_FAILED`，不再“自证正确”。旧文件元数据读取失败（非 NotFound）会中止提交，而不是静默放弃该文件的哈希保护。
- **下载韧性**：同 size 但哈希错误的文件先删除再重下，避免 Range EOF 416；Range 下载遇到预签名 403 会回退单流并刷新 URL；HTTP 重定向逐跳校验 scheme/host/allowed_hosts。
- **CLI 可靠性**：对象上传增加 3 次退避重试，S3/COS 总超时从 300s 提升到 1800s；CLI 初始化 tracing-subscriber；worker 失败写入 `journal/worker.log`。
- **可观测性**：`inspect` 修正 Trash/RemoveTrash 的 commit_id 路径，并在诊断中包含 rollback journal；补丁应用强制执行 `patch_memory_factor` 预算；不支持的已知补丁策略会输出警告并回退全量；`worker.log` 记录 `commit_id` 与日志路径。
- **补丁 I/O**：源文件只读取一次，并在应用前就地校验其哈希，去掉 `apply_patch` 之前的整文件预读，同时消除“先校验、后读取”之间源文件被替换的窗口。
- **进度单位显式化**：状态事件的 `progress` 新增 `unit`（`bytes`/`files`/`ops`），前端不再需要按状态猜测 `completed` 的含义；旧客户端可忽略该字段。
- **接入与 CLI 提示**：`init` 明确配置合并位置并改用 `run_applier_from_context`；`release`/`verify --help` 列出 COS/S3 所需环境变量；`--patch-max-file-size` 的内存说明改为实际组成而非未经验证的固定倍数。
- **配置告警**：release 构建中启用 `allow_insecure_http` 会输出显眼的 `tracing::warn`（该开关本身仍只允许 loopback）；debug 下 `install_dir` 与 exe 目录不一致时会提示 worker 无法替换文件。
- **分块下载可调**：`FetchLimits` 新增 `range_chunk_threshold`、`range_chunk_size`、`range_concurrency`，默认值不变，引擎级用户可按网络环境调整。
- **`init --write`**：在 `src-tauri/capabilities/delta-update.json` 生成独立 capability（identifier 为 `delta-update`，不覆盖已有文件），但仍不改写 `tauri.conf.json`。
- **`init --permission-set`**：可选 `default`/`manage`/`monitor`，生产环境可直接生成最小权限集；输出附带 minisign 密钥生成命令，默认集时提示改用更窄的权限；检测到官方 updater 配置时提示移除旧依赖与权限。
- **诊断包**：`inspect --export <PATH>` 输出单文件 JSON，包含报告、`worker.log` 尾部 64 KiB、journal 目录文件清单、系统信息（Windows 版本、安装形态）与工具版本，便于用户直接附到工单；诊断逻辑下沉到 `delta_update_engine::diagnostics`，插件新增 `export_diagnostics` 命令（`exportDiagnostics()`）让应用自身生成同样的诊断包，路径由插件决定、前端不可指定。
- **提交审计**：`worker.log` 在成功时写 `apply commit <id> ok in <ms> (version <v>, retries <n>)` / `recovery ok in <ms> (retries <n>)`，失败时写 `... failed after <ms> (retries <n>): ...`（错误消息中的换行替换为空格，保证一条日志一行）；`n` 是本次提交/恢复中文件占用重试的次数（成功与失败都记录，进程进入对应模式时清零）；文件超过 1 MiB 时自动裁剪为最近 256 KiB，避免持续失败导致无限增长。
- **云占位文件跳过**：`scan_local` 通过 `MetadataExt::file_attributes()` 识别 OneDrive/Files On-Demand 与离线占位文件（`RECALL_ON_DATA_ACCESS` / `OFFLINE`；不把 `RECALL_ON_OPEN` 当作占位，避免已水合文件误判），跳过哈希并输出 warning，避免 `check` 触发整树水合卡顿；这些文件不进入 `modified`/`removed`，若清单仍包含则按缺失文件从全量 blob 修复。`CheckOutcome`/`CheckResult` 新增 `cloud_placeholders_skipped` 供 UI 解释下载量。
- **诊断包脱敏**：`inspect --export --redact` 与插件的 `export_diagnostics`（始终脱敏）把 `install_root`、`staging_root`、`journal_root`、用户主目录前缀替换为占位符，并在 `redacted` 字段注明。
- **重试策略统一**：移至 `fs::retry_file_in_use`，固定 200/400/800 ms 三次；覆盖同卷 rename/remove、跨卷 copy/rename/remove 与 `remove_dir_all_best_effort`，消除同卷与跨卷的耐心差异。清理路径单独识别 `ERROR_DIR_NOT_EMPTY`（145）——杀软短暂持有已移动文件时 Windows 会报“目录非空”，仅重试 32/33 会漏掉该场景（本轮由 `lifecycle_contract` 回归测试捕获）。
- **磁盘预检**：`preflight` 除 staging 卷外也检查安装根卷的剩余空间，覆盖 `staging_dir` 跨卷配置；`.old`/`.trash` 为同卷 rename，不重复计入。
- **Journal 格式前向兼容**：读到比客户端更新的 `format` 时返回 `E_JOURNAL_FORMAT_TOO_NEW`（`action = update_client`），引导升级客户端而不是报告“损坏”。
- **网络错误分类**：连接失败会区分 DNS、TLS 握手与代理前缀，弱网排障不再只有 `E_NETWORK_CONNECT`。
- **发布端时间偏差提示**：COS/S3 上传返回 403 且响应体含 `RequestTimeTooSkewed`/`Request has expired` 时，明确提示检查发布机 NTP/系统时间；HEAD 响应缺少 `Content-Length` 时报错而不是当作 0。
- **元数据与文档**：Cargo/npm 包描述统一标注 Windows-only；README 首屏新增“何时使用/与官方 updater 的边界”对照与 Windows 环境矩阵入口；补充密钥轮换 runbook、轮换期间的 `E_SIGNATURE_NO_TRUSTED_KEY` 预期、`fetch_signature` 403 排查、`.old`/`.trash` 集中备份路径语义、`verify --sample 0` 跳过下载、旧客户端遇到更高格式 Journal 与 `apply.json` 损坏的处置；示例补“本地跑通完整链路”步骤。
- **测试**：新增 `future_journal_format_requires_a_client_update`，证明更高 Journal 格式经 `recover_and_cleanup` 返回 `NeedsAttention` 且码值为 `E_JOURNAL_FORMAT_TOO_NEW`（`action = update_client`），而不是 `E_JOURNAL_CORRUPT`；新增 `parent_wait_timeout_reports_a_helper_failure`、`export_writes_a_support_bundle_with_the_worker_log_tail`、`sharing_violations_name_the_likely_cause`。
- **e2e 走生产路径**：e2e harness 改用 `apply_commit_verified`，对象布局改为 `blobs/<sha256>`（含未变化条目），并断言 `commit.meta.json`/`old_hashes.json` 已清理。
- 项目更名为 **Delta Update Engine（增量更新内核）**：仓库与根包标识统一为 `delta-update-engine`；核心 crate 名称不变，插件 `tauri-plugin-delta-update`、CLI `delta-update-cli` 与前端包 `tauri-plugin-delta-update-api` 保持不变。README、LICENSE、Cargo 元数据与全部文档同步更新。
- 文档校准：目录结构补上 `hash_cache`、`recovery`、Journal plan/state、50k 编解码基准与源码导出脚本；修正 `patch/apply` 被误写为“流式应用”的描述。
- 发布端：新增 S3 兼容发布（AWS S3 / Cloudflare R2 / MinIO，SigV4 path-style），`--bucket s3://bucket`；凭证支持 `S3_ACCESS_KEY_ID` / `S3_SECRET_ACCESS_KEY` / `S3_REGION` / `S3_ENDPOINT`，并兼容 `AWS_*` 变量。
- 离线诊断：新增 `delta-update inspect --journal-dir <DIR> [--json]`，直接解码 `apply.json`、`commit.status`、Journal、PREPARED、attempts 与 skipped_version，输出可读结论；损坏文件列为警告而不是崩溃。
- 弱网处理：HTTP 429 按可重试处理并尊重 `Retry-After`（上限 60 秒），清单、签名与对象下载共用该策略。
- 进度透明度：`ProgressPayload` 新增可选 `file`，前端可在下载/校验阶段显示当前文件名；`progress` 类型同步为可选字段。
- 大文件策略：CLI 新增 `--patch-max-file-size`（默认 64 MiB），超过阈值的文件直接全量，避免 qbsdiff 约 17 倍输入大小的内存峰值；文件内分块差分暂不实现，已在文档中标注边界。
- 断点复用：`prepare` 先校验已存在的暂存目标，哈希匹配直接复用；已下载的 patch 在大小匹配时复用。取消、崩溃或重复调用 prepare 不再重复下载同一文件。
- 离线诊断增强：`inspect --verbose` 逐条列出 `FileOp` 及 `landed` / `pending` / `backed_up` 状态，便于判断替换进度与备份完整性。
- 下载加速：≥ 8 MiB 的对象按 8 MiB 分块、最多 4 路并发 Range 下载，合并后统一校验；服务端忽略 Range、分块异常或尺寸不符时自动回退单流，不改变最终哈希校验。
- 接入 CLI：新增 `delta-update init`，按 `--endpoint` / `--public-key` 生成可直接合并的 `plugins.delta-update` 配置与 capability 片段，并列出接入检查项。
- 大对象重试：单个 Range 分块失败时按指数退避重试（尊重 `Retry-After`），不再因一个分块的瞬时 502 丢弃全部已下载分块；合并改为写临时文件后原子 rename，崩溃不会留下半截目标文件。
- `inspect` 门禁：存在未完成/回滚 Journal 时返回退出码 `1`（重启可恢复），状态损坏返回 `2`（需人工介入），干净状态返回 `0`。
- 接入迁移：`delta-update init --dir` 会读取现有 `src-tauri/tauri.conf.json`，复用 `plugins.delta-update` 的 endpoint/公钥，或从官方 updater 的 `pubkey` 迁移公钥。
- 下载清理：每次 `download` 前清理过期的 `.chunks` 目录与 `.merging` 临时文件，避免崩溃残留撑爆缓存。
- 修复：自定义 `headers` 现在只发送到清单与签名请求，不再泄露到第三方对象下载（此前实现与文档相反）。
- 测试：新增 `crates/delta-update-testkit/tests/worker_process.rs`，用真实 worker 二进制跨进程验证签名绑定的提交、启动恢复、`install_root` 精确匹配与错误密钥拒绝；新增自定义 Range 分块参数测试。

## [0.1.0] - 初始开发版

### Added

- `Engine::check`：拉取清单与 detached 签名、验签、清单校验、通道/版本守卫、本地差异计算，并缓存验签快照。
- `Engine::prepare`：只物化变化文件、未变化文件原地校验、qbsdiff+zstd 补丁应用、全量回退、取消、磁盘与暂存上限预检、`prepared.json` 持久化。
- `Engine::begin_commit`：TDJ1 二进制 Journal（尾部 SHA-256、5 类操作）、`apply.json`、`update.lock`，提交点前不触碰根目录。
- 替换进程执行器：幂等向前重放、跨卷复制+原子 rename、执行前后哈希校验、`commit.status`。
- 启动恢复：重放未完成 Journal、崩溃循环检测、逆日志回滚、`skipped_version`、健康确认清理。
- CLI 发布管线：`release` 扫描/差异计划/qbsdiff+zstd `.tdp`/60% 降级/清单构建/minisign 签名/原子发布；`verify` 回读验签并抽样应用补丁；`verify-key` 校验密钥配对；`doctor` 检查多端点版本与对象可访问性。
- 发布端：COS REST v5 签名（固定文档向量测试）与本地目录发布；`--prefix` 贯穿清单与对象键；`--dry-run`、`--require-delta`、`--jobs`。
- Tauri 插件生命周期：六个命令接入引擎，`auto_check` 后台 check+prepare、退出时 `auto_apply`、主窗口就绪后 `auto_confirm`，以及启动恢复状态映射与进度事件。
- 替换进程：`--delta-update-apply <journal_dir> <commit_id> [--relaunch]` 参数入口，等待父进程退出后执行 Journal；`restart_to_apply` 通过 `--relaunch` 重新拉起新版本；Windows 下无窗口启动，并校验 `install_root` 必须精确等于 worker 自身所在目录。
- 提权提交：安装根不可写时（典型是 `Program Files`）先探测写权限，再通过 `ShellExecuteW("runas")` 启动替换进程，UAC 提示出现在应用退出后；取消授权会撤销提交点（删除 Journal/apply.json）并保留 PREPARED，下次启动复用重试；提权 worker 完成后经 Explorer 降权拉起新版本。
- 启动期提权恢复：per-machine 安装在上次提交/回滚中断后，下一次启动先判断恢复是否需要写安装根；需要且不可写时通过 `ShellExecuteExW("runas")` 同步运行同一二进制的 `--delta-update-recover <journal_dir> <crash_loop_threshold>` 模式并等待结果，再以 `recover_after_elevation` 读取状态。正常的“已提交待确认”启动不弹 UAC；取消授权保留 Journal 与回滚素材并进入 `Failed`，下次启动重试。提权 worker 与父进程共享同一次启动计数，不会重复累加。
- 恢复核心重构为 `recovery` 模块：`Engine::recover_and_cleanup`、`Engine::recovery_needs_install_write`、`Engine::recover_after_elevation` 与 worker 侧 `run_recovery` 共用同一份重放/回滚/复用逻辑。
- 进度修复：`Downloading` 改为累计字节（`total` 为本次下载总量，`completed` 单调递增），`Preparing` 按已暂存文件计数；插件以 100ms 窗口合并事件，避免大文件集 IPC 风暴。
- 启动回滚可见：`StartupOutcome::RolledBack` 现在映射为 `EngineState::RolledBack` 并广播，前端可提示“已回滚到 vX”。
- 接入防呆：worker 模式参数到达 `setup` 时返回 `E_CONFIG_INVALID`，提示必须在 `main` 开头调用 `run_applier_from_context(&context)`，不再静默开第二个实例。
- 定时检查：新增 `check_interval_secs`（可选，最小 60 秒），空闲或失败状态下周期执行 check+prepare。
- 发布说明与强制更新：清单新增可选 `notes`（最大 64 KiB）与 `mandatory`，CLI 新增 `--notes-file` / `--mandatory`，`check` 结果透传到前端。
- 前端 DX：`tauri-plugin-delta-update-api` 新增 `createUpdateController()`（快照 + 订阅 + 六个命令）与可选 React 18+ 子路径 `tauri-plugin-delta-update-api/react` 的 `useUpdater()`；`ConfirmResult` 与返回类型更精确；示例改用控制器并延迟健康确认。
- 控制器时序：`start()` 改为“先订阅事件、再读取 `status` 快照”，避免启动瞬间的事件被旧快照覆盖；`start()`/`stop()` 按引用计数管理监听，多组件共享同一 controller 时不会被先卸载的组件提前断开；`useUpdater()` 启动失败不再产生未处理的 Promise rejection。
- 事件载荷：Downloading/Preparing/Committing/Failed 状态的 `version` 现在携带本次目标版本，UI 可在下载/准备阶段直接展示“正在更新到 vX”；`status` 返回同样字段。
- 文档修正：React 示例在 `Prepared` 状态改用 `restart()`，并说明 `confirm()` 仅用于重启后的健康确认。
- 企业网络：插件新增 `proxy` 与 `headers` 配置，支持 HTTP/SOCKS 代理和清单/签名请求的鉴权头（不会发送到第三方对象主机）；`ErrorPayload` 新增稳定的 `ErrorAction` 动作码，便于 UI 本地化。
- 检查性能：新增 `journal_dir/hash_cache.json`，按 `size + mtime` 复用安装根文件哈希，大目录的重复 check 只重新哈希变化文件；更新确认后自动失效，缓存损坏或缺失时自动回退全量哈希；`prepare` 仍对未变化文件逐个重哈希，缓存不会绕过内容校验。
- 异步运行时保护：`check` 的安装树扫描、`prepare` 的未变化文件重哈希、暂存文件哈希与 PREPARED 全量校验全部移到阻塞线程池，大目录/大文件更新不再占住异步运行时。
- 暂存 I/O 去重：全量下载后不再重复整文件哈希（下载校验已覆盖）；补丁只读取 82 字节头部再交给 `apply_patch`，避免为大补丁额外加载整包。
- 跨进程一致性：`Engine::prepare` 在暂存与 PREPARED 持久化期间持有 `update.lock`，与 commit/recover/rollback 一致；第二个实例的 prepare 立即返回 `E_UPDATE_LOCKED`。
- 手动更新流程：`check` 成功后 120 秒内调用 `prepare` 直接复用验签结果，不再重复请求清单与签名。
- 定时检查：使用 `MissedTickBehavior::Skip`，系统休眠唤醒后不会连续触发多次检查。
- 类型体验：`isPrepared()` 改为 TypeScript 类型谓词，收窄后 `version` 为非空字符串。
- 错误动作：清单解析/过大/版本号非法映射为 `contact_support`，只有 schema 过新才建议 `update_client`；版本回退拒绝建议 `check_update`，避免 UI 给出错误指引。
- 订阅韧性：`createUpdateController` 隔离抛出异常的 listener，一个组件报错不影响其他订阅者；`subscribe` 与事件广播使用同一套保护。
- 类型契约：`PrepareResult.state` 修正为 `UpdateStateName`——并发 prepare 时返回的是当前进度状态名，而不是永远 `"Prepared"`。
- 定时恢复：`RolledBack` 状态下定时检查也会重试，长驻应用可在发布修复版本后自动恢复；后台检查失败不会覆盖 `RolledBack` 提示。
- 本地开发：debug 构建且 `auto_apply: true` 时输出警告，避免退出时把 `target/debug` 的开发二进制替换成发布版；接入指南新增本地开发注意事项。
- 健康确认解耦：`confirm_healthy` 在没有待清理的已完成提交时不再获取 `update.lock`，命令层也不再等待命令锁。长时间 prepare 持锁期间，自动健康确认仍能重置启动计数，避免“每次启动都在下载、退出时未确认”被误判为崩溃循环。
- 错误文案：`E_UPDATE_LOCKED`、`E_INVALID_TRANSITION`、`E_CANCELLED`、`E_NOT_PREPARED` 使用针对性的 `user_action`，不再落到“收集日志”“检查安全软件”这类泛化建议。
- 路径配置错误可直接定位：`install_root` / `staging_root` / `journal_root` / `install_dir` 的绝对路径与“不得位于安装目录内”错误现在会打印实际路径值。
- Journal 容量：上限提升到 32 MiB / 100 000 个操作，与默认 `max_files = 20000` 的最坏情况对齐，避免“全部替换 + 大量删除”在提交点触发 `E_JOURNAL_CORRUPT`；新增 50 000 op 的编解码基准。
- 文档：协议状态机与端到端调用链改用 Mermaid；接入指南说明替换进程是同一 exe 的短生命周期 worker；前端包补充 Vue 3 / Svelte 用法；README 明确 engine 可独立于 Tauri 使用。
- 取消语义：`cancel_prepare` 不再抢先广播 `Cancelled`，由正在结束的 prepare 统一发布，避免“取消动作与下载完成同时发生”时出现事件与实际状态不一致。
- 文档修正：故障排查不再声称插件自动写日志，补充 `tracing-subscriber` 初始化与 `tauri-plugin-log` 桥接方法；澄清 `Checking` 是瞬态、检查结束后的回流是合法状态。
- 发布元数据：engine/plugin/cli 补充 `readme`、`keywords`、`categories`、`documentation`；testkit 开发依赖改为 path-only，避免要求发布测试夹具 crate；npm 包补充 `keywords` 与 `sideEffects`。
- 性能基线：新增 `journal/verify` criterion 基准（100 个 Journal 操作重放、1000 个暂存文件校验），并为所有 bench 声明 `harness = false`。
- 文档：补充 per-machine 启动恢复、Authenticode 签名、NSIS perUser/perMachine 手动验收清单、crates.io/npm 发布顺序与 `key_rotation` 当前边界。
- `HttpFetcher::new_default`：统一 10s 连接超时、30s 单次读取超时与 User-Agent，避免大对象被总超时中断。
- 网络重试：连接/超时/5xx 指数退避，最多 3 次；4xx 立即失败。
- 测试夹具：真实 HTTP `MockServer`（Range 开关、断流注入、403/5xx、请求与 Range 记录）与运行时 minisign 测试密钥。
- 端到端场景：`e2e` 覆盖首次安装、增量更新、无更新零下载、5xx 重试、崩溃循环回滚、UAC 取消恢复、跨卷替换与真实 CLI 回滚发布；断言增量成功时全量对象未被请求。
- 清理韧性：`confirm_healthy` 与回滚删除 staging/备份目录时对 Windows 瞬时占用重试，避免目录永久残留。
- 恢复语义修复：确认流程删掉 Journal 后中断，下次启动仍读取 `commit.status=completed` 并报告 `Committed`，不再误复用已消费的 PREPARED 计划。
- 故障注入测试：Journal J-01..J-12（部分提交、状态丢失、重复重放、外部篡改、失败重试）与 Committer C-01..C-07/R-09/R-10（替换运行中镜像、残留备份、trash 创建、并发锁、确认后不回滚、阈值 1，以及 `FaultyFs` 注入的磁盘满、权限拒绝与落盘损坏）。
- 提交失败恢复：`commit.status=failed` 在下次启动先幂等重放，临时性失败可直接续完；仍失败则按逆日志回滚到旧版本并记录 `skipped_version`，不再让安装停留在混合状态。
- 文件移动仅在跨卷（`ErrorKind::CrossesDevices`）时回退到复制；权限或磁盘错误立即失败，避免无谓的大文件复制并暴露真实原因。
- 测试稳定性：修正 `cancelled_prepare_keeps_staged_files` 对并发下载顺序的错误假设，改为以取消时已完成的对象做断言。
- 网络与暂存韧性测试：F-01 断流续传、F-02/F-03 预签名刷新、F-04 Range 不支持回退全量、F-05 有效头但载荷错误回退全量、F-06 重试有界、F-08 4xx 不重试、F-09 并发上限、F-10 读超时；S-04 补丁与全量双双失败、S-05 下载前磁盘空间预检（超大声明体积触发真实可用空间不足）、S-07 PREPARED 跨重启复用、S-08 暂存区篡改阻止提交。
- 预签名 URL：`signing_endpoint` 按协议 §9.5 的 POST 契约换取清单/签名/对象下载 URL，缓存并在到期前 60 秒刷新；403 时刷新后从当前偏移续传，连续 3 次仍失败返回 `E_NETWORK_PRESIGN_EXPIRED`。
- prepare 并发下载：按 `max_concurrent_downloads`（默认 4）限制并发，保持暂存结果排序、取消与失败即停语义；F-09 用延迟响应断言并发峰值不超限。
- 签名错误细分：受信 key ID 匹配但清单被篡改返回 `E_SIGNATURE_INVALID`；签名对象 404 返回 `E_SIGNATURE_SIG_MISSING`，不再一律归为无受信密钥。
- 安全测试：SEC-01/02/03 清单字段篡改、SEC-08 签名缺失、SEC-10 `key_rotation` 不扩大信任集、SEC-14 Journal 路径逃逸、SEC-16 多受信密钥、SEC-17 暂存源篡改重放失败、SEC-18 超大清单、SEC-19 解压上限。
- 生命周期边界：`cancel_prepare` 取消后保留已完成暂存文件，下次 prepare 直接复用；缺少 PREPARED 时 `begin_commit`/`resume_prepared` 返回 `E_NOT_PREPARED`；新增默认权限覆盖六个命令的契约测试。
- 插件生命周期测试：`tests/lifecycle_contract.rs` 用 Tauri mock runtime 覆盖 P-01/P-02/P-03/P-04/P-06/P-11/P-14，包括启动期 Journal 恢复、崩溃循环回滚、`auto_check` 后台 prepare 与 `Prepared` 事件、`auto_confirm` 窗口就绪延迟确认、`confirm_healthy` 的备份清理、退出时提交 Journal flush 以及无 ACL 时命令被拒；同文件覆盖 SEC-12（未知命令被运行时拒绝）与 SEC-13（额外 IPC 参数被忽略）；Windows 测试目标补充 comctl32 v6 清单依赖以便加载 mock runtime。
- 平台范围：只交付 Windows 桌面端，移除移动端占位模块；非桌面平台仍在注册期返回 `E_UNSUPPORTED_PLATFORM`，不会静默失败。
- 工程骨架：workspace、引擎/插件/CLI/testkit crate、前端包、示例与端到端测试目录。
- 目录结构规范、协议、架构与测试文档。
- 国内镜像配置与基础 CI 门禁。
