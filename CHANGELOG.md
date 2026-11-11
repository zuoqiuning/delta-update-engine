# Changelog

本项目遵循 [SemVer](https://semver.org/)。发布前必须同步更新本文件。

## [Unreleased]

### Changed

- 项目更名为 **Delta Update Engine（增量更新内核）**：仓库与根包标识统一为 `delta-update-engine`；核心 crate 名称不变，插件 `tauri-plugin-delta-update`、CLI `delta-update-cli` 与前端包 `tauri-plugin-delta-update-api` 保持不变。README、LICENSE、Cargo 元数据与全部文档同步更新。
- 文档校准：目录结构补上 `hash_cache`、`recovery`、Journal plan/state、50k 编解码基准与源码导出脚本；修正 `patch/apply` 被误写为“流式应用”的描述。
- 发布端：新增 S3 兼容发布（AWS S3 / Cloudflare R2 / MinIO，SigV4 path-style），`--bucket s3://bucket`；凭证支持 `S3_ACCESS_KEY_ID` / `S3_SECRET_ACCESS_KEY` / `S3_REGION` / `S3_ENDPOINT`，并兼容 `AWS_*` 变量。
- 离线诊断：新增 `delta-update inspect --journal-dir <DIR> [--json]`，直接解码 `apply.json`、`commit.status`、Journal、PREPARED、attempts 与 skipped_version，输出可读结论；损坏文件列为警告而不是崩溃。
- 弱网处理：HTTP 429 按可重试处理并尊重 `Retry-After`（上限 60 秒），清单、签名与对象下载共用该策略。
- 进度透明度：`ProgressPayload` 新增可选 `file`，前端可在下载/校验阶段显示当前文件名；`progress` 类型同步为可选字段。
- 大文件策略：CLI 新增 `--patch-max-file-size`（默认 64 MiB），超过阈值的文件直接全量，避免 qbsdiff 约 17 倍输入大小的内存峰值；文件内分块差分暂不实现，已在文档中标注边界。
- 下载加速：≥ 8 MiB 的对象自动切成最多 4 个 Range 分块并发下载，合并后统一校验；服务端忽略 Range、分块异常或尺寸不符时自动回退单流，不改变最终哈希校验。
- 修复：自定义 `headers` 现在只发送到清单与签名请求，不再泄露到第三方对象下载（此前实现与文档相反）。

### Added

- `Engine::check`：拉取清单与 detached 签名、验签、清单校验、通道/版本守卫、本地差异计算，并缓存验签快照。
- `Engine::prepare`：只物化变化文件、未变化文件原地校验、qbsdiff+zstd 补丁应用、全量回退、取消、磁盘与暂存上限预检、`prepared.json` 持久化。
- `Engine::begin_commit`：TDJ1 二进制 Journal（尾部 SHA-256、5 类操作）、`apply.json`、`update.lock`，提交点前不触碰根目录。
- 替换进程执行器：幂等向前重放、跨卷复制+原子 rename、执行前后哈希校验、`commit.status`。
- 启动恢复：重放未完成 Journal、崩溃循环检测、逆日志回滚、`skipped_version`、健康确认清理。
- CLI 发布管线：`release` 扫描/差异计划/qbsdiff+zstd `.tdp`/60% 降级/清单构建/minisign 签名/原子发布；`verify` 回读验签并抽样应用补丁；`verify-key` 校验密钥配对；`doctor` 检查多端点版本与对象可访问性。
- 发布端：COS REST v5 签名（固定文档向量测试）与本地目录发布；`--prefix` 贯穿清单与对象键；`--dry-run`、`--require-delta`、`--jobs`。
- Tauri 插件生命周期：六个命令接入引擎，`auto_check` 后台 check+prepare、退出时 `auto_apply`、主窗口就绪后 `auto_confirm`，以及启动恢复状态映射与进度事件。
- 替换进程：`--delta-update-apply <journal_dir> <commit_id> [--relaunch]` 参数入口，等待父进程退出后执行 Journal；`restart_to_apply` 通过 `--relaunch` 重新拉起新版本；Windows 下无窗口启动，并校验 `install_root` 必须包含 worker 自身。
- 提权提交：安装根不可写时（典型是 `Program Files`）先探测写权限，再通过 `ShellExecuteW("runas")` 启动替换进程，UAC 提示出现在应用退出后；取消授权会撤销提交点（删除 Journal/apply.json）并保留 PREPARED，下次启动复用重试；提权 worker 完成后经 Explorer 降权拉起新版本。
- 启动期提权恢复：per-machine 安装在上次提交/回滚中断后，下一次启动先判断恢复是否需要写安装根；需要且不可写时通过 `ShellExecuteExW("runas")` 同步运行同一二进制的 `--delta-update-recover <journal_dir> <crash_loop_threshold>` 模式并等待结果，再以 `recover_after_elevation` 读取状态。正常的“已提交待确认”启动不弹 UAC；取消授权保留 Journal 与回滚素材并进入 `Failed`，下次启动重试。提权 worker 与父进程共享同一次启动计数，不会重复累加。
- 恢复核心重构为 `recovery` 模块：`Engine::recover_and_cleanup`、`Engine::recovery_needs_install_write`、`Engine::recover_after_elevation` 与 worker 侧 `run_recovery` 共用同一份重放/回滚/复用逻辑。
- 进度修复：`Downloading` 改为累计字节（`total` 为本次下载总量，`completed` 单调递增），`Preparing` 按已暂存文件计数；插件以 100ms 窗口合并事件，避免大文件集 IPC 风暴。
- 启动回滚可见：`StartupOutcome::RolledBack` 现在映射为 `EngineState::RolledBack` 并广播，前端可提示“已回滚到 vX”。
- 接入防呆：worker 模式参数到达 `setup` 时返回 `E_CONFIG_INVALID`，提示必须在 `main` 开头调用 `run_applier_from_args()`，不再静默开第二个实例。
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
- 真实演练：`run.ps1` 在缺少 Python 时给出明确错误；README 补充 PowerShell/Rust/Python 前置条件。
- 文档修正：故障排查不再声称插件自动写日志，补充 `tracing-subscriber` 初始化与 `tauri-plugin-log` 桥接方法；澄清 `Checking` 是瞬态、检查结束后的回流是合法状态。
- 发布元数据：engine/plugin/cli 补充 `readme`、`keywords`、`categories`、`documentation`；testkit 开发依赖改为 path-only，避免要求发布测试夹具 crate；npm 包补充 `keywords` 与 `sideEffects`。
- 性能基线：新增 `journal/verify` criterion 基准（100 个 Journal 操作重放、1000 个暂存文件校验），并为所有 bench 声明 `harness = false`。
- 真实更新演练：新增 `real-test/`，一条 PowerShell 命令完成测试密钥生成、v1/v2 软件目录构造、真实 CLI 补丁发布、本地 HTTP 服务、真实引擎更新与健康确认；`delta-update-testkit` 增加 `testkeygen` 测试密钥工具。
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
