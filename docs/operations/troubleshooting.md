# Delta Update Engine 运维与故障排查手册

> 本文面向技术支持、运维人员与需要自助排查的高级用户。当用户报告“更新坏了”时，这里定义去哪里找证据、每个状态与错误码意味着什么、哪些问题可以自助修复、哪些必须收集信息后上报。故障排查的目标不是“绕过问题让更新成功”，而是**准确判断问题属于哪一层，并给出与该层匹配的处置动作**。


## 1. 日志与诊断信息的位置

### 1.1 插件日志：只发事件，不落盘

插件本身**不创建日志文件**。它通过 `tracing` 发出事件，target 为 `delta_update_engine`、`tauri_plugin_delta_update` 等；是否输出、写到哪里，由宿主应用安装的 subscriber 决定。

排查时在 `main` 最前面初始化一个 subscriber（`tracing-subscriber`）：

```toml
[dependencies]
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

```rust
fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "delta_update_engine=info,tauri_plugin_delta_update=info".into()
            }),
        )
        .init();

    let context = tauri::generate_context!();
    if let Some(code) = tauri_plugin_delta_update::run_applier_from_context(&context) {
        std::process::exit(code);
    }
    // ... tauri::Builder::default().run(context)
}
```

需要更详细的信息时用 `RUST_LOG=delta_update_engine=debug,tauri_plugin_delta_update=debug` 临时提高级别。

**使用 `tauri-plugin-log`**：它捕获的是 `log` crate。需要同时启用 `tracing` 的 `log` 特性（或安装 `tracing-log` 桥接），本插件的 `tracing` 事件才会进入日志文件；使用它时日志通常位于 `%LOCALAPPDATA%\{bundleIdentifier}\logs\`。

**日志内容**：状态迁移、版本、下载/暂存数量、恢复与回滚决策。不会记录文件内容或私钥；路径可能包含用户名，上报前请脱敏。

### 1.2 更新专用的数据目录

除日志外，插件在**应用数据目录**下维护两个与更新状态直接相关的目录：

```
%LOCALAPPDATA%\<bundleIdentifier>\delta-update\
├── staging/
│   └── <version>/                 # 只存放变化/新增文件的目标版本
└── journal/
    ├── update.lock                # 跨进程更新锁
    ├── apply.json                 # 替换进程的路径配置
    ├── prepared.json              # 已就绪的暂存计划
    ├── attempts                   # 启动尝试计数
    ├── skipped_version            # 回滚后跳过的版本
    ├── known_files.json           # 历史官方文件集合
    ├── hash_cache.json            # 安装根文件哈希缓存
    ├── old_hashes.json            # 提交前旧文件哈希快照（回滚校验）
    ├── commit.meta.json           # journal ↔ 签名清单绑定
    ├── verified_manifest.json(.minisig)  # 最近验签通过的清单
    ├── installed_manifest.json(.minisig) # 已确认健康版本的清单
    ├── worker.log                 # 替换/恢复 worker 的执行记录（成功与失败都追加）
    ├── commit.status              # 替换进程执行结果
    ├── commit.journal             # 提交日志（内部含 commit_id）
    └── rollback.journal           # 回滚逆日志
```

`staging/` 存放 prepare 阶段物化的变化文件；未变化文件不复制，prepare 时在安装目录原地校验。`journal/` 存放提交日志、状态文件与跨进程锁。**这些文件/目录的存在与内容，是判断更新处于哪个阶段的最直接物理证据**。

`worker.log` 每行是一个 `event=` 事件加若干 `key=value` 字段，第二个字段固定为 `ts=<unix 秒>`（用于和 Windows 事件日志、杀毒软件隔离记录对齐），值含空格或引号时才加双引号：成功 `event=apply_ok ts=<n> commit=<id> ms=<n> version=<v> retries=<n>` / `event=recovery_ok ts=<n> ms=<n> retries=<n>`，失败 `event=apply_failed ts=<n> commit=<id> ms=<n> retries=<n> msg=...` / `event=recovery_failed ...`，启动时先写 `event=apply_started ts=<n> commit=<id> journal=<path> exe=<path> form=<per-machine|per-user|portable>` / `event=recovery_started ...`。`form` 是从可执行文件位置推断的安装形态，只用于诊断：per-machine 失败通常意味着提权路径有问题。`prepare` 阶段每个补丁回退追加 `event=patch_fallback ts=<n> path=<path> code=<code>`。`retries` 是该进程本次提交/恢复中文件占用重试的次数，**成功与失败都会记录**，因此验收时用 `retries ≥ 1` 判断“重试确实发生过”，用 `retries = 0` 判断“没有重试”。旧版本写的纯文本行（`apply commit <id> ok in ...`、`patch fallback: <path>: <code>`）仍然可读，`inspect --bundle` 两种都解析。

**用 `inspect --bundle` 读取**：把诊断包和远端清单一起传给它，会输出“文件 × manifest 策略 × journal 操作 × 回退证据”的对照表，不需要手工 grep：

```bash
delta-update inspect --bundle delta-update-diagnostics-<ts>.json \
  --from-release cos://your-bucket --version 2.0.0 --public-key release.pub
```

**为什么放在 Local 而不是 Roaming**：暂存区可能是数百 MB 甚至 GB 级，放在漫游配置目录会触发不必要的同步，拖慢登录并放大磁盘占用。

### 1.3 安装目录定位

安装目录**只以当前运行 exe 所在目录为准**；插件不读取注册表，也不接受“包含 exe 的任意祖先目录”。生产构建中 `install_dir` 必须精确等于 exe 父目录，否则更新功能会禁用并报 `E_CONFIG_INVALID`。

手工排查时可对照注册表卸载项（仅供参考，不是信任源）：

```
HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\<AppName>\InstallLocation
HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\<AppName>\InstallLocation
```

**注册表不是信任源**：如果它指向另一个目录，以当前 exe 实际位置为准；先确认启动的是哪一份 exe。

### 1.4 离线诊断：`delta-update inspect`

在用户机器上收集到 journal 目录后，用 CLI 直接解码，不需要启动应用：

```bash
delta-update inspect --journal-dir "%LOCALAPPDATA%\com.example.app\delta-update\journal"
delta-update inspect --journal-dir ... --verbose
delta-update inspect --journal-dir ... --json > update-report.json
delta-update inspect --journal-dir ... --export support-bundle.json
```

输出包括：`apply.json` 配置、`commit.status`、`commit.journal` / `rollback.journal` 的版本与操作统计、`prepared.json`、启动尝试次数、跳过版本，以及一句结论（未完成提交 / 等待健康确认 / 失败重试 / 已准备 / 已回滚）。`--verbose` 会逐条列出 `FileOp` 及其当前状态（`landed` / `pending` / `backed_up`）。损坏的状态文件会列为警告而不是让命令崩溃；上报时附上该输出与目录文件列表即可。

`--export <PATH>` 额外写一个**单文件诊断包**（JSON）：上面的报告、`worker.log` 最后 64 KiB、journal 目录的文件名与大小、系统信息（Windows 版本、安装形态）与 CLI 版本。它不包含文件内容，可直接附到工单；单文件比目录更容易让用户上传。加 `--redact` 会把 `install_root`、`staging_root`、`journal_root` 与用户主目录前缀替换为 `<install_root>` 等占位符，并在 `redacted` 字段标记为 `true`。

**用户自助导出**：应用可以调用 `plugin:delta-update|export_diagnostics`（前端封装为 `exportDiagnostics()`），把同样的诊断包写到应用日志目录并返回路径。这不需要用户找到 journal 目录，也不需要安装 CLI；适合做成“导出诊断”按钮。该命令**始终脱敏**。

**注意日志目录可能被同步**：企业环境常把 `%LOCALAPPDATA%` 下的日志纳入 OneDrive/SharePoint 同步。诊断包包含安装路径（`--redact` 可去除）与日志尾部，附到工单前请确认目标目录不会被自动同步到云端。

**退出码**：`0` = 无待处理状态；`1` = 存在未完成/回滚 Journal，重启应用通常可自动恢复或重试；`2` = Journal/状态文件损坏，需要人工介入。CI 与运维脚本可据此区分“自动修复”和“发告警”。


## 2. 状态机各状态的含义

状态通过 `delta-update://state` 事件和 `status` 命令对外暴露。正常流程中的状态停留时间有预期范围，超出范围即值得关注。

| 状态          | 含义                                   | 正常停留时间         | 异常信号                                                     |
| ------------- | -------------------------------------- | -------------------- | ------------------------------------------------------------ |
| `Idle`        | 无更新活动。                           | 持久状态             | —                                                            |
| `UpdateRequired` | 本地版本低于发布者 `min_supported_version`。 | 持久状态 | **不是故障**。应用可选择限制功能；检查 `min_supported_version` 是否发布者写错，或确认更新为何未准备完成 |
| `Checking`    | 正在获取并验签清单。                   | 秒级                 | 超过 30 秒未迁移，检查 `endpoint` 可达性或验签耗时           |
| `Downloading` | 正在下载补丁或全量对象。               | 取决于网络与体积     | 长时间停在相同 `progress.completed` 值，可能网络中断或 URL 过期 |
| `Preparing`   | 正在将补丁应用到暂存区并逐文件校验。   | 取决于文件数量和大小 | 长时间无进展，可能磁盘 I/O 阻塞或内存不足                    |
| `Prepared`    | 暂存计划已就绪，等待退出时应用。       | 可持久停留           | **这是期望的终态之一**。如果一直停留在此且退出后仍是旧版本，问题在替换进程侧 |
| `Committing`  | 替换进程正在执行 rename-then-replace。 | 秒级到分钟级         | **主进程通常已退出，此状态只在替换进程日志中可见**；长时间停留可能文件被锁定或权限不足 |
| `Committed`   | 替换已完成，等待 `confirm_healthy()`。 | 直到确认或回滚       | 如果新版本启动后仍为 `Committed` 且未确认，检查 `auto_confirm` 或前端调用 |
| `RollingBack` | 正在执行回滚。                         | 秒级到分钟级         | 失败返回 `E_ROLLBACK_FAILED`，需人工介入                     |
| `RolledBack`  | 回滚完成，以旧版本运行。               | 直到下一次检查更新   | 持续停留在 `RolledBack` 且反复尝试同一远程版本，检查回滚后跳过逻辑 |
| `Failed`      | 更新流程失败。                         | 直到下一次尝试       | 根据 `error.category` 和 `error.code` 判断严重程度           |

**关键诊断规则**：如果状态在 `Prepared` 之后、用户退出之前一直不变，这是**正常行为**——更新已经准备好，等待退出时由替换进程应用。用户退出再启动后如果版本未变，问题出在**替换进程**（未能启动、被安全软件拦截、UAC 被取消、权限不足），而不是下载或 prepare 阶段。若替换进程在写完 Journal 后被 kill，下一次启动的 `recover_and_cleanup()` 会重放并续完。


## 3. 错误码到用户可操作建议的映射

错误码的结构和完整表定义在协议文档 §7.3。本节给出**面向用户和一线支持**的处置映射。

### 3.1 网络类（`E_NETWORK_*`）

| 错误码                        | 用户可见含义         | 建议动作                                                     |
| ----------------------------- | -------------------- | ------------------------------------------------------------ |
| `E_NETWORK_CONNECT`           | 无法连接更新服务器   | 检查网络连接；如果使用代理，确认代理允许访问 COS             |
| `E_NETWORK_TIMEOUT`           | 下载超时             | 稍后重试；弱网环境下属预期，插件会自动重试                   |
| `E_NETWORK_HTTP_5XX`          | 服务器暂时不可用     | 稍后重试，无需用户操作                                       |
| `E_NETWORK_HTTP_4XX`          | 请求被拒绝           | 检查 `endpoint` 和对象键是否正确；非 403 的 4xx 通常意味着配置错误 |
| `E_NETWORK_PRESIGN_EXPIRED`   | 下载链接过期         | 插件应自动刷新并续传；如果持续出现，检查签名服务的有效性和本机时间 |
| `E_NETWORK_RANGE_UNSUPPORTED` | 服务器不支持断点续传 | 插件会回退到全量重下；告知用户更新可能比预期慢               |

**COS 403 专项**：`E_NETWORK_HTTP_4XX` 或 `E_NETWORK_PRESIGN_EXPIRED` 在 COS 场景下最常见的原因是签名过期或本机时间偏差。排查时先确认**业务是否允许签发、临时凭据是否有效、实际请求是否与签名条件一致**，不要一遇到 403 就延长有效期或把桶改成公有读。如果重新生成签名仍报 403，检查用户机器的系统时间是否准确（是否开启 NTP 同步），以及代理是否改写了请求头。

**发布端时间偏差**：`delta-update release` 上传对象时若 COS/S3 返回 403 且响应体含 `RequestTimeTooSkewed` / `Request has expired`，是**发布机**时钟偏差（COS 默认容忍约 15 分钟）。同步 NTP 后重试即可；不要通过放宽桶权限或放宽签名有效期来绕过。

### 3.2 签名与清单类（`E_SIGNATURE_*`、`E_VERSION_*`、`E_MANIFEST_*`）

| 错误码                       | 含义                       | 建议动作                                                     |
| ---------------------------- | -------------------------- | ------------------------------------------------------------ |
| `E_SIGNATURE_INVALID`        | 清单验签失败               | **不要重试**。检查客户端配置的 `signing_public_keys` 与发布端使用的私钥是否配对；若刚做过密钥轮换，确认客户端版本是否已内嵌新公钥 |
| `E_SIGNATURE_NO_TRUSTED_KEY` | 无匹配的受信公钥           | 同上；如果确认公钥正确，检查签名文件是否完整上传             |
| `E_SIGNATURE_SIG_MISSING`    | 签名对象缺失               | 检查 COS 上 `latest.json.minisig` 是否存在                   |
| `E_VERSION_ROLLBACK`         | 远程版本低于本地且未授权降级 | 通常不需要用户操作。如果持续出现且用户确信有更新，检查发布端的版本号是否严格递增 |
| `E_MANIFEST_SCHEMA_TOO_NEW`  | 清单格式版本高于客户端支持 | 用户需要更新到支持新 schema 的客户端版本；发布端应考虑 `min_client_schema` 的设置 |
| `E_MANIFEST_PARSE`           | 清单 JSON 解析失败         | 检查 `endpoint` 返回的是否为合法 JSON；可能是 CDN 或代理返回了错误页面 |

签名类错误**不可自动重试**，因为它们不因时间或网络变化而消失。支持人员应优先核对密钥配对和签名流程。

**轮换期间的 `E_SIGNATURE_NO_TRUSTED_KEY` 是预期的**：桥接版本尚未覆盖到的旧客户端只信任旧公钥，自然会拒绝新公钥签名的清单。它不是攻击信号——确认发布端按 [密钥轮换 runbook](release.md#134-密钥轮换的当前边界) 操作，等待客户端更新即可；不要因此回退签名密钥。

**`fetch_signature` 返回 `E_NETWORK_HTTP_4XX`**：说明客户端无法读取 `.minisig` 对象。按顺序检查 (a) bucket policy / 对象 ACL 是否允许读取 `latest.json.minisig`，(b) 使用预签名地址时 `signing_endpoint` 是否配置正确且未被改写，(c) 对象键是否与 `release --prefix` 一致。配置了 signing service 时 403 会自动刷新重试，通常不会以该错误暴露给用户。

### 3.3 路径与安全类（`E_PATH_*`）

| 错误码           | 含义                         | 建议动作                                                     |
| ---------------- | ---------------------------- | ------------------------------------------------------------ |
| `E_PATH_ESCAPE`  | 清单中的相对路径逃逸安装目录 | **安全事件**。检查发布流程是否被篡改；不要继续尝试更新，收集日志并上报 |
| `E_PATH_INVALID` | 路径不符合协议规范           | 同上；通常意味着清单生成侧存在 bug                           |

### 3.4 磁盘与权限类（`E_DISK_*`、`E_PERMISSION_*`）

| 错误码                       | 含义               | 建议动作                                                     |
| ---------------------------- | ------------------ | ------------------------------------------------------------ |
| `E_DISK_INSUFFICIENT`        | 磁盘空间不足       | 用户清理磁盘空间后重试；插件会在下载前预检，通常在开始下载前就报错 |
| `E_DISK_STAGING_LIMIT`       | 暂存区超过配置上限 | 检查 `staging/` 大小；可能是历史残留未清理，见 §5 手动清理 |
| `E_PERMISSION_DENIED`        | 写入被拒绝         | 如果安装在 `Program Files`，确认替换进程或启动期恢复的 UAC 授权是否被拒绝；**不要让主进程整体以管理员运行**；检查安全软件是否拦截 |
| `E_APPLY_HELPER_FAILED`      | 替换进程未能启动或异常退出 | 检查安全软件是否拦截同一二进制的 `--delta-update-apply` 或 `--delta-update-recover` 模式；保留的 PREPARED/Journal 可重试 |
| `E_APPLY_RECOVERY_TIMEOUT`   | 提权恢复等待超时，worker 可能仍在运行 | **重启应用**让下一次启动幂等收敛；不要手动删除 Journal 或 `.old` |
| `E_UPDATE_LOCKED`            | 另一个进程正在提交或恢复 | 等待对方完成；若长期占锁，检查是否有残留进程 |
| `E_ENDPOINT_INSECURE`        | 配置的更新地址不是 HTTPS | 修正 `endpoint` / `signing_endpoint`；生产构建禁用 `allow_insecure_http` |
| `E_UNSUPPORTED_PLATFORM`     | 当前平台/安装形态不受支持 | 插件仅支持 Windows 10+ 桌面；macOS/Linux/移动端改用官方安装器或包管理器 |
| `E_PERMISSION_UAC_CANCELLED` | 用户取消了 UAC     | 用户主动取消。提交阶段取消会保留 PREPARED，下次退出应用时重新尝试；启动恢复阶段取消会保留 Journal 与回滚素材，下次启动时重新请求授权 |

权限问题的一个常见混淆来源是：prepare 阶段成功（写入的是用户可写的暂存区），但 Commit 阶段失败（写入的是可能受保护的安装目录）。**看到 `Prepared` 之后、退出应用时出现 `E_PERMISSION_DENIED`，指向的是替换进程的提权问题，而非下载问题**。

**启动阶段弹 UAC 是恢复流程，不是重新下载更新**：只有上次提交/回滚中断、且恢复需要写安装根时才会出现。允许授权后 worker 会完成 Journal 重放或回滚，主窗口随后才创建；取消授权则状态进入 `Failed`，Journal 与回滚素材保留，下次启动会再次请求。不要在用户级删除 `journal/` 或 `.old/` 来“绕过”提示。

**提权恢复等待超时**：如果 UAC 提示停留超过 worker 等待上限，启动会记录 `elevated recovery did not finish` 并继续启动；worker 可能仍在后台运行。`update.lock` 与 Journal 保证下一次启动幂等收敛，无需手工删除任何文件。若长期反复超时，检查安全软件是否拦截提权子进程，或 UAC 是否被策略静默拒绝。

**UAC 使用其他管理员账户时**：journal 位于当前用户的 `%LOCALAPPDATA%`，另一个管理员账户可能没有读取权限，worker 会报 `E_NOT_PREPARED` 或权限错误。应使用同一账户完成提权（split token），不要放开 journal 目录的 ACL。

### 3.5 补丁类（`E_PATCH_*`、`E_MANIFEST_PARSE`）

| 错误码                         | 含义                                     | 建议动作                                                     |
| ------------------------------ | ---------------------------------------- | ------------------------------------------------------------ |
| `E_PATCH_SOURCE_MISMATCH`      | 本地源文件哈希与生成补丁时的源文件不一致 | 插件应自动回退到全量；如果全量也失败，收集信息上报           |
| `E_PATCH_APPLY_FAILED`         | 补丁应用过程失败                         | 回退全量；持续出现说明差分管线或本地文件异常                 |
| `E_PATCH_TARGET_HASH_MISMATCH` | 补丁应用后目标哈希不匹配                 | 回退全量；如果全量仍不匹配，可能是 COS 对象损坏或清单哈希错误 |
| `E_PATCH_MEMORY_BUDGET`        | 内存预算超限                             | 大文件差分时内存不足；可尝试在磁盘空间允许的情况下回退全量，或调整内存预算配置 |
| `E_MANIFEST_PARSE`（该条目缺少 full_key） | 未知策略且没有全量回退 | 发布端应为所有条目生成 `full_key`（CLI 默认 `blobs/<sha256>`）；不要生成客户端未实现的策略 |

**补丁类错误的设计意图是“可回退”** 。大多数 `E_PATCH_*` 都标记为 `retryable: true`，因为插件会自动尝试全量回退。如果回退成功，用户可能甚至不会感知到错误，只在日志中留下一条记录。

### 3.6 Journal 与回滚类（`E_JOURNAL_*`、`E_ROLLBACK_FAILED`）

| 错误码                    | 含义                   | 建议动作                                                     |
| ------------------------- | ---------------------- | ------------------------------------------------------------ |
| `E_JOURNAL_CORRUPT`       | Journal checksum 失败  | **不尝试重放**。检查 `journal/` 目录；如果根目录状态不确定，需要人工判断。收集 Journal 文件并上报 |
| `E_JOURNAL_FORMAT_TOO_NEW` | Journal 格式高于当前客户端支持 | Journal 由更新版本的应用写入，旧客户端不能重放。**升级应用后重启**，让新版本完成恢复；不要删除 Journal |
| `E_JOURNAL_REPLAY_FAILED` | 重放时遇到不可恢复错误 | 同上；通常意味着文件系统状态与 Journal 预期严重不一致        |
| `E_ROLLBACK_FAILED`       | 回滚自身失败           | **最高严重级别**。用户可能处于“新版本不可用、旧版本也回不去”的状态。需要人工介入恢复备份 |

### 3.7 内部类（`E_INVALID_TRANSITION`、`E_INTERNAL`）

这些错误意味着实现中存在未预期的分支或状态机被非法驱动。**不可由用户操作修复**。收集完整日志和状态快照后上报。


## 4. 常见故障的排查路径

### 4.1 用户报告“更新准备了但重启后没变化”

**排查顺序**：

1. **确认退出前状态是否为 `Prepared`**。如果是 `Idle` 或 `Failed`，更新根本没有准备好。
2. **检查退出时替换进程的日志**。应用退出前应 spawn `--delta-update-apply <journal_dir> <commit_id>`；如果替换进程没有启动或被 kill，下一次启动的 `recover_and_cleanup()` 会重放未完成的 Journal。
3. **检查 `journal/` 目录**。如果 Commit 开始后失败，Journal 可能存在，记录了失败的操作。
4. **检查安装目录下的 `.old/<commit_id>/` 目录**。如果旧文件已被移入但新文件未就位，说明 Commit 中途失败。
5. **确认启动的是哪一份 exe**。安装目录以当前 exe 所在目录为准；如果用户从另一个副本启动，更新不会作用于他以为的那份安装。

### 4.2 用户报告“一直提示有更新，但永远装不上”

这通常意味着 prepare 成功、但 Commit 反复失败，或者回滚后跳过逻辑没有生效。

1. 检查错误码：如果是 `E_PERMISSION_DENIED`，问题在提权；如果是 `E_PATCH_*`，问题在差分或文件状态。
2. 检查是否陷入了“更新→崩溃→回滚→再更新”的循环。回滚后插件应跳过同一远程版本一段时间；如果这个逻辑失效，用户会反复收到同一更新。
3. 检查 `confirm_healthy()` 是否被调用。如果前端从未调用，新版本启动后会在达到阈值时被回滚，然后下一次检查又发现同一更新。

### 4.3 文件被锁定导致替换失败

Windows 上 `rename` 或替换失败的一个常见原因是文件被其他进程占用（杀毒软件、资源管理器、遗留句柄）。排查工具：

- **Process Explorer**：以管理员身份运行，按 `Ctrl+Shift+F` 打开 “Find Handle or DLL”，输入被锁定的文件名，查看占用进程。
- **Handle**（Sysinternals）：以管理员命令提示符运行 `handle.exe <文件名>`，列出持有句柄的进程。
- **PowerToys File Locksmith**：右键被锁文件，选择 “Unlock with File Locksmith”，查看并结束占用进程。

**注意**：rename-then-replace 的设计本身可以处理“运行中的 exe/dll 被进程映射”的情况，因为 rename 不破坏内存映像。如果替换仍然失败，锁定源通常不是主进程，而是**安全软件或外部工具**。在排查时优先怀疑杀毒软件的实时扫描。

Journal 重放遇到 OS 错误 32/33（sharing/lock violation）时，`E_JOURNAL_REPLAY_FAILED` 的 `message` 会明确指出“文件正被其他进程占用，常见于杀毒软件实时扫描或旧进程尚未退出；稍后重试”。这属于可重试状态：下一次启动会幂等重放，不需要删除 Journal。

### 4.6 旧版本客户端遇到更高格式的 Journal

用户在更新途中把应用降级回旧版本时，旧版本可能读到新版本写入的 Journal：

- 旧版本返回 `E_JOURNAL_FORMAT_TOO_NEW`（`action = update_client`），**不会**尝试重放，也不会破坏安装目录。
- 正确处理是**先升级回新版本完成恢复**，再执行降级；如果业务上必须立即降级，只能在应用退出后手工移除 `journal/` 与 `.old/`（对应声明中的“手工介入”路径），代价是失去这次提交的回滚素材。
- 不要把 Journal 的 `format` 字段手工改小：校验和会失配，结果退化为 `E_JOURNAL_CORRUPT`。

### 4.7 `apply.json` 损坏

`apply.json` 记录 `install_root`、`staging_root`、`commit_id` 与父进程 PID，是替换进程定位 Journal 的依据。它损坏时：

1. `inspect` 会把 `apply.json` 列为警告并继续输出其余状态；
2. 启动恢复返回 `NeedsAttention`（`E_JOURNAL_CORRUPT`），不会在根因不明时猜测路径；
3. 修复方式是在应用内重新 `check` + `prepare`（会重写 `apply.json`）；若 `commit.journal` 仍存在且安装目录状态不确定，先保留目录并上报，不要手工构造 `apply.json`。


### 4.4 磁盘占用异常增长

`staging/` 在 prepare 成功后应保持到退出后替换完成并确认健康，然后被清理。如果它持续增长：

1. 检查 `confirm_healthy()` 是否被调用。未被确认的更新不会触发暂存区清理。
2. 检查是否有多个版本的暂存目录残留（`staging/<version>/`）。
3. 检查 `.old` 和 `.trash/` 是否在确认健康后未被清理。
4. 如果确认是残留，可按 §5 手动清理，但**必须在应用未运行时进行**。

### 4.5 回滚后用户仍看到同一更新反复出现

回滚模块的设计意图是：回滚后记录 `skipped_version`，在该版本仍是远程最新版本时跳过对它的自动 prepare；远程出现更高版本或 `rollback: true` 清单时清除标记。如果用户仍然被反复提示同一更新：

- 检查 `skipped_version` 是否正确写入与应用（没有时间窗口配置）。
- 检查远程是否发布了**不同版本号但内容相同**的构建——版本单调性只比较版本字符串，如果发布端递增了版本号，客户端会将其视为新更新。


### 4.6 旧版本客户端遇到更高格式的 Journal

用户在更新途中把应用降级回旧版本时，旧版本可能读到新版本写入的 Journal：

- 旧版本返回 `E_JOURNAL_FORMAT_TOO_NEW`（`action = update_client`），**不会**尝试重放，也不会破坏安装目录。
- 正确处理是**先升级回新版本完成恢复**，再执行降级；如果业务上必须立即降级，只能在应用退出后手工移除 `journal/` 与 `.old/`（对应声明中的“手工介入”路径），代价是失去这次提交的回滚素材。
- 不要把 Journal 的 `format` 字段手工改小：校验和会失配，结果退化为 `E_JOURNAL_CORRUPT`。

### 4.7 `apply.json` 损坏

`apply.json` 记录 `install_root`、`staging_root`、`commit_id` 与父进程 PID，是替换进程定位 Journal 的依据。它损坏时：

1. `inspect` 会把 `apply.json` 列为警告并继续输出其余状态；
2. 启动恢复返回 `NeedsAttention`（`E_JOURNAL_CORRUPT`），不会在根因不明时猜测路径；
3. 修复方式是在应用内重新 `check` + `prepare`（会重写 `apply.json`）；若 `commit.journal` 仍存在且安装目录状态不确定，先保留目录并上报，不要手工构造 `apply.json`。


### 4.8 检查更新很慢或日志提示跳过了云占位文件

OneDrive / Files On-Demand 的占位文件在读取时才下载内容。为避免 `check` 触发整树水合，`scan_local` 会检测 `RECALL_ON_DATA_ACCESS` / `OFFLINE` 属性并跳过该文件，同时输出 `skipping cloud placeholder` 警告；`check` 结果的 `cloud_placeholders_skipped` 会给出被跳过的数量，便于 UI 解释下载量。行为是确定的：

- 被跳过的文件**不会**参与本次 diff；不会因此被删除；
- 若清单里仍有该文件，会按缺失文件从内容寻址的全量对象修复（相当于重新下载一份）；
- 需要让该文件参与差异计算时，先让 OneDrive 在本地保留一份（右键 → “始终保留在此设备上”），再重新 `check`。

只检测 `RECALL_ON_DATA_ACCESS` 与 `OFFLINE`，**不检测 `RECALL_ON_OPEN`**：后者可能残留在已完整下载的文件上，用它判断会把本地可读文件误判为占位文件。

这也是诊断包里 `system.install_form` 只能给出启发式分类的原因之一：云同步目录可能不在标准的 `Program Files` / `%LOCALAPPDATA%` 下。


## 5. 手动清理的安全步骤

**重要前提**：手动清理只能在应用**完全退出**后进行。运行中的进程可能持有暂存区或安装目录中的文件句柄，清理可能导致不确定状态。

### 5.1 可以安全清理的残留

| 残留类型                   | 位置                             | 清理条件                                                     |
| -------------------------- | -------------------------------- | ------------------------------------------------------------ |
| 已确认健康后的 `.old/` 目录 | 安装目录 `R` 下                  | 新版本已稳定运行并调用了 `confirm_healthy()`；`.old/<commit_id>/` 是旧版本的备份，不再需要回滚 |
| 已确认健康后的 `.trash/`   | 安装目录 `R` 下                  | 同上                                                         |
| 残留的暂存区               | `%LOCALAPPDATA%\<id>\delta-update\staging\` | 应用退出后；如果当前没有 `PREPARED` 状态需要保留，整个目录可以删除 |
| 残留的 Journal             | `%LOCALAPPDATA%\<id>\delta-update\journal\` | **仅在应用退出后，且你接受“放弃未完成的 Commit”**。删除 Journal 等于放弃重放，安装目录保持当前状态 |

### 5.2 不应手动删除的内容

| 内容                               | 原因                                                 |
| ---------------------------------- | ---------------------------------------------------- |
| 安装目录中的官方文件               | 删除会导致应用无法启动；应通过重新安装或更新流程处理 |
| `latest.json` 或 COS 上的对象      | 服务端资产，删除会影响其他用户的更新                 |
| 正在被 `PREPARED` 状态引用的暂存区 | 删除会丢失已准备好的更新，用户需要重新下载           |

### 5.3 清理暂存区的步骤

1. 确认应用已完全退出（任务管理器中无相关进程）。
2. 导航到 `%LOCALAPPDATA%\<bundleIdentifier>\delta-update\staging\`。
3. 如果存在多个版本子目录，可以全部删除——它们只是待应用的变化文件，删除后下一次检查更新会重新 prepare。
4. 同样可以删除 `journal/` 目录下的日志（如决定放弃未完成的 Commit）；**不要删除 `known_files.json`**，否则会退化为首次迁移语义（不会误删，但会丢失已删除文件的追踪）。
5. 重新启动应用。插件会从 `Idle` 开始，根据配置重新检查更新。

**不要使用系统级的“磁盘清理”工具来清理这些目录**。Windows 磁盘清理针对的是系统更新缓存和临时文件，对本插件的暂存区没有识别能力，可能产生不可预期的交互。


## 6. 诊断信息收集清单

当问题无法通过上述自助步骤解决时，收集以下信息并上报。**收集前务必脱敏**：不包含用户名、完整安装路径中的个人标识、以及任何密钥或令牌。

### 6.1 必须收集

- [ ] **`status` 命令的返回值**（包含当前状态、版本、`error` 对象）。
- [ ] **`delta-update` 相关的全部日志行**，从日志文件中截取，覆盖最近一次完整的更新尝试。
- [ ] **`staging/` 和 `journal/` 目录的文件名列表**（不要文件内容）。
- [ ] **安装目录的绝对路径**（帮助判断是否在 `Program Files` 下、是否需要提权）。
- [ ] **应用是否以管理员权限运行**。
- [ ] **用户操作系统版本**（Windows 10/11，具体版本号）。

### 6.2 有则更好

- [ ] 安装目录下是否存在 `.old` 或 `.trash/` 残留。
- [ ] 用户是否安装了第三方安全软件（杀毒、防火墙、HIPS）。
- [ ] `endpoint` 是否可访问（仅确认 HTTP 状态，不分享完整 URL 中的签名参数）。
- [ ] 问题是否可稳定复现，还是偶发。
- [ ] 如果是“重启后未生效”，重启前后的 `status` 快照。

### 6.3 必须脱敏的内容

- 日志中可能包含的完整文件路径中的用户名。
- 预签名 URL（包含签名参数，应视为短期凭证）。
- 任何私钥或临时凭证（公钥本身就是公开信息，不需要脱敏）。


## 7. 运维手册总结

故障排查的核心判断是：**先确定问题发生在哪一层，再选择与该层匹配的动作**。`Prepared` 之后重启不生效是 Commit 层；反复下载失败是网络或签名层；更新准备好但写不进去是权限层；补丁应用失败但有全量回退是差分层的预期降级。将错误码与层对齐，比笼统地“重试”更有效。

对于一线支持，最重要的两条规则是：**签名类错误不重试，先核对密钥配对**；**`E_ROLLBACK_FAILED` 是最高严重级别，需要人工介入恢复备份**。对于用户自助，最重要的两条规则是：**手动清理只在应用退出后进行，只清理暂存区和 `.old` 残留**；**收集诊断信息时先脱敏，预签名 URL 和路径中的用户名不应进入工单**。