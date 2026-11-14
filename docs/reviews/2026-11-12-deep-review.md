# Delta Update Engine 深度评审报告

> **状态说明**：本报告记录 2026-11-12 修复前的基线问题。随后已按报告优先级实施了 Windows-only 收敛、内容寻址 blob/全量修复、配置降级、默认值收紧、精确 install_root、回滚一致性、旧哈希快照、重定向/416/预签名修复、CLI 重试与日志，以及 **journal 绑定签名清单（worker 用内嵌公钥验签）** 等改动，具体见 `CHANGELOG.md` 的 `[Unreleased]`。报告中“当前实现”的描述以修复前代码为准；仍未完成的主要是真实 UAC 提权交互、真实 COS 自动化 E2E 与性能 I/O 收敛（worker 跨进程路径已由 `worker_process.rs` 自动化覆盖）。

- 评审对象：`delta-update-engine` / `tauri-plugin-delta-update` / `delta-update-cli` / `tauri-plugin-delta-update-api`
- 评审日期：2026-11-12
- 代码状态：workspace `0.1.0`，未发布 crates.io/npm；本地仓库无远端
- 评审范围：Rust 源码、Cargo 配置、权限文件、构建脚本、CLI 发布管线、前端包、示例、e2e、测试、CI、全部文档
- 评审方法：逐文件阅读 + 契约/故障注入测试核对 + 本地执行测试 + 真实升级路径推演
- 结论口径：按“能否作为生产级、可跨平台、可长期维护的 Tauri 增量更新内核”评估，而不是按“0.1.0 原型是否跑通”评估

---

## 0. 一页结论

### 0.1 总体判断

**这是一个架构方向正确、核心机制已成型、测试基础明显高于普通原型，但安全信任链、恢复状态机和文档可信度存在硬伤的 Windows 优先 0.1.0 项目。**

它已经具备：

- 文件级增量更新与 qbsdiff+zstd 补丁闭环；
- minisign 验签先于解析、路径穿越防护、目标哈希校验；
- write-ahead journal、幂等重放、崩溃循环回滚、跨卷替换；
- Tauri 2 生命周期、六个命令、权限包、状态事件；
- 真实 CLI 发布管线与 6 个 e2e 场景；
- 182 个测试全部通过（本地首次全量执行）。

它还不能：

- 作为通用跨平台更新器交付；
- 在 per-machine / 提权场景下把 journal 当作可信输入；
- 支持跨多个版本的升级（只支持 N-1 → N 的常规路径）；
- 在本地文件被篡改、缺失或损坏时自修复；
- 让开发者“照文档接入即成功”。

### 0.2 评分

| 维度 | 得分/10 | 结论 |
|---|---:|---|
| 架构设计与模块边界 | 7.0 | 分层清晰，但公开面和运行时状态机没有收住 |
| 增量算法 / 差分与合并 | 5.5 | 单策略可用，单基线、无修复、多版本与内存问题明显 |
| 安全性与签名校验 | 4.0 | 清单验签强，提权 worker 的 journal 信任链断裂 |
| 可靠性与回滚 / 失败恢复 | 5.0 | WAL 设计好，恢复状态机存在 P0 级漏洞 |
| 跨平台兼容 | 2.5 | 实际 Windows-only，文档宣称的 Linux/macOS 支持不成立 |
| 性能与资源占用 | 5.0 | Range 下载和哈希缓存是亮点，I/O 放大和内存峰值严重 |
| Tauri 2.x 集成与权限 | 6.5 | 生命周期完整，但接入钩子、启动失败策略和默认值有风险 |
| 代码质量与可维护性 | 6.0 | Rust 代码整洁，存在死代码、死配置和文档漂移 |
| 测试覆盖 | 6.0 | 数量与层次不错，关键安全/恢复/平台场景缺失 |
| 文档完善度与准确性 | 3.5 | 体量大，但存在不存在的 API、过期版本和自相矛盾 |
| 工程实践 / CI/CD / 发布 | 5.0 | 基础门禁有，缺 macOS、MSRV、覆盖率、发布自动化 |
| 开发者体验（DX） | 4.0 | 接入步骤、日志、错误、CLI 效率均有明显摩擦 |
| 用户体验 / 产品完整度 | 5.0 | 静默更新体验好，进度、健康确认和同意权不足 |
| 社区生态与长期演进 | 2.5 | 未发布、无社区入口、Windows-only、文档信任受损 |

**综合：4.8/10。工程质量单独看约 6.2/10；生产就绪度约 3.5/10。**

一句话：**作为 Windows 内部灰度/受控环境可用；作为公开宣传的跨平台生产级更新内核，当前不具备发布条件。**

### 0.3 必须先修的 7 个 P0

1. **提权 worker 信任未认证的 `apply.json` / `commit.journal`**：checksum 不是签名，`Place` 的哈希也来自 journal；配合可被放宽的 `install_root`，本地攻击者可让提权 worker 把任意内容写入安装根祖先目录。文档却声称 worker 会验证操作列表签名。
2. **回滚中断恢复后不清理 commit journal**：回滚完成后下一次启动会报告 `Committed`（新版本），但磁盘上仍是旧版本；还可能重新应用更新。
3. **未变化文件没有 `full_key`**：跨版本升级（v0→v2）或本地文件被改坏/删除时，`diff` 直接报 `E_MANIFEST_PARSE`，没有全量修复路径，用户永久卡住。
4. **平台支持声明与实现不符**：macOS/Linux 没有格式门禁，Unix 可执行位不保留，macOS 资源不在 install root 内，AppImage 安装根只读；但文档声称支持。
5. **配置校验失败会阻止整个应用启动**：可选的更新功能可以把主应用直接“打不开”；`delta-update init` 的占位公钥复制后必然触发。
6. **`auto_confirm` 默认 true + 示例 1 秒后无条件确认**：坏更新只要窗口出现就会被判定健康，崩溃循环回滚失效。
7. **回滚用备份文件自身哈希构造 Restore**：`.old` 备份被篡改/损坏时，回滚会把损坏内容当作旧版本恢复，并报告成功。

---

## 1. 评审方法与证据

### 1.1 实际执行

| 命令 | 结果 |
|---|---|
| `cargo test --workspace --quiet` | 全部通过；本地首次执行约 182 个测试 |
| `cargo test -p delta-update-engine -p delta-update-cli --quiet`（清理本地 crate 后） | 159 个测试全部通过 |
| `cargo run -q -p delta-update-e2e` | 6/6 PASS：`fresh_install`、`delta_update`、`crash_loop_rollback`、`uac_cancelled`、`cross_volume`、`rollback_release` |
| `cargo xtask docs-check` | 通过，所有相对链接存在 |
| 源码审阅 | engine/plugin/cli/testkit/e2e/xtask 全部 Rust 源文件；根配置、CI、权限、示例、前端包 |
| 场景推演 | 新接入、v0→v2 跨版本、文件损坏修复、回滚中断、私桶大文件、macOS/Linux、CI 发布 |

> 说明：本地首次 e2e 运行复用了工作区 `target` 缓存，`cross_volume` 输出中出现旧目录 `D:\Desktop\tauri-delta-updater`；清理本地 crate 后重新运行，路径已回到当前仓库，6/6 仍通过。这说明 CI/发布必须 clean build，不能复用跨路径的 `target`，否则会嵌入旧的 `CARGO_MANIFEST_DIR`。

### 1.2 严重级别

| 级别 | 含义 |
|---|---|
| P0 | 安全边界、数据一致性或主流程不可用；发布阻断 |
| P1 | 高概率生产故障、明显 DX 伤害或重要场景不可用 |
| P2 | 可用性/性能/可维护性问题，不阻断但应排期 |
| P3 | 细节、清理、文档措辞 |

### 1.3 评分口径

- 9–10：生产级，边界清晰，有证据与自动化门禁。
- 7–8：核心可用，存在已知限制但不影响主路径。
- 5–6：原型/早期产品，正常路径可用，异常路径和 DX 有明显缺口。
- 3–4：声明大于实现，或存在会阻断采用的硬伤。
- 0–2：不可用或不可信。

---

## 2. P0 缺陷详述

### P0-01 提权 worker 信任未认证的 journal / apply.json

**现象与证据**

- `apply.json` 和 `commit.journal` 只是普通 JSON/二进制，`rollback.journal` 也一样；journal 尾部是**无密钥 SHA-256**，只能发现损坏，不能认证来源：`crates/delta-update-engine/src/journal/format.rs:95-97`、`crates/delta-update-engine/src/journal/format.rs:132-135`。
- `Place` / `Restore` 的 `sha256`、`size` 来自 journal 自身；重放时用这些值校验来源和目标，而不是用签名清单校验：`crates/delta-update-engine/src/journal/replay.rs:122-171`。
- worker 的 `validate_scope` 只检查 `exe.starts_with(install_root)`，没有要求 `install_root` 是 exe 的**直接父目录**，也没有要求它与签名清单/配置绑定：`crates/delta-update-engine/src/apply/applier.rs:135-165`。
- `apply.json` 的 `install_root`、`staging_root`、`journal_root` 没有签名：`crates/delta-update-engine/src/journal/state.rs:17-26`。
- 文档明确声称 worker 会“验证操作列表的签名/完整性”：`docs/design/permission-model.md:54`，并声称“同名 Journal 不可伪造”：`docs/design/permission-model.md:218`。这与实现不符。
- `docs/design/security.md:118-122` 自己承认 checksum 不是认证，但给出的防线是“Place/Restore 哈希”；这些哈希同样由不可信 journal 提供，形成循环论证。

**影响**

- 能写 `journal_root`（默认在用户 AppData，用户可写）的本地程序，可以构造 `apply.json` + `rollback.journal`/`commit.journal`，把 `install_root` 设为 exe 的某个祖先目录（例如 `C:\Program Files` 而非 `C:\Program Files\MyApp`），并附带任意 `sha256`。
- 应用下一次启动时，`recovery_needs_install_write` 看到 `rollback.journal` 就会走启动期提权恢复：`crates/tauri-plugin-delta-update/src/lifecycle.rs:115-127`；用户看到的是应用自己的 UAC 提示，批准后提权 worker 执行伪造 journal。
- 受影响范围包括 per-machine 安装、便携版被管理员启动、任何具有提权恢复路径的场景。标准用户没有管理员凭据时无法批准 UAC，但这不是安全边界：UAC 是“允许这个应用”，不是“允许任意文件写入”。
- 这也意味着“journal 被篡改不会让未校验字节进入安装目录”的文档承诺不成立。

**最佳修复方案**

1. **把签名清单带进 journal 目录**：`begin_commit` 同时持久化 `latest.json` 原始字节与 `.minisig`，以及（回滚用）上一版本的签名清单。worker 启动后先用内嵌公钥重新验签，再执行任何文件操作。
2. **journal 绑定清单哈希**：`commit.journal` 增加 `manifest_hash`、`from_manifest_hash`、`install_root_canonical`、`staging_root_canonical`；`FileOp` 的 `sha256` 必须能在签名清单中找到对应路径和值，找不到即拒绝。
3. **回滚使用受信旧哈希**：inverse journal 的 `Restore` 必须使用旧签名清单中的哈希，而不是对备份文件现算。
4. **收紧安装根**：worker 从自身 `current_exe` 推导安装根，要求 canonical 后等于配置值；拒绝盘符根、用户目录、`Program Files` 等宽祖先。最稳妥的是在安装目录放一个受保护的 marker 文件，worker 只接受 marker 所在的目录。
5. **journal 版本与路径校验**：`Journal.version` / `from_version` 必须通过 SemVer 校验；`staging_root.join(version)` 必须仍在 staging root 内；拒绝 `.old` / `.trash` 大小写变体。
6. **ACL 与降权**：journal/staging 目录必须只允许当前用户写；提权 worker 不应读取其他用户可写的目录。若做不到，拒绝提权而不是“带病执行”。
7. **测试**：新增“伪造 apply.json + journal + 宽 install_root”的 E2E，断言 worker 在验签失败时拒绝执行，且无法写入安装根祖先。

> 这是一项架构级修复，不是加一个 checksum 能解决的。核心原则：**worker 的信任根必须回到内嵌公钥，而不是磁盘上可写的 journal。**

---

### P0-02 回滚中断恢复后不清理 commit 状态

**现象与证据**

- `recovery::recover` 的“恢复 rollback.journal”分支在重放成功后只删除 rollback 文件、重置 attempts，然后返回 `RolledBack`：`crates/delta-update-engine/src/recovery.rs:61-93`。
- 它没有删除 `commit.journal`、`commit.status`、`prepared.json`，没有写 `skipped_version`，也没有清理 staging/`.old`/`.trash`。
- 正常回滚路径 `rollback::rollback` 则会做这些清理：`crates/delta-update-engine/src/rollback/mod.rs:39-48`。
- 下一次启动时，`recovery` 看到仍存在的 `commit.journal` 且 `commit.status=completed`，在 attempts 未超过阈值时直接返回 `Committed { version: 新版本 }`：`crates/delta-update-engine/src/recovery.rs:101-120`。此时磁盘上已经是回滚后的旧版本。

**影响**

- UI 显示“已更新到 vX”，实际文件是旧版本；
- `confirm_healthy` 可能清理回滚素材，使后续无法再恢复；
- 崩溃循环的 `skipped_version` 没有写入，可能再次自动更新到同一坏版本；
- 如果状态是 Failed 或无状态，下一次启动还可能重放 commit journal，把旧版本再次替换成新版本，形成回滚/更新循环。

**最佳修复方案**

- 把 commit/rollback 恢复收敛为**单一状态机 + 单一事务**：`update.state.json` 记录 `phase`、`commit_id`、`from_version`、`to_version`、`manifest_hash`、`generation`；所有恢复分支只通过状态转移表执行。
- 回滚完成的最后一个原子步骤必须同时完成：写 `skipped_version`、删除 commit/rollback/status/prepared/apply、清理 staging 与备份。任何一步失败都保持 `RollingBack` 状态，下次继续，而不是返回 `RolledBack`。
- 给 `recover` 增加“恢复后校验”：返回 `Committed` 前必须校验目标版本文件树；返回 `RolledBack` 前必须校验旧版本文件树。
- 为 rollback 的每个步骤做 kill-point 测试（当前测试只覆盖正常回滚和部分 commit 故障，没有覆盖 rollback 中断后的下一轮状态）。

---

### P0-03 未变化文件没有 full 回退，跨版本升级与损坏修复不可用

**现象与证据**

- CLI 只对 `Added` / `Modified` 生成 `full_key`；`Unchanged` 条目 `full_key=None`：`crates/delta-update-cli/src/pipeline/mod.rs:182-240`，测试也明确断言 `same.txt` 的 `full_key.is_none()`：`crates/delta-update-cli/tests/release_pipeline.rs:149`。
- 客户端 `diff` 只要本地文件与远程哈希不一致，就要求 `full_key`，否则返回 `E_MANIFEST_PARSE`：`crates/delta-update-engine/src/manifest/diff.rs:46-64`、`crates/delta-update-engine/src/manifest/diff.rs:99-105`。
- 因此：
  - 用户从 v1.0 直接跳到 v3.0（跳过 v2.0）时，v1→v3 中“在 v2→v3 未变化”的文件没有 full_key，只要本地版本不同就失败；
  - 用户本地文件被杀毒软件隔离、手动改坏、断电损坏，且该文件在新版本中未变化时，`prepare` 的 `verify_unchanged_files` 返回 `E_PATCH_SOURCE_MISMATCH`，没有全量修复路径：`crates/delta-update-engine/src/staging/prepare.rs:170-208`。
- 错误不是“更新失败可重试”，而是清单解析/源哈希错误；普通用户没有恢复入口。

**影响**

- 实际发布中“跳过版本升级”和“本地文件损坏”都是常见场景；当前设计只保证相邻版本、文件树完全健康时的升级。
- 发布端即使想补 full_key 也没有开关；只能手工改清单，破坏签名流程。
- 这与架构文档“未变化文件零传输”的优化目标冲突：为了可修复性，必须接受一定冗余。

**最佳修复方案**

- **首选：内容寻址 blob 存储**。发布端把所有文件内容按 `blobs/<sha256>` 去重存储；清单每个条目都带 `blob`（目标哈希）；`delta_key` 只是可选的加速路径。客户端任何本地不匹配都可以直接拉 `blob`，天然支持跨版本、修复、回滚旧版本，且相同内容只存一份。
- **次选（改动小）**：发布端默认给**每个条目**都生成 `full_key`/`blob`，包括 Unchanged；存储成本由 CDN 生命周期和去重控制。客户端把 `full_key` 作为“修复源”，允许在 `check` 阶段就把损坏文件标记为 Modified。
- **多基线**：清单或旁路索引支持多个 `from_sha` 的 delta；客户端按当前文件哈希选择补丁，找不到就用 blob。不要只支持单基线。
- **修复模式**：`check` 发现本地文件与当前版本不一致时，不应只返回“无更新”；应返回可修复的差异，或在 `prepare` 中自动走全量修复，而不是报错卡死。

---

### P0-04 平台支持声明与实现不符

**现象与证据**

- `ensure_supported` 只按 `target_os` 判断，Windows/Linux/macOS 全部返回 `Ok`：`crates/tauri-plugin-delta-update/src/desktop.rs:4-17`。
- `UnsupportedFormat` 错误码存在，但全仓库没有任何构造点（grep 仅错误定义/映射/文档）；API 文档声称“注册期或下载前返回 `E_UNSUPPORTED_FORMAT`”：`docs/reference/api.md:803-815`，实际没有格式检测。
- `FileEntry.executable` 字段存在，但 CLI 从不写入、引擎从不读取：`crates/delta-update-engine/src/manifest/model.rs:147`、`crates/delta-update-cli/src/pipeline/manifest.rs:9-20`。
- 暂存/替换创建新文件时没有保留 Unix mode；完整文件下载、补丁输出都是默认权限。Linux/macOS 更新主程序后会丢失 `+x`。
- 插件默认 `install_root` 是 exe 父目录：`crates/tauri-plugin-delta-update/src/lifecycle.rs:39-46`。macOS `.app` 的 exe 在 `Contents/MacOS`，资源在 `Contents/Resources`，默认根无法更新资源；同时修改 bundle 内部文件会破坏代码签名。
- Linux AppImage 的 exe 位于只读挂载；`needs_elevation` 在非 Windows 永远返回 `false`：`crates/tauri-plugin-delta-update/src/applier.rs:112-115`，worker 会以普通用户直接写只读挂载，失败后没有格式门禁。
- 文档自相矛盾：`CHANGELOG.md:89` 说“只交付 Windows 桌面端”，`docs/guides/getting-started.md:27` 和 `docs/reference/api.md:807-815` 说 Linux AppImage/tar、macOS 未签名包支持；`CONTRIBUTING.md:7` 又说“有限支持”。

**影响**

- 用户在 Linux/macOS 上会先下载大量数据，再在 commit 阶段失败；或者更糟，替换成功但可执行位丢失/签名失效，应用无法启动。
- 文档承诺的 `E_UNSUPPORTED_FORMAT` 不会出现，支持人员无法按文档定位。
- 公开宣称“跨平台”会直接损害项目可信度。

**最佳修复方案**

- **1.0 明确 Windows-only**：在插件 setup 最早期按安装形态门禁，不支持时返回 `E_UNSUPPORTED_FORMAT`，且不下载任何字节；文档、README、平台矩阵统一为 Windows 10+ per-user/per-machine。
- macOS 交给官方 Tauri updater / PKG / Sparkle 类方案；修改 `.app` 后必须重新签名，文件级替换不适合作为默认方案。
- Linux AppImage 交给 AppImageUpdate 或整包/仓库更新；tar 自更新若要支持，必须：
  - 清单记录 mode（至少 `executable` 位）与 symlink 信息；
  - 替换后恢复权限；
  - 明确 `install_root`，不要把 `Contents/MacOS` 当整个 bundle；
  - 在 CI 的 Linux/macOS runner 上跑真实替换测试。
- 如果短期不做，删除相关文档承诺，标注“规划中”，不要用 `ensure_supported` 的 `Ok` 误导调用方。

---

### P0-05 配置校验失败阻止主应用启动

**现象与证据**

- 插件 `setup` 直接 `api.config().validate()?`，随后 `lifecycle::setup(...)?`：`crates/tauri-plugin-delta-update/src/lib.rs:32-36`。
- `Config::validate` 对 `endpoint`、公钥、`install_dir`、`staging_dir`、`journal_dir` 等做硬校验，任何一项失败都返回 `Err`：`crates/tauri-plugin-delta-update/src/config.rs:90-229`。
- Tauri 的插件 setup 返回 `Err` 会导致整个应用启动失败，而不是进入“更新功能不可用”的状态。
- `delta-update init` 的占位公钥是 `RWQ...your-minisign-public-key...`：`crates/delta-update-cli/src/commands/init.rs:10-12,77-80`；这个值不是合法 minisign 公钥，复制到 `tauri.conf.json` 后 `validate_public_key` 失败，应用无法启动。示例 README 说占位 endpoint 只会让状态变 `Failed`、不影响界面；这只对已有合法公钥的示例成立，对 `init` 输出不成立。

**影响**

- 一个可选更新功能的配置错误可以让整个桌面应用打不开，用户只能看到 Tauri 启动错误或空白窗口。
- 开发者第一次照 `init` 输出接入时，最容易犯的错误（占位值未替换）直接导致“应用启动不了”，而不是“更新不可用”。
- CI 中配置拼写错误无法通过“应用仍可启动、日志报错”降级，必须回滚安装包。

**最佳修复方案**

- **更新功能必须可降级**：`setup` 只做最低限度检查；配置无效时构造一个 `Disabled`/`Failed` 状态，记录结构化错误并广播，主窗口照常创建。
- `Engine` 构造失败不应成为 Tauri setup 错误；把 engine 构建放到 `manage` 前的 `Result` 中，失败则 `manage(PluginState::disabled(error))`，命令返回 `E_CONFIG_INVALID`。
- `init` 输出必须是“可启动”的：要么默认写一个 `enabled: false` 的配置，要么拒绝在没有合法公钥/endpoint 时输出占位值并明确返回非零。
- 增加 `delta-update doctor --config <tauri.conf.json>`，在 CI 中校验配置，不依赖启动应用。

---

### P0-06 `auto_confirm` 默认 true 且示例 1 秒无条件确认

**现象与证据**

- `auto_confirm` 默认 `true`：`crates/tauri-plugin-delta-update/src/config.rs:19-24`。
- `confirm_delay_ms` 默认 3000ms；`on_window_ready` 在**第一个窗口就绪**后延迟确认：`crates/tauri-plugin-delta-update/src/lifecycle.rs:238-269`。
- 示例 `examples/minimal-tauri-app/src/update.ts:36` 在页面加载 1 秒后无条件调用 `controller.confirm()`，不判断是否处于 `Committed`，也不等待业务初始化。
- 健康确认的语义是“应用已进入可用状态”，但默认实现只保证“窗口出现了 3 秒”。

**影响**

- 新版本如果窗口能显示、但核心功能/数据库/登录/插件初始化在 3 秒后崩溃，会被判定为健康；崩溃循环保护失效，用户被永久留在一个坏版本。
- 示例会教育接入者写出错误实现，且 `confirm_healthy` 在非 Committed 状态下也会重置 attempts，掩盖问题。
- 对启动慢、首屏重的应用，3 秒阈值会误判。

**最佳修复方案**

- `auto_confirm` 默认改为 `false`；默认行为是“等待应用显式确认”，并在文档中给出正确时机（主界面完成首屏 + 核心数据加载 + 关键插件初始化）。
- 提供 Rust/JS 健康检查注册接口：`register_health_check(|| async { ... })` 或 `confirm_healthy()` 由应用在明确的 `ready` 事件调用。
- `confirm_healthy` 在非 `Committed` 状态下应返回 `E_INVALID_TRANSITION` 或至少不重置 attempts；示例不得无条件调用。
- 增加启动看门狗：在确认前记录阶段（窗口、前端 ready、业务 ready），只有达到最后阶段才允许确认。

---

### P0-07 回滚会把损坏的备份当作旧版本恢复

**现象与证据**

- `rollback::inverse` 对每个 `Place` 检查 `.old/<commit_id>/<dst>` 是否存在，存在则**现场计算备份文件的 SHA-256 和大小**，用它们构造 `Restore`：`crates/delta-update-engine/src/rollback/inverse.rs:27-56`、`crates/delta-update-engine/src/rollback/inverse.rs:77-89`。
- `replay` 的 `Restore` 校验的是“来源哈希 == journal 提供的哈希”“目标哈希 == journal 提供的哈希”：`crates/delta-update-engine/src/journal/replay.rs:151-171`。
- 也就是说，备份被篡改/损坏时，journal 里的“期望哈希”会跟着变成损坏内容的哈希，校验必然通过。
- 正确做法应使用旧版本签名清单中的哈希；当前没有把旧清单/旧哈希持久化到 journal 上下文。

**影响**

- 回滚可能把损坏文件恢复成“旧版本”，应用仍然打不开；但系统报告 `RolledBack` 成功。
- 对 per-machine 场景，`.old` 在安装目录内，受保护；但损坏（磁盘、杀毒、部分写入）仍可能发生；对 per-user 场景，本地用户也可修改。
- 这是回滚可信度的根本缺口，和 P0-01 同源：journal 里的哈希不应是信任源。

**最佳修复方案**

- 在 `begin_commit` 时把**当前版本的签名清单**（或至少所有被替换/删除文件的 sha256 + size 清单）写入 journal 上下文，并由 worker 验签；rollback inverse 只使用这份受信清单中的旧哈希。
- `Restore` 校验失败时返回 `E_ROLLBACK_FAILED`，保留备份和 journal，进入 `NeedsAttention`，不要继续清理。
- 为“备份损坏”“备份缺失”“备份哈希不匹配”增加故障注入测试。

---

## 3. 分维度评审

### 3.1 架构设计与模块边界（7.0/10）

**做得好的地方**

- engine 零 Tauri 依赖，plugin 只做生命周期适配，CLI 复用 engine 的容器/哈希/清单，依赖方向基本单向。
- 模块划分（manifest/fetch/patch/staging/journal/apply/rollback/state/fs/path）与问题域对应，文件规模可控。
- `#![forbid(unsafe_code)]`（engine/CLI）与插件仅一处 `unsafe`（ShellExecuteW），审查面小。

**不足**

1. **公开面失控**：`lib.rs` 把 `apply`、`journal`、`patch`、`staging`、`state`、`fs`、`path` 全部 `pub mod`，`PreparedPlan`、`HashCache`、`PatchHeader`、`ReplayContext`、`RealFs`、`DiffProvider` 等都成为公开 API。文档却宣称“公开接口的窄比全更重要”、trait 采用 sealed/feature 限制：`docs/reference/api.md:1-22`。实际 SemVer 维护面远大于文档承诺。
2. **状态机没有进入运行时**：`EngineState::can_transition_to` / `transition_to` 只在单元测试中使用：`crates/delta-update-engine/src/state/machine.rs:78-117`；插件 `publish` 直接 `set_state`：`crates/tauri-plugin-delta-update/src/commands.rs:72-75`。因此 `E_INVALID_TRANSITION` 并不是全局状态机保证，只是命令层的零散 busy 判断。
3. **未实现的抽象**：`DiffProvider` 没有实现；`Strategy` 里的 hdiff/zucchini/lzma2/zstd-dict 没有运行侧分发；`Clock` 模块在文档中存在但代码不存在。
4. **状态表达自相矛盾**：`Committing { processed, total }` 在插件中永远以 `0,0` 发布；`RollingBack` 从未在插件中发布；`RolledBack` 只在启动恢复时出现。协议文档却把它们当作可观测状态。
5. **engine 与 plugin 的职责边界在恢复/提权上被拉长**：engine 的 `run_recovery`、`validate_scope`、`run_applier` 同时承担文件操作、参数解析和提权安全判断；插件又做一次 `needs_elevation`。边界可以更清晰，但当前不是最严重问题。

**最佳方案**

- 把 `lib.rs` 收敛为 curated re-export：只公开 `Engine`、`EngineConfig`、`Limits`、`CheckOutcome`、`PreparedUpdate`、`PendingCommit`、`StartupOutcome`、`Progress`、错误类型、`Fetcher`/`FileSystem`（如需扩展）；其余 `pub(crate)`。
- 把状态机做成唯一写入口：`PluginState::transition(next)` 内部校验并持久化；命令和 recovery 都只能走它。
- 删除 `DiffProvider` 或实现并接入；删除未实现策略，或在 manifest 中用 `supported: false` 明确标注。

### 3.2 增量算法与差分 / 合并流程（5.5/10）

**做得好的地方**

- qbsdiff+zstd 的 `.tdp` 容器有 magic/format/strategy/codec/target_size/source_sha/target_sha，运行侧可在解压前拒绝错配。
- 目标哈希和源哈希双重校验，补丁被视为不可信输入，失败回退全量。
- 补丁体积超过全量 60% 时降级；已压缩扩展名黑名单减少无效差分。
- 全量对象作为回退来源，CLI 对 changed 条目生成 full_key。

**不足**

1. **只实现了 qbsdiff+zstd**：`should_try_patch` 只认 `Strategy::QbsdiffZstd`：`crates/delta-update-engine/src/staging/prepare.rs:521-528`；`Codec::Lzma2` 直接返回“不支持”：`crates/delta-update-engine/src/patch/apply.rs:76-79`。但 manifest/协议枚举了 hdiff/zucchini/lzma2/zstd-dict：`crates/delta-update-engine/src/manifest/model.rs:12-21`、`docs/design/protocol.md:147-161`。第三方发布这些策略时，客户端会静默走全量；`download_bytes` 还会按 `delta_size` 低估下载量：`crates/delta-update-engine/src/manifest/diff.rs:92-96`。
2. **单基线**：CLI 只接受一个 `--from`，对象键是 `<from>-to-<to>`；没有多基线索引。跨版本升级只能靠 full_key，而 full_key 又缺失（P0-03）。
3. **无修复/自愈路径**：本地文件与远程不一致但远程未变化时，没有全量修复；`check` 甚至可能因为哈希缓存返回“无更新”。
4. **补丁生成策略与文档不符**：架构/技术栈文档声称“按文件类型路由、Zucchini 归一化、HDiffPatch、zstd 字典、文本双候选取小、zstd-19/lzma2-9”：`docs/design/architecture.md:143-187`、`docs/reference/tech-stack.md:29-38`。实际 CLI 只有 qbsdiff+zstd level 3、按扩展名黑名单：`crates/delta-update-cli/src/pipeline/patches.rs:92-127`。这不是“可选加速器未启用”，而是文档把规划写成了现状。
5. **全量对象未压缩**：full_key 指向原始文件；文本/JS/CSS 等可压缩内容没有 gzip/brotli/zstd 传输，新增文件流量优化不足。
6. **内存模型不可控**：CLI 读取整个 old/new 文件做 qbsdiff，且把所有候选补丁保存在 `BTreeMap` 直到上传：`crates/delta-update-cli/src/pipeline/patches.rs:52-74`、`crates/delta-update-cli/src/pipeline/patches.rs:99-108`。默认 `--jobs` 为 CPU 核数，64MiB 文件 × 17 倍内存 × 核数，CI 极易 OOM。
7. **运行侧 apply 不是流式**：`apply_patch` 一次性读 patch 和 source，再 qbsdiff 输出：`crates/delta-update-engine/src/patch/apply.rs:28-47`；与文档“流式 apply”矛盾（`docs/design/modules.md:106`、`docs/operations/capacity.md:45`）。

**最佳方案**

- 明确 1.0 只支持 qbsdiff+zstd，把其他策略从协议/文档/枚举中移除或标记为 reserved；不要发布客户端不能 apply 的清单。
- 内容寻址 blob + 每文件 blob 回退（见 P0-03），从架构上解决多版本/修复/回滚。
- 大文件：实现有界内存的流式 bsdiff apply（分块读写），或至少按 `patch_memory_factor` 计算真实预算并在 apply 前拒绝。
- CLI：补丁候选落临时文件而不是全内存；按内存预算限制默认 jobs；增加 `--max-memory`；生成 zstd level 19 与 level 3 双候选取小（在预算内）；对文本/资源考虑 zstd 字典。
- 全量对象增加 `content_encoding` 或容器压缩，CDN 侧启用 gzip/brotli。

### 3.3 安全性与签名校验（4.0/10）

**做得好的地方**

- 先验签原始字节，再解析 JSON：`crates/delta-update-engine/src/engine.rs:149-159`；`minisign` 多公钥任一匹配。
- 版本单调性守卫独立于签名，拒绝未授权降级：`crates/delta-update-engine/src/version.rs:17-35`。
- 路径校验覆盖 `..`、绝对路径、Windows 非法字符、保留设备名、段尾空格/句点、ASCII 大小写冲突：`crates/delta-update-engine/src/path/rel_path.rs:40-99`。
- 补丁目标哈希 + 全量对象哈希在落盘前校验；journal 的 Place/Restore 也在落地后校验。
- 前端无文件路径参数；权限包只含六个命令；命令面确实很窄。

**不足**

1. **P0-01 journal/apply.json 未认证**（详见上）。
2. **P0-07 回滚备份哈希循环**。
3. **install_root 宽松**：worker 只要求 `exe.starts_with(install_root)`，不要求精确父目录；`resolve_install_root` 有更严格的“exe 必须在 root 下”逻辑但**从未被插件使用**：`crates/delta-update-engine/src/path/resolver.rs:9-37`。
4. **重定向未按策略检查**：`reqwest::Client` 只设置了 connect/read timeout 和 UA，没有 `redirect::Policy`：`crates/delta-update-engine/src/fetch/http.rs:118-129`；`check_host` / HTTPS 校验只作用于解析后的初始 URL，不作用于重定向后的每一跳：`crates/delta-update-engine/src/fetch/http.rs:274-290`。`allowed_hosts` 和“HTTPS everywhere”的承诺可被 30x 绕过。
5. **符号链接/junction/reparse point 未防护**：路径校验是纯词法；`resolve_within` 只做 `starts_with`；扫描/暂存/替换都用 `root.join` 跟随链接：`crates/delta-update-engine/src/path/guard.rs:12-20`、`crates/delta-update-engine/src/engine.rs:520-533`、`crates/delta-update-engine/src/staging/prepare.rs:95,218`。安装根或 staging 内被放入 junction/symlink 时，写操作可逃逸。
6. **保留目录未禁止**：manifest 允许 `.old/...`、`.trash/...` 及其大小写变体；`scan_local` 只跳过精确 `.old`/`.trash`：`crates/delta-update-engine/src/engine.rs:532`，`parse_manifest` 不拒绝：`crates/delta-update-engine/src/manifest/parse.rs:60-106`。在 Windows 大小写不敏感文件系统上，`.OLD/foo` 会写进 `.old` 备份目录，破坏回滚素材。
7. **签名服务响应无大小上限**：`HttpSigningService::resolve` 直接 `response.json()`：`crates/delta-update-engine/src/fetch/signing.rs:80-87`；恶意/故障签名服务可返回巨大 JSON 造成内存压力。
8. **私有桶/企业网络支持不完整**：`headers` 只加在清单/签名请求，没有加到签名服务 POST；无法给预签名服务配置 Authorization header：`crates/delta-update-engine/src/fetch/http.rs:292-358`、`crates/delta-update-engine/src/fetch/signing.rs:67-79`。没有客户端证书/自定义 CA 配置。
9. **权限粒度**：默认权限包含 `restart_to_apply`（可触发退出/重启）和 `prepare`（可消耗带宽/磁盘）；没有只读 `status` 的独立权限包。WebView 被攻破时可反复触发 prepare/restart，造成 DoS。
10. **文档安全承诺过度**：permission-model 声称 worker 验证“操作列表签名”、Journal 不可伪造；security.md 声称“本地攻击者篡改 Journal 不能写入未校验字节”。这些在 P0-01 下不成立。

**最佳方案**

- 见 P0-01/P0-07 的签名清单 + 受信旧哈希方案。
- 重定向策略：自定义 `redirect::Policy`，每一跳重新校验 scheme/host/allowed_hosts；禁止 HTTPS→HTTP；限制跳转次数。
- 路径解析：使用 `openat`/`O_NOFOLLOW`（Unix）与 `FILE_FLAG_OPEN_REPARSE_POINT`/拒绝 reparse point（Windows）打开目录/文件；对安装根做 canonicalize 并拒绝链接；对 staging 目录设置用户私有 ACL。
- 明确保留目录：manifest/journal/Journal path 校验统一拒绝首段 `.old`/`.trash`（大小写不敏感）。
- 签名服务响应限制为 64KiB；支持自定义 headers/客户端证书；`allowed_hosts` 默认收紧。
- 默认权限拆分为 `delta-update:read`（status/check）与 `delta-update:manage`（prepare/cancel/confirm/restart），默认只授予 read；高风险命令要求显式开启。

### 3.4 可靠性与回滚 / 失败恢复（5.0/10）

**做得好的地方**

- write-ahead journal：先在 journal 目录写入并 fsync，再触碰安装根；提交点清晰。
- 幂等重放：Backup/Place/Trash/Restore 都有 `is_done` 判定；目标哈希校验；跨卷 copy+rename。
- 崩溃循环检测、`skipped_version`、`confirm_healthy` 清理、启动期提权恢复都有实现和测试。
- e2e 覆盖 fresh install、delta update、crash-loop rollback、UAC cancel、cross-volume、rollback release。

**不足**

1. **P0-02 回滚中断状态机**。
2. **P0-07 回滚备份哈希**。
3. **同长度损坏文件导致 416 硬失败**：`download_verified` 发现已有文件大小正确但哈希错误时，先进入 `download_with_retry`，`resume_from` 等于文件大小；HTTP 请求 `Range: bytes=<size>-`，多数对象存储返回 416；`Http4xx` 不可重试，`download_with_retry` 立即返回错误，`download_verified` 不会先删除损坏文件：`crates/delta-update-engine/src/staging/prepare.rs:439-519`、`crates/delta-update-engine/src/fetch/http.rs:556-639`。用户必须手工删 staging。
4. **Range 下载不刷新预签名 URL**：`try_download_ranged` 只 resolve 一次 URL；分块 403 时 `download_chunk` 重试同一过期 URL，3 次后返回 `Http4xx`；`download` 只对 `retryable()` 错误回退单流，403 不可重试，直接失败：`crates/delta-update-engine/src/fetch/http.rs:365-421`、`crates/delta-update-engine/src/fetch/http.rs:474-495`、`crates/delta-update-engine/src/fetch/http.rs:784-823`。这与文档“预签名 URL 过期自动刷新续传”矛盾。
5. **Hash cache 可能造成错误结论**：`check` 用 size+mtime 命中缓存，不重新哈希：`crates/delta-update-engine/src/engine.rs:541-553`、`crates/delta-update-engine/src/manifest/hash_cache.rs:1-10,66-71`。备份恢复、`robocopy` 保留时间戳、同秒修改都可能让缓存返回旧哈希；远程版本相同时会显示“已是最新”，远程有更新时 `prepare` 会报 SourceMismatch，没有修复。
6. **多实例 race**：`begin_commit` 在写 journal 后释放锁；worker 还没启动前，另一个实例可以再次 `begin_commit` 覆盖 `apply.json` 和 `commit.journal`。插件没有强制单实例（README 只是建议），两个实例可能互相覆盖提交意图，worker 因 commit_id 不匹配失败或最后写入者获胜：`crates/delta-update-engine/src/engine.rs:286-323`、`crates/delta-update-engine/src/apply/writer.rs:33`。
7. **Journal 容量最坏情况不匹配**：32MiB / 100k ops，与 `max_files=20000`、单路径 1024 字节组合在最坏情况下可能超过 32MiB；文档声称“对齐”不严谨：`crates/delta-update-engine/src/journal/format.rs:9-10`、`crates/delta-update-engine/src/manifest/parse.rs:9-10`。
8. **清理不完整**：`confirm_healthy` 只清理当前 commit 的 `.old`/`.trash` 和当前版本 staging；历史中断留下的其他版本 staging、其他 commit 的 `.old` 可能长期残留。`remove_dir_all_best_effort` 失败只静默忽略：`crates/delta-update-engine/src/engine.rs:383-391`、`crates/delta-update-engine/src/fs/mod.rs:29-39`。
9. **Worker 启动失败/异常无可视化**：worker 的 stderr 是 `Stdio::null()`，apply 失败只 `eprintln!`：`crates/tauri-plugin-delta-update/src/applier.rs:89-104`、`crates/delta-update-engine/src/apply/applier.rs:94-96,124-127`。启动恢复 `spawn_elevated_and_wait` 只等待句柄，不检查退出码：`crates/tauri-plugin-delta-update/src/applier.rs:281-291`。
10. **`move_path` 先删目标再 rename**：Windows 上没有原子替换，存在目标短暂消失窗口；崩溃后靠 journal 重放恢复，但“任何时刻目标要么旧要么新”的文档表述不准确：`crates/delta-update-engine/src/journal/replay.rs:222-273`。
11. **Place 目标校验失败时来源已丢失**：同卷 rename 已经把 staging 源移走；目标哈希不符时删除目标，源不再存在，无法“保留来源”重试（跨卷 copy 也随后删除源）：`crates/delta-update-engine/src/journal/replay.rs:133-140`。文档说“保留来源”只在 copy 成功但 rename 前成立。
12. **回滚成功后 `apply.json` 未清理**：`rollback::rollback` 删除 commit/status/prepared，但没有删除 `apply.json`：`crates/delta-update-engine/src/rollback/mod.rs:39-48`。无害但增加诊断噪声。

**最佳方案**

- 把恢复做成“状态机 + 事务 + 可验证文件树”，而不是靠多个文件的先后删除。
- 下载层统一“损坏文件删除再重试”，禁止 `resume_from == expected_size`；对 Range 路径同样支持 403 刷新与分块续传。
- 预签名刷新逻辑下沉到统一的 `DownloadSession`，单流/Range 共用；URL 过期时重新 resolve 并从已完成字节继续。
- Hash cache 改为“仅用于展示/避免重复哈希”的提示，不作为 `check` 的唯一依据；对远程有更新时始终重新验证本地文件；提供 `--rehash`。
- 单实例：插件启动时获取命名互斥量/锁；或把 `begin_commit` 的锁持有到 worker 接管（通过文件锁/句柄继承），防止意图覆盖。
- 清理：按 commit_id 扫描 `.old`/`.trash` 和 staging 版本目录，保留当前 prepared/commit 所需的，其他可安全清理；清理失败写结构化警告。

### 3.5 跨平台兼容（2.5/10）

**不足**

1. 实际只验证 Windows：CI 矩阵是 Windows + Ubuntu，但没有 macOS；UAC/per-machine 手动验收只写在文档。
2. 平台门禁缺失（P0-04）；`E_UNSUPPORTED_FORMAT` 从未产生。
3. Unix 可执行位、符号链接、目录权限、owner/group、xattr 不保留；主程序更新后可能无法启动。
4. macOS `.app` 的 `Contents/Resources` 不在默认 `Contents/MacOS` 安装根内；修改 bundle 会破坏代码签名/公证。
5. Linux AppImage 只读挂载；deb/rpm 属于包管理器；Flatpak/Snap 只读沙箱。文档却把 AppImage/tar 标为支持。
6. Windows 长路径、大小写不敏感、保留名/尾随空格已部分处理，但 Unicode 大小写/规范化冲突、junction/reparse point、只读属性/ACL 继承未处理。
7. `dunce` 只在少数路径使用；插件构建 engine 时直接用 `current_exe().parent()`，没有 canonicalize/包含性校验。
8. 替换后的 PE 签名：更新主 exe/dll 后 Authenticode 会失效（如果发布时签名），文档只提到“签名要发生在扫描之前”，没有说明运行时替换后如何保持签名有效；对 Windows SmartScreen/企业策略是实际风险。

**最佳方案**

- 战略上收敛为 Windows-only 1.0；其他平台用官方安装器/包管理器，不要用一套文件级替换硬套。
- 若必须支持 Unix 自更新：
  - manifest 增加 `mode`（至少 executable）、`symlink` 类型；
  - 暂存/替换保留 mode，使用 `std::os::unix::fs::PermissionsExt`；
  - macOS 安装根可配置为 `Contents`，更新后执行 `codesign --force --deep --sign -`（仅开发）或走 PKG 重签名；
  - AppImage 使用 AppImageUpdate 的 zsync，而不是直接写只读镜像。
- 在 macOS/Linux CI 上跑“替换后启动”测试，而不是只编译。

### 3.6 性能与资源占用（5.0/10）

**做得好的地方**

- 大对象 ≥8MiB 使用 8MiB Range 分块、4 路并发，合并后哈希；失败回退单流。
- 哈希缓存减少重复 `check` 的全树哈希。
- 未变化文件不复制到 staging，降低峰值磁盘。
- 插件以 100ms 合并进度事件，避免 IPC 风暴。

**不足**

1. **每个变化文件被反复读取**：
   - 补丁：`verify_staged` 哈希一次（`staging/prepare.rs:545-563`）；
   - `verify_plan` 再哈希一次（`staging/verify.rs:7-30`）；
   - `begin_commit` 再 `verify_plan` 一次（`engine.rs:286-297`）；
   - journal replay 校验来源 + 目标各一次（`journal/replay.rs:133-140`）。
   一个大文件最多可被完整读取 4–5 次，更新耗时和磁盘磨损明显。
2. **`check`/`prepare` 的未变化文件重哈希是串行**：`verify_unchanged_files` 逐文件 `sha256_file`，没有使用工作区已有的 rayon；大目录首次更新会很慢。
3. **补丁 apply 内存峰值高**：source + patch + 解压 payload + 目标缓冲同时存在；`patch_memory_factor` 完全未使用（grep 仅在 config/limits 定义），`E_PATCH_MEMORY_BUDGET` 只在“文件超过 max_patch_file_size”时触发，不是真实内存预算。
4. **CLI 内存**：所有补丁候选 `Vec<u8>` 保存在内存；`--jobs` 默认核数；qbsdiff 约 17 倍输入内存，CI 可能 OOM。
5. **并发模型不统一**：`max_concurrent_downloads` 控制文件级并发，但每个 ≥8MiB 文件内部又有 4 路 Range，实际峰值可达 `4 × 4 = 16` 个连接；文档/测试 F-09 只验证文件级并发，未验证全局连接数。没有限速。
6. **无文件内进度**：`Progress::Downloading` 只在文件完成后按累计字节上报，`report_download` 一次加整个文件大小：`staging/prepare.rs:388-403`。大文件下载/apply 时 UI 长时间停在同一个百分比。
7. **验证阶段无进度**：`verify_plan`（所有 staged 文件）和 `begin_commit` 的再次校验期间没有事件；用户看到 `Downloading 100%` 后长时间无变化。
8. **HTTP 无 ETag/304**：每次 `check` 都完整下载 8MiB 上限的 manifest + signature；没有 `If-None-Match`/`If-Modified-Since`，大规模用户会放大小文件带宽和 CDN 成本：`fetch/http.rs:292-358`。
9. **全量对象未压缩**；manifest 使用 pretty JSON，20k 文件时体积偏大：`pipeline/manifest.rs:49-62`。
10. **CLI 上传串行且无重试**：`upload_objects` 逐文件 PUT + HEAD；S3/COS 客户端总超时 300 秒，大文件慢网必失败；没有 multipart/resume：`pipeline/publish.rs:30-38`、`publisher/s3.rs:50-54`、`publisher/cos.rs:31-34`。
11. **`sysinfo` 轮询父进程**每 200ms 一次，持续最多 600 秒；对一个只等待 PID 退出的场景偏重。
12. **`check` 扫描使用 `walkdir` + 串行 SHA256**；对于 20k 文件、GB 级安装，首次 check 可能数十秒，且没有进度/取消。

**最佳方案**

- 建立 `VerifiedInventory`：`check` 哈希后持久化 (path, size, mtime, file_id, sha256)，`prepare`/`commit` 复用；只在文件变化或安全策略要求时重哈希。这样把 4–5 次读降到 1 次哈希 + 1 次落地校验。
- staged 文件哈希在写入时计算一次，`verify_plan` 只校验 size/mtime + 抽样哈希；`begin_commit` 不再全量重哈希，除非跨进程恢复。
- 用 rayon 并行哈希（有界线程池），与下载并发分开；增加 `--hash-jobs`。
- 下载层使用全局 `Semaphore`（文件 × Range 总和），尊重 `max_concurrent_downloads`；增加带宽限速。
- 文件内进度：下载流每 N 字节上报；apply 分块上报；verify/commit 上报阶段进度。
- HTTP 增加 ETag 缓存（journal 目录持久化），304 时复用已验证清单；CDN 设置 `Cache-Control`（latest no-cache，artifacts immutable）。
- CLI：补丁候选落临时文件；`--jobs` 根据内存预算自动降级；上传并行 + 指数退避 + multipart；增加 `--progress`/`--json`。

### 3.7 Tauri 2.x 集成与权限（6.5/10）

**做得好的地方**

- 插件生命周期完整：setup 早期恢复、auto_check、auto_apply、auto_confirm、退出提交、启动提权恢复。
- worker 复用同一二进制、无 IPC、参数入口在 main 最早期；命令面窄；默认权限包清晰。
- 事件 payload 有版本号、错误码、action；前端控制器处理“先订阅后快照”的竞态。
- 有 Tauri mock runtime 测试，验证 ACL 拒绝、自动检查、恢复、事件。

**不足**

1. **接入必须改 `main`**：`run_applier_from_args()` 必须在 Tauri builder 之前调用，否则 setup 报错：`crates/tauri-plugin-delta-update/src/lib.rs:22-31`、`lifecycle.rs:140-145`。文档“三步接入”实际是四步，且遗漏会导致应用直接启动失败。
2. **setup 失败中止应用**（P0-05）。
3. **默认值偏危险**：`auto_check`、`auto_apply`、`auto_confirm` 全默认 true；开发者不写任何代码就获得“后台下载 + 退出替换 + 3 秒确认”。对生产应用，至少 `auto_apply`/`auto_confirm` 应显式开启。
4. **没有 Rust 侧扩展 API**：API 文档描述了 `DeltaUpdaterExt` / `DeltaUpdater`（`docs/reference/api.md:537-559`），代码不存在。Rust 侧只能通过 Tauri command 或直接拿 `Arc<PluginState>`（非公开约定）。
5. **没有单实例强制**：README 只是“建议使用 Tauri single-instance 插件”；`begin_commit` 的多实例 race 未被阻止（P0-02/P1）。
6. **权限粒度过粗**：默认包同时授予 `restart_to_apply` 和 `prepare`；没有 read-only 包。
7. **`Committing` 状态无意义**：`processed=0,total=0`，主进程随即退出；UI 无法展示替换进度。
8. **`cancel_prepare` 不能取消 check/verify/commit**；`prepare` 在 verify_plan 阶段无法取消，用户点取消后仍可能持续很久。
9. **`prepare` busy 返回 `Ok`**：命令层 `try_lock` 失败时返回当前状态并 `Ok`（`commands.rs:311-318`），调用方容易误以为准备成功；协议文档虽如此定义，但 DX 上应返回明确的 `E_INVALID_TRANSITION` 或 `InProgress` 类型。
10. **`check` 成功会清掉 `RolledBack` 提示**：`resting` 只保留 `Committed`/`Prepared`，否则回 Idle（`commands.rs:80-92`）；用户会丢失“上次已回滚”的信息。
11. **`auto_confirm` 在第一个窗口就绪时启动**，不是“应用健康”；P0-06。
12. **`app.exit(0)` 后前端 promise 可能无法完成**；`restart_to_apply` 的返回值通常到不了 UI，需要文档明确 fire-and-forget。

**最佳方案**

- 提供 `delta_update::run(tauri::Builder)` 包装器或 `#[delta_update::main]` 宏，自动在 builder 前处理 worker 参数，消除“忘记调用”的致命接入点。
- 启动失败降级为 Disabled/Failed，不阻止主窗口。
- 默认 `auto_check=true`、`auto_apply=false`、`auto_confirm=false`；提供 `on_ready` 健康检查注册。
- 实现文档中的 `DeltaUpdaterExt`，或删除文档；给 Rust 用户一个稳定句柄。
- 默认权限拆成 `delta-update:read` 与 `delta-update:manage`；高风险命令要求显式授权。
- 单实例：插件内部使用命名互斥量；或至少在 `begin_commit` 前检查已有 `apply.json`/journal 并拒绝。

### 3.8 代码质量与可维护性（6.0/10）

**做得好的地方**

- Rust 代码风格统一，错误类型用 `thiserror`，模块职责清楚，命名可读。
- `#![forbid(unsafe_code)]`；unsafe 仅限 Windows ShellExecute，且有 SAFETY 注释。
- 测试夹具、FaultyFs、MockServer 设计合理；故障注入不污染生产路径。
- 原子写入、fsync、跨卷处理等细节考虑较周全。

**不足**

1. **死代码/死配置**：
   - `test-faults` feature 声明但从未使用：`crates/delta-update-engine/Cargo.toml:13-15`。
   - `indicatif` 依赖未使用：`crates/delta-update-cli/Cargo.toml:25`。
   - `DiffProvider` 无实现、无调用：`crates/delta-update-engine/src/patch/provider.rs:7-11`。
   - `Strategy` 的 hdiff/zucchini/lzma2/zstd-dict 没有运行侧支持。
   - `KnownFiles::extend` 未使用：`crates/delta-update-engine/src/manifest/history.rs:42-44`。
   - `resolve_install_root` 未被插件使用：`crates/delta-update-engine/src/path/resolver.rs:9-37`。
   - `UnsupportedFormat` 从未构造。
   - `FileEntry.executable` 从未设置/读取。
   - `patch_memory_factor` 从未使用。
   - `EngineState::can_transition_to` / `transition_to` 只在测试中使用。
   - `RollingBack` / `Committing` 在真实插件中几乎不出现。
2. **公开 API 面过大**（见 3.1）。
3. **重复类型定义**：Rust `CheckResult`/`StatePayload`/`ErrorPayload` 与 TS `types.ts` 手工维护两份；没有 `ts-rs`/`specta`/JSON Schema 生成，容易漂移：`crates/tauri-plugin-delta-update/src/commands.rs:21-68`、`packages/plugin-api/src/types.ts:19-87`。
4. **`ErrorPayload.detail` 永远为空**：`crates/delta-update-engine/src/error.rs:427-437`；文档示例却展示带 file/hash 的 detail（`docs/design/protocol.md:862-881`）。
5. **错误上下文弱**：`EngineError::Internal(String)` 大量使用，缺少结构化字段（path/op/status）；日志与 UI 只能解析字符串。
6. **`PluginState` 可被任意代码 `try_state::<Arc<PluginState>>` 获取并直接改状态**；没有封装 transition。
7. **`state.machine` 与 `plugin.state` 两套状态逻辑**，没有单一来源。
8. **文档/代码漂移本身是维护成本**：同一功能在 README、architecture、protocol、api、tech-stack、capacity、modules、test-plan 中重复描述，任何修改要同步 8 处，已经出现大量不一致。

**最佳方案**

- 删除死代码/死配置，或实现并接入；每个公开 item 有 rustdoc 与测试。
- 用 `pub(crate)` + curated `pub use` 收紧 API；为扩展点使用 sealed trait。
- 用 `ts-rs`/`specta` 从 Rust 生成 TS 类型；用 `schemars` 生成插件配置 JSON Schema。
- `EngineError` 增加结构化 context（`path`, `op_index`, `status`, `file`），`detail` 填充脱敏后的摘要。
- 状态机唯一入口；插件不得直接 `set_state`。
- 文档去重：协议/API 作为单一契约，README/指南只链接，不复制字段表。

### 3.9 测试覆盖（6.0/10）

**做得好的地方**

- 182 个测试覆盖 manifest、path、version、journal、committer、staging、fetch、presign、plugin lifecycle、CLI pipeline、config、frontend controller。
- 故障注入：J-01..J-12、C-01..C-07、R-09/R-10、F/S 场景、SEC-01..SEC-19 的大部分。
- property test：任意字节 manifest 不 panic、路径属性。
- e2e：6 个跨进程/跨卷/回滚发布场景；CLI 真实生成补丁/签名/本地发布。
- docs-check、version-sync、cargo-deny。

**不足**

1. **没有真正的 Tauri 应用 + 真实 COS + 真实 worker 的自动化 E2E**：e2e 的 `apply` 直接调用 `apply_commit`（`e2e/src/harness.rs:265-278`），没有 spawn `--delta-update-apply`、等待父进程、`--relaunch`；plugin lifecycle 也用 `install_unconfirmed` 直接 apply。真实 worker 入口/参数/父进程等待/提权恢复没有 CI 覆盖。`real-test/run.ps1` 是手动脚本，不在 CI。
2. **P0/P1 场景无测试**：回滚中断后下一轮、伪造 journal + 宽 install_root、跨版本 v0→v2、本地文件损坏修复、Unix 可执行位、macOS/Linux 门禁、重定向绕过、Range 下载中预签名过期、416 恢复、同 size+mtime 缓存、双实例 begin_commit、`.OLD` 保留目录。
3. **测试计划存在过度声明**：
   - P-07 声称状态机非法迁移被覆盖，但插件没有使用 `transition_to`；测试只覆盖了单元级状态机。
   - L4 声称“真实 Tauri 应用 + 真实 COS”，实际 e2e 是 engine/CLI + 本地 mock HTTP；E2E-06/11 手动。
   - P-01/P-14 用 in-process apply，不是 worker。
4. **没有 macOS CI**；Linux CI 只证明编译/engine e2e，不证明插件替换。
5. **没有覆盖率门禁、fuzz、Miri、cargo-semver-checks**；property test 范围较窄。
6. **测试依赖网络端口/临时目录**；并行测试有 `SERIAL` 锁，但真实 CI 可能出现端口/时序 flaky；没有重复运行门禁。
7. **没有性能回归阈值**：bench 存在，但只说“观察”，没有基线失败。
8. **前端只测 controller，不测 React hook/错误路径/Vue/Svelte 示例**；`useUpdater` 启动失败只 console.warn，未测。

**最佳方案**

- 增加 Windows CI 的真实 worker E2E：启动 app、prepare、退出、等待 worker、校验版本、kill worker 后重启恢复、取消 UAC 的模拟（可用非提权 + 只读目录）。
- 增加 macOS/Linux runner 的编译 + 平台门禁测试（断言 `E_UNSUPPORTED_FORMAT`）。
- 把 P0/P1 场景加入故障注入矩阵，作为发布阻断项。
- 增加 `cargo-llvm-cov` 覆盖率（核心模块阈值）、`cargo-fuzz`（manifest/journal/patch header）、`cargo-semver-checks`。
- 修正测试计划，区分“自动化覆盖”和“手动验收”，不要用 L4 名义覆盖未自动化的 UAC/真实 COS。

### 3.10 文档完善度与准确性（3.5/10）

**做得好的地方**

- 文档体系完整：指南、参考、运维、测试、安全、权限、容量。
- 对威胁模型、密钥轮换、发布顺序、回滚语义的思考深度明显高于一般项目。
- 文档链接检查、目录规范、版本同步有工程化门禁。

**不足**

1. **文档描述了不存在的 API**：
   - `CommitJournal` / `begin` / `mark_completed`：`docs/reference/api.md:328-350`；代码只有 `Journal`。
   - `DeltaUpdaterExt` / `DeltaUpdater`：`docs/reference/api.md:537-559`；代码不存在。
   - `SignatureVerifier`：`docs/design/modules.md:85-90`；代码不存在。
   - `PrivilegedWriter::apply_journal(&self, root, journal)` 签名与实现不符：`docs/reference/api.md:311-316` vs `crates/delta-update-engine/src/apply/writer.rs:14-19`。
   - `Clock` 模块在 `docs/design/modules.md:33-35` 中存在，代码没有。
2. **平台矩阵自相矛盾**：`CHANGELOG.md:89` Windows-only；`getting-started.md:27`、`api.md:807-815`、`permission-model.md:225-237` 声称 Linux/macOS 支持；代码全放行。`E_UNSUPPORTED_FORMAT` 从未产生。
3. **技术栈版本过期/错误**：`docs/reference/tech-stack.md:31-38` 写 zstd 0.14、minisign 0.10、tauri 2.12.5、`hdiffpatch-rs`/`zucchini` 可选；实际 `Cargo.toml` 是 zstd 0.13、minisign 0.7、tauri "2"，且没有 hdiff/zucchini 依赖。`tech-stack.md:221` 说“插件不直接依赖 tokio”，实际插件依赖 tokio。`tech-stack.md:105` 把 indicatif 列为发布进度，实际未使用。
4. **把规划写成现状**：Zucchini/HDiff/lzma2/zstd-dict、双候选取小、zstd-19、流式 apply、`patch_memory_factor` 内存预算，均在文档中描述为已实现/默认行为；代码没有或未使用。
5. **协议行为不一致**：`docs/design/protocol.md:157-161` 说未知 strategy + full_key 回退全量、无 full_key 时 `available:false` 并写 detail；实际 `diff` 对 changed 条目直接 `E_MANIFEST_PARSE`（`manifest/diff.rs:99-105`），`download_bytes` 还按 delta_size 低估。
6. **安全承诺不成立**：permission-model 声称 worker 验证“操作列表签名”、Journal 不可伪造；security.md 声称篡改 Journal 无法写入未校验字节；实际 checksum 无密钥、哈希来自 journal（P0-01）。
7. **故障排查文档与实现不符**：`troubleshooting.md:69-77` 说安装目录以 exe 为准，注册表仅交叉校验；代码从未读取注册表，`resolve_install_root` 也未使用。`inspect` 文档说 Trash 状态可判断，实际路径少了 commit_id（`commands/inspect.rs:111-120`）。
8. **接入指南的“三步”不完整**：必须改 `main` 调 `run_applier_from_args`，这是第四步且是致命步骤：`getting-started.md:16,66-82`。
9. **`--dry-run` 帮助文本说“生成清单和补丁”，实际不落盘、不输出清单**：`cli.rs:73-75`、`pipeline/mod.rs:289-305`。
10. **文档语气/信息密度**：大量“生产级/不妥协/核心判断”的宣言与重复表格，掩盖了实现边界；开发者很难快速判断“哪些已实现、哪些是规划”。这本身是 DX 成本。

**最佳方案**

- 建立“实现状态矩阵”：每个功能标注 `已实现/部分实现/规划/不支持`，文档只描述已实现行为；规划放到 roadmap。
- 用 rustdoc 作为 API 单一来源，文档引用 `cargo doc`；删除手写的不存在 API。
- 统一平台矩阵与 CHANGELOG；`E_UNSUPPORTED_FORMAT` 要么实现，要么删除。
- 技术栈文档从 `Cargo.toml`/`Cargo.lock` 生成或加 CI 校验，禁止手写版本。
- 为协议/事件/错误码增加文档测试：示例 JSON 可被 `parse_manifest`/`serde` 解析；示例命令可编译。
- 英文 README（至少 crates.io/npm 首页），中文文档保留为深度指南。

### 3.11 工程实践 / CI/CD / 发布（5.0/10）

**做得好的地方**

- `cargo fmt --check`、`clippy -D warnings`、`cargo test --workspace`、docs-check、version-sync。
- cargo-deny（advisories/licenses/bans/sources），license 白名单合理。
- CLI 发布顺序（对象 → 签名 → versioned manifest → latest）正确，且 HEAD 校验；这比很多更新器严谨。
- release workflow 有 protected environment、版本一致性检查、crates/npm 发布顺序。
- 本地/CI 有 `xtask`，文档链接检查自动化。

**不足**

1. **CI 没有 macOS**，与文档跨平台声明冲突；Windows 的 UAC/per-machine 也不在 CI。
2. **没有 MSRV job**：`rust-version = "1.90"` 但 CI 用 stable 最新；依赖升级可能悄悄提高 MSRV。
3. **没有覆盖率/变异测试/fuzz/semver-checks**；`test-faults` feature 未使用。
4. **Release workflow 脆弱**：
   - `cargo publish` 连续执行，`sleep 60` 等待 registry 传播，可能不足或浪费；
   - 没有 `cargo publish --dry-run` / `cargo package --list` / `npm pack --dry-run` 前置；
   - 没有生成 GitHub Release、tag、checksum、SBOM；
   - 没有 `cargo semver-checks`；
   - actions 未 pin commit SHA（供应链风险）。
5. **没有发布并发保护**：两个 release 同时写 `latest.json` 可能后写覆盖先写；CLI 不检查远端最新版本，可能发布降级版本（未带 rollback）导致客户端 `E_VERSION_ROLLBACK`。
6. **没有缓存头策略**：CLI 不设置 `Cache-Control`（latest 需要 no-cache，artifacts 需要 immutable），也没有 ETag；CDN 配置完全交给用户，文档没有强制门禁。
7. **没有 artifact 保留自动化**：文档建议保留 2–3 版本，但没有 lifecycle 脚本/检查；回滚素材可能被误删。
8. **`deny.toml` 的 advisory ignore 只针对一个 RUSTSEC**；没有定期升级 Tauri/GTK 的实际计划。
9. **`rust-toolchain.toml` stable + workspace rust-version 1.90**：没有固定 CI toolchain 版本，构建不可复现。
10. **`pnpm` 版本不一致**：`package.json` 12.15.0，tech-stack 12.19.0；CI 用 12.15.0。小问题但反映文档漂移。
11. **仓库元数据缺失**：Cargo.toml/package.json 没有 `repository`/`homepage`/`bugs`；crates.io/npm 页面无法指向源码，影响信任与贡献。
12. **`项目完整源码.md`（662KB）和 `导出源码.bat`** 在仓库根（虽 .gitignore）；生成物污染工作区，docs-check 还要排除。建议移出仓库或放到 `target/`。

**最佳方案**

- CI 矩阵：Windows（完整）、Ubuntu（完整）、macOS（编译 + 平台门禁）、MSRV 1.90、stable。
- 发布流水线：`verify` → `package --dry-run` → `release`（dry-run 产物归档）→ `publish` → `verify`（用公钥）→ GitHub Release/tag/checksums/SBOM。
- 加 `cargo-semver-checks`、`cargo-llvm-cov`、`cargo-fuzz` smoke、`cargo-deny` PR job。
- CLI 发布增加远端版本检查、`Cache-Control`、ETag、并发锁（或文档强制 CI concurrency）。
- 固定 toolchain 与 actions SHA；生成依赖清单/SBOM。

### 3.12 开发者体验（DX）（4.0/10）

| DX 子维度 | 得分/10 | 主要不足 |
|---|---:|---|
| 便携性 / 可移植性 | 3.0 | 实际 Windows-only；目录/路径假设多；无安装器/平台适配；target 跨路径复用会嵌入旧路径 |
| 开箱即用 | 3.5 | 必须改 main、生成密钥、建桶、配 endpoint；`init` 占位值会让应用启动失败 |
| 接入成本 | 5.0 | Cargo + npm + capability + main hook + 发布 CLI，步骤比官方 updater 多；但没有自定义 trait |
| 配置复杂度 | 4.0 | 字段多、部分死配置（patch_memory_factor）、无 JSON Schema、错误会阻止启动 |
| API / CLI 易用性 | 5.0 | 命令名清晰、TS 类型友好；CLI 缺 JSON/进度/安装说明，`prepare` busy 返回 Ok，`--dry-run` 不产物 |
| 学习曲线 | 4.0 | 必须理解 manifest/journal/rollback/签名/提权；文档长且部分错误，增加认知负担 |
| 错误提示 / 日志 / 可观测性 | 3.5 | 错误码/action 设计好；默认无日志、`detail` 空、worker stderr 丢弃、缺少结构化上下文 |
| 调试体验 | 4.5 | `inspect` 是好工具；但 inspect 有状态 bug、无真实 worker e2e、无日志、复现门槛高 |
| 示例与文档质量 | 4.0 | 示例最小可用，但 `confirm` 1 秒、bundle false、auto_apply true；文档存在幻影 API |
| 发布 / 升级流程 | 4.5 | 原子发布顺序好；无多平台/多通道布局、无缓存头、无上传重试/进度、verify 有盲区 |
| CI/CD 集成 | 4.0 | CLI 可在 CI 跑；无官方 Action/Docker/JSON 输出；300s 上传超时；密钥注入需手写 |
| 依赖与版本兼容 | 4.0 | MSRV 1.90 偏高；Tauri 依赖写 "2"；技术栈文档过期；未发布、无 semver 检查 |
| 迁移与维护成本 | 3.5 | 自定义协议/对象布局；文档漂移；公开 API 面大；状态机 bug 会迫使升级 |
| 社区生态 / 长期风险 | 2.5 | 0.1.0 未发布、无 repository/社区入口、Windows-only、官方 updater 竞争、bus factor |

**关键 DX 断点**

- “照文档三步接入”不成立：第四步 `main` hook 缺失会导致应用启动失败，且文档没有把它放在最显眼位置。
- “照 init 输出配置”会让应用启动失败（占位公钥非法）。
- 没有日志：默认 `tracing` 无 subscriber，开发者看不到任何内部状态；这会让第一次接入几乎不可调试。
- 错误会阻止应用启动，而不是降级；这是最伤 DX 的设计之一。
- `inspect` 是亮点，但 `Trash` 状态路径 bug 和忽略 rollback journal 会误导排障。

**最佳方案**

- 一行接入：`delta_update::run(tauri::Builder::default()).plugin(...)` 或宏，自动处理 worker 参数。
- `delta-update init --write`：幂等合并配置/capability，拒绝占位值，输出“下一步命令”。
- 启动失败降级 + 内置日志初始化（可选 feature）。
- 提供官方 GitHub Action / Docker 镜像 / 模板仓库。
- CLI 增加 `--json`、进度条、`--max-memory`、`--retry`、`--concurrency`；`verify` 默认用公钥。
- 生成 JSON Schema 与 TS 类型；编辑器自动补全配置。
- 文档首屏放“5 分钟最小可运行”与“已知限制”，把设计长文放到后面。

### 3.13 用户体验 / 产品完整度（5.0/10）

**不足**

- 没有“发现更新”状态/用户同意：`auto_check` 直接后台下载，流量敏感用户无法选择“仅 Wi-Fi/稍后/查看说明”。
- 进度粒度过粗：大文件下载/校验时长时间无变化；`Committing` 无进度；`Preparing` 只按文件数。
- 健康确认默认过早（P0-06），用户可能被留在坏版本。
- `mandatory` 只是透传，引擎/插件不强制；是否阻止跳过完全由 UI 决定，容易实现错误。
- 回滚提示会被下一次成功 check 清掉（`resting` 回 Idle），用户可能不知道发生过回滚。
- `restart_to_apply` 触发后应用立即退出，前端无法展示“正在重启”；需要文档/产品设计。
- 没有更新历史、失败原因详情（`detail` 空）、没有“复制诊断信息”按钮/命令。
- 没有带宽限制/调度/夜间更新；没有多语言 `user_action`（固定中文）。
- 没有“跳过此版本/稍后提醒”；`skipped_version` 只在回滚后自动写，用户无法主动跳过。
- per-machine 首次启动 UAC 恢复会打断启动，缺少解释性 UI。

**最佳方案**

- 增加 `Checking → Available → Downloading` 三段式，允许“立即/稍后/跳过/查看说明”；`auto_check` 只检查不下载，`auto_download` 单独开关。
- 进度细化到字节/阶段；`Committing` 提供真实进度或至少 indeterminate 状态。
- `confirm_healthy` 只在应用自定义健康检查通过后调用；提供 `HealthStatus` 事件。
- 回滚状态持久化到下一次用户确认；不要被普通 check 清掉。
- 错误 payload 提供 `detail`/诊断 ID；前端可一键复制 `status + inspect`。
- 增加带宽限制、仅 Wi-Fi、夜间窗口（可选）。
- `mandatory` 在 engine 暴露 `update_required` 语义，提供 UI 约定；不要只在文档里说“由 UI 决定”。

### 3.14 社区生态与长期演进（2.5/10）

**不足**

- 0.1.0 未发布 crates.io/npm，无 repository/homepage，外部开发者无法 `cargo add`/`pnpm add` 即用；README 的安装命令目前无效（插件 README 已注明）。
- 只有中文文档；国际社区无法参与。
- Windows-only 实际能力与跨平台文档冲突，会消耗早期用户信任。
- 与官方 `tauri-plugin-updater` / `tauri-plugin-updater-delta` 竞争；本项目优势是“文件级、免安装器、回滚”，但接入成本、平台覆盖、维护成本更高。
- 自定义 manifest/journal/对象布局形成迁移锁定；没有从官方 updater 的迁移工具（`init` 只复用 pubkey）。
- 没有公开 roadmap、版本支持策略、弃用政策、安全公告流程；社区无法判断长期维护承诺。
- 代码/文档漂移和幻影 API 会提高贡献门槛；潜在贡献者难以判断哪些是真实实现。
- MSRV 1.90 偏高，企业/CI 采用门槛高。
- 单一维护者/小团队风险（无 CODEOWNERS、无发布审批记录）。

**最佳方案**

- 先发布 0.1.0 到 crates.io/npm，补 repository/homepage/badges；提供 GitHub Discussions/Issues 模板。
- 明确 1.0 支持矩阵：Windows 10+ per-user/per-machine；其他平台明确“不支持/规划中”。
- 提供迁移指南：官方 updater → delta-update（pubkey、安装形态、回滚策略）。
- 建立 roadmap 与弃用政策；每个版本 changelog 与升级说明。
- 增加英文 README/快速开始；文档站点（mdBook/Docusaurus）与版本化。
- 用 `cargo-semver-checks` 和 API 稳定性承诺降低下游维护成本。

---

## 4. 真实接入与升级场景演练

| # | 场景 | 预期 | 实际/代码路径 | 结论 |
|---|---|---|---|---|
| S1 | 新 Tauri 应用按 `init` 输出配置，使用占位公钥启动 | 应用可启动，更新状态 Failed | `init` 输出非法公钥 → `Config::validate` 失败 → Tauri setup 返回 Err → 应用启动失败 | **P0** |
| S2 | 只做了“注册插件 + 配置 + 权限”，忘记在 main 调 `run_applier_from_args` | 更新不可用但应用可用 | plugin setup 返回 `E_CONFIG_INVALID`，应用启动失败 | **P0/DX** |
| S3 | Windows per-user，v1→v2，auto_check/auto_apply/auto_confirm 默认 | 后台下载，退出替换，下次启动 v2，3 秒确认 | e2e 正常路径通过；但 health 确认过早，坏更新风险 | 可用，P0-06 |
| S4 | 用户从 v1 直接升到 v3（跳过 v2） | 成功更新或提示可修复 | v2→v3 未变化文件无 full_key；本地哈希不同 → `E_MANIFEST_PARSE` | **P0-03** |
| S5 | 用户本地一个未变化文件被杀毒软件隔离/损坏 | 自动修复或失败可恢复 | `prepare` 的 unchanged 重哈希失败 → `E_PATCH_SOURCE_MISMATCH`，无 full 修复 | **P0-03** |
| S6 | 回滚执行到一半断电，重启 | 继续回滚并清理，旧版本可用 | 恢复分支删除 rollback.journal 后返回 RolledBack，但 commit.journal/status 保留；下一次启动报 Committed（新）而文件是旧的 | **P0-02** |
| S7 | per-machine 安装，本地用户伪造 apply.json + rollback.journal | 提权 worker 拒绝 | worker 只校验路径包含关系，journal checksum 无密钥，Place 哈希来自 journal；可写入安装根祖先 | **P0-01** |
| S8 | 私有桶，>8MiB 补丁，下载中预签名 URL 过期 | 自动刷新 URL 并续传 | Range 分块 403 后不刷新，重试同一过期 URL，最终 `E_NETWORK_HTTP_4XX` | **P1** |
| S9 | 上次下载留下同 size 的损坏全量文件 | 删除并重下 | Range 从 EOF 开始 → 416 → 非可重试 → prepare 失败 | **P1** |
| S10 | `.old` 备份被磁盘/杀毒损坏，触发回滚 | 拒绝回滚并报 E_ROLLBACK_FAILED | inverse 用损坏备份现算哈希，Restore 校验通过，恢复损坏旧版本 | **P0-07** |
| S11 | macOS 签名 .app 安装后接入 | 明确拒绝（E_UNSUPPORTED_FORMAT） | `ensure_supported` 返回 Ok；install_root=Contents/MacOS；资源不在根内；替换破坏签名 | **P0-04** |
| S12 | Linux AppImage 接入 | 明确拒绝或走 AppImageUpdate | `needs_elevation=false`；worker 写只读挂载失败；无格式门禁 | **P0-04** |
| S13 | 两实例同时准备并退出 | 串行/拒绝第二个 | `begin_commit` 锁在写 journal 后释放；第二个可覆盖 apply.json/journal；worker commit_id 不匹配 | **P1** |
| S14 | 开发者配置 `install_dir` 指向父目录/错误目录 | 启动时拒绝 | 插件只校验绝对路径/存在；不校验包含 exe（`resolve_install_root` 未使用）；auto_apply 可能替换错误目录 | **P0/P1** |
| S15 | CLI 发布 500MB 对象，CI 上行 20Mbps | 成功上传 | S3/COS 客户端总超时 300s；无 multipart/重试；大概率失败 | **P1** |
| S16 | 使用 `--verify` 自检签名 | 验证客户端信任链 | `--verify` 从私钥派生公钥，只证明自洽，不证明客户端内嵌公钥信任 | **P1/DX** |
| S17 | 开发者照 API 文档使用 `DeltaUpdaterExt` | 编译通过 | 该 trait 不存在，文档幻影 API | **P1/DOC** |
| S18 | 大文件补丁 apply，内存 256MB 限制 | 按 patch_memory_factor 拒绝或流式处理 | 配置项未使用；实际一次性读 source+patch+解压 payload | **P1** |

---

## 5. 最佳优化方案（按优先级）

### 5.1 第一优先级：安全信任链（阻断发布）

1. **签名清单进入 journal 上下文**：`begin_commit` 同时保存 `manifest.json`、`manifest.json.minisig`、`from-manifest`（回滚用）；worker 验签后才执行。
2. **journal 绑定 manifest_hash / commit_id / install_root_canonical**；每个 `Place`/`Restore` 的 `sha256` 必须来自签名清单。
3. **worker 从自身 exe 推导 install root**；拒绝祖先目录；staging/journal 必须是用户私有目录；用 canonicalize 和 reparse point 检查。
4. **rollback 使用旧签名清单哈希**，备份损坏时拒绝回滚并进入 NeedsAttention。
5. **路径保留目录**：大小写不敏感拒绝 `.old`/`.trash` 首段；所有 journal 路径重新校验；`Journal.version` 必须 SemVer。
6. **网络策略**：重定向逐跳校验 scheme/host/allowed_hosts；签名服务响应限 64KiB；私有桶支持自定义 headers/客户端证书。
7. **测试门禁**：伪造 journal、宽 install_root、symlink/junction、损坏备份、重定向、保留目录。

### 5.2 第二优先级：恢复状态机与数据一致性（阻断发布）

1. 单一 `update.state.json`，状态：`Idle/Preparing/Prepared/Committing/Completed/RollingBack/RolledBack/Failed/Disabled`；所有转换原子持久化。
2. `recover()` 返回前校验文件树；回滚完成在同一事务写 `skipped_version` + 清理所有 commit/rollback/status/prepared/apply 和备份/staging。
3. 为 commit/rollback 每个 op 增加 kill-point 测试；用 property test 验证“任意中断后下一轮要么旧树、要么新树”。
4. 下载层：损坏文件先删除；禁止 `resume_from == expected_size`；Range 403 刷新 URL；全局并发信号量；下载完成 `sync_all`。
5. 单实例：命名互斥量或 `begin_commit` 锁持有到 worker 接管；阻止双实例意图覆盖。

### 5.3 第三优先级：更新数据模型（解决多版本/修复/回滚）

1. **内容寻址 blob**：`blobs/<sha256>` 存所有文件内容；manifest 每条都有 `blob`；`delta_key` 可选。
2. **多基线**：`deltas/<from_sha>-<to_sha>` 或清单 `from_sha` 索引；客户端按本地文件哈希选择。
3. **修复模式**：本地不匹配时直接拉 blob，不报错；`check` 报告 `repair_available`。
4. **旧清单保留**：至少保留最近 2–3 个签名清单，供回滚/修复；发布生命周期规则保护。
5. **全量压缩**：blob 可用 zstd/gzip 容器或 CDN Content-Encoding；reduce added-file traffic。

### 5.4 第四优先级：性能与资源

1. `VerifiedInventory` 复用哈希；check 一次、prepare/commit 只验证变化文件；staged 哈希只算一次。
2. 并行哈希（有界 rayon）；下载全局信号量 + 限速；文件内/阶段进度。
3. 补丁 apply 流式化或严格执行 `patch_memory_factor`；CLI 补丁候选落盘 + 内存预算 + `--max-memory`。
4. HTTP ETag/304、Cache-Control、CDN；manifest 紧凑 JSON；可选 gzip。
5. CLI 上传并行/重试/multipart/长超时；`--json`/进度条。

### 5.5 第五优先级：平台与接入体验

1. **Windows-only 1.0**，格式门禁在 setup/下载前返回 `E_UNSUPPORTED_FORMAT`；文档统一。
2. macOS/Linux 走官方安装器/包管理器；如果未来支持 Unix tar，补 mode/symlink/权限。
3. 提供 `delta_update::run(...)`/宏，消除 main hook 陷阱；`init --write` 幂等合并；配置无效降级 Disabled，不阻止启动。
4. 默认 `auto_check=true`、`auto_apply=false`、`auto_confirm=false`；健康检查注册接口。
5. 默认权限拆分 read/manage；高风险命令显式授权。
6. 内置可选日志初始化；错误 `detail` 结构化；worker 日志落盘；`inspect` 修复 bug。

### 5.6 第六优先级：文档与工程

1. 实现状态矩阵；删除幻影 API；从 rustdoc 生成 API 参考；文档示例做 doctest/JSON 校验。
2. 技术栈从 Cargo 元数据生成；平台矩阵统一；CHANGELOG 每版本。
3. 英文 README/quickstart；repository/homepage/bugs；发布 0.1.0 到 crates.io/npm。
4. CI：Windows/Ubuntu/macOS + MSRV + coverage + fuzz + semver-checks；release dry-run/tag/checksums/SBOM。
5. 清理生成物（`项目完整源码.md`、`导出源码.bat`）和死代码/死配置。

### 5.7 发布门禁（建议）

- [ ] 伪造 journal + 宽 install_root 的 E2E 失败（安全）
- [ ] 回滚中断 kill-point 测试通过，目录状态可判定（可靠性）
- [ ] vN-2→vN 升级与本地损坏修复测试通过（可用性）
- [ ] macOS/Linux 不支持形态在下载前返回 `E_UNSUPPORTED_FORMAT`（平台）
- [ ] 配置错误不阻止应用启动（DX）
- [ ] 默认 auto_confirm=false，健康检查由应用显式调用（产品）
- [ ] 回滚备份损坏时拒绝并报 `E_ROLLBACK_FAILED`（可靠性）
- [ ] 真实 worker 进程 E2E 在 Windows CI 通过（测试）
- [ ] 文档 API 示例编译/解析通过，平台矩阵与代码一致（文档）
- [ ] `cargo semver-checks`、覆盖率、fuzz smoke、发布 dry-run 通过（工程）

---

## 6. 附录

### A. 高优先级问题索引

| ID | 优先级 | 问题 | 证据 |
|---|---|---|---|
| P0-01 | P0 | journal/apply.json 未认证 + install_root 过宽 | `apply/applier.rs:135-165`、`journal/format.rs:95-97`、`journal/replay.rs:122-171`、`docs/design/permission-model.md:54,218` |
| P0-02 | P0 | 回滚中断恢复不清理 commit 状态 | `recovery.rs:61-93`、`recovery.rs:101-120`、`rollback/mod.rs:39-48` |
| P0-03 | P0 | 未变化文件无 full_key，跨版本/修复不可用 | `pipeline/mod.rs:182-240`、`manifest/diff.rs:99-105`、`release_pipeline.rs:149` |
| P0-04 | P0 | 平台声明与实现不符；无格式门禁；Unix 权限丢失 | `desktop.rs:4-17`、`applier.rs:112-115`、`model.rs:147`、`api.md:803-815` |
| P0-05 | P0 | 配置校验失败阻止应用启动；init 占位值非法 | `plugin/lib.rs:32-36`、`config.rs:90-229`、`init.rs:10-12,77-80` |
| P0-06 | P0 | auto_confirm 默认 true + 示例 1s 无条件确认 | `config.rs:19-24`、`lifecycle.rs:238-269`、`examples/.../update.ts:36` |
| P0-07 | P0 | 回滚用备份自身哈希，损坏备份被接受 | `rollback/inverse.rs:27-56,77-89`、`journal/replay.rs:151-171` |
| P1-01 | P1 | 重定向不校验 scheme/host | `fetch/http.rs:118-129,274-290` |
| P1-02 | P1 | 符号链接/junction 未防护 | `path/guard.rs:12-20`、`engine.rs:520-533` |
| P1-03 | P1 | `.old`/`.trash` 保留目录未拒绝 | `engine.rs:532`、`manifest/parse.rs:60-106` |
| P1-04 | P1 | Range 下载不刷新预签名 URL | `fetch/http.rs:365-421,474-495,784-823` |
| P1-05 | P1 | 同长度损坏文件 416 硬失败 | `staging/prepare.rs:439-519`、`fetch/http.rs:556-639` |
| P1-06 | P1 | hash cache 可导致错误“无更新”/卡死 | `engine.rs:541-553`、`hash_cache.rs:66-71` |
| P1-07 | P1 | 多实例 begin_commit race | `engine.rs:286-323`、`apply/writer.rs:33` |
| P1-08 | P1 | I/O 放大（4–5 次全文件读取） | `staging/prepare.rs:545-563`、`staging/verify.rs:7-30`、`engine.rs:258-264,286-297`、`journal/replay.rs:133-140` |
| P1-09 | P1 | patch_memory_factor 未使用；apply 非流式 | `patch/apply.rs:28-47`、`limits.rs:10-25` |
| P1-10 | P1 | CLI 全内存补丁 + 默认 jobs 过高 + 300s 上传超时 | `pipeline/patches.rs:52-74`、`publisher/s3.rs:50-54`、`publisher/cos.rs:31-34` |
| P1-11 | P1 | verify 不校验 patched 条目的 full 回退内容 | `pipeline/mod.rs:651-686` |
| P1-12 | P1 | install_dir 不校验包含 exe；resolver 未使用 | `lifecycle.rs:39-46`、`path/resolver.rs:9-37` |
| P1-13 | P1 | 默认无日志；detail 空；worker stderr 丢弃 | `error.rs:427-437`、`applier.rs:89-104` |
| P1-14 | P1 | 文档幻影 API / 平台矛盾 / 版本过期 | `api.md:328-350,540-557`、`modules.md:85-90`、`tech-stack.md:31-38,221` |
| P1-15 | P1 | e2e 未覆盖真实 worker/UAC；测试计划过度声明 | `e2e/harness.rs:265-278`、`lifecycle_contract.rs:169-175` |
| P2-01 | P2 | 无 ETag/304/Cache-Control | `fetch/http.rs:292-358`、`publish.rs` |
| P2-02 | P2 | 无带宽限制；Range×文件并发峰值 16 | `fetch/http.rs:19-25`、`staging/prepare.rs:74` |
| P2-03 | P2 | 无文件内/verify/commit 进度 | `engine.rs:47-60`、`staging/prepare.rs:388-403` |
| P2-04 | P2 | 清理不完整（历史 staging/.old） | `engine.rs:383-391`、`fs/mod.rs:29-39` |
| P2-05 | P2 | Journal 32MiB 与 max_files 最坏情况不匹配 | `journal/format.rs:9-10`、`manifest/parse.rs:9-10` |
| P2-06 | P2 | move_path 非原子替换；Place 失败源丢失 | `journal/replay.rs:222-273,133-140` |
| P2-07 | P2 | 签名服务响应无上限；headers 不到签名服务 | `fetch/signing.rs:80-87`、`fetch/http.rs:292-358` |
| P2-08 | P2 | 权限粒度过粗（restart/prepare） | `permissions/default.toml:1-10` |
| P2-09 | P2 | 状态机未接入；Committing/RollingBack 无意义 | `state/machine.rs:78-117`、`commands.rs:72-75` |
| P2-10 | P2 | 公开 API 面过大；死代码/死配置 | `lib.rs:8-24`、`Cargo.toml:13-15` |
| P2-11 | P2 | 重复 TS/Rust 类型；无 schema 生成 | `commands.rs:21-68`、`types.ts:19-87` |
| P2-12 | P2 | 示例 auto_apply/bundle/confirm 默认危险 | `tauri.conf.json:24-37`、`update.ts:36` |
| P2-13 | P2 | 无 repository/homepage；中文-only；无 rustdoc | `Cargo.toml`、`package.json`、README |
| P2-14 | P2 | CI 无 macOS/MSRV/覆盖率/fuzz；release 脆弱 | `.github/workflows/ci.yml`、`release.yml` |
| P2-15 | P2 | manifest 允许 Unicode 大小写/规范化冲突 | `manifest/parse.rs:62-65`、`path/rel_path.rs:78-89` |
| P2-16 | P2 | 对象键含 `#`/`%` 时 URL join 错误 | `fetch/http.rs:262-271`、`path/rel_path.rs:78-89` |

### B. 本地执行记录

```
cargo test --workspace --quiet
  -> 全部测试通过（约 182 个）

cargo test -p delta-update-engine -p delta-update-cli --quiet
  -> 本地 crate 清理后 159 个测试全部通过

cargo run -q -p delta-update-e2e
  -> PASS fresh_install
  -> PASS delta_update
  -> PASS crash_loop_rollback
  -> PASS uac_cancelled
  -> PASS cross_volume
  -> PASS rollback_release

cargo xtask docs-check
  -> docs links: ok
```

> 说明：首次 e2e 运行复用了跨目录的旧 `target`，`cross_volume` 输出中的 staging 路径落在旧目录 `D:\Desktop\tauri-delta-updater\e2e\fixtures`；清理本地 crate 后重新运行，路径已回到当前仓库，6/6 仍通过。该现象不是产品缺陷，但说明“移动仓库后直接复用 target”会嵌入旧 `CARGO_MANIFEST_DIR`，CI/发布必须 clean build。

### C. 评审局限

- 未在真实 macOS/Linux 机器上执行文件替换；结论来自源码路径、平台 API 语义与文档对照。
- 未执行真实 UAC 提权（需要交互式管理员环境）；P0-01 的提权影响按 Windows UAC 语义推演。
- 未执行真实 COS/S3/R2 发布；CLI 签名/上传逻辑按单元测试与代码审阅。
- 未做长时间/大规模性能压测；性能结论来自代码路径与基准说明。
- 未审计所有依赖的已知漏洞；`cargo-deny` 已覆盖 advisories/licenses/bans/sources。
