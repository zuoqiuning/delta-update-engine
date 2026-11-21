# CLI 接口

> 本文件是 [API 参考](api.md) 的 CLI 部分：子命令、参数、Feature Flags 与平台支持矩阵。

## 5. delta-update-cli 公开接口

CLI 的命令行参数是公开接口，输出格式不承诺稳定。

### 5.1 子命令

```bash
delta-update release    # 生成清单、补丁、签名、发布
delta-update verify     # 从对象存储验证已发布内容
delta-update verify-key # 验证私钥与公钥配对；--json 输出 {v,tool,matches,public_key}
delta-update key-info   # 从私钥导出 base64 公钥与 key ID；--json 输出 {v,tool,public_key,key_id}
delta-update doctor     # 线上体检：清单、签名、补丁链、资源可访问性
delta-update inspect    # 离线解码本地 journal 目录，或审计已发布版本 / 对照诊断包
delta-update init       # 打印插件配置与 capability 片段；`--write` 生成 capabilities/delta-update.json
delta-update migrate    # 从官方 tauri-plugin-updater 打印迁移计划；--write 生成 capability 与迁移指南
delta-update report     # 汇总 release --pipeline 的历史报告，查看增量覆盖率趋势
delta-update promote    # 把已发布版本切到另一个 channel（canary → stable），只改 channel 并重签
delta-update rollback   # 让 latest 指回已发布版本（事故响应）
delta-update serve      # 本地 HTTP 演示服务
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
--baseline <VERSION=DIR> 额外补丁基线，可重复，需同时提供 --from；写入清单 deltas
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
--journal-dir <DIR>     离线校验安装树：使用该目录里的 installed_manifest / verified_manifest
--install-root <DIR>    安装根，配合 --journal-dir；缺失或哈希不符的文件会逐条列出并以非零码退出

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
--write                 生成 capability 与编辑器 schema（不改 tauri.conf.json）
--merge-config          把 plugins.delta-update 合并进 tauri.conf.json 并写 capability；已有该段不覆盖，原文件保留 .bak
--auto                  写入 auto_apply/auto_confirm = true：无需前端代码即可自动检查、退出替换、自动确认
--permission-set <SET>  manage-confirm（默认）/ manage / monitor / default
--json                  stdout 只输出一个 JSON 对象（配置、写入结果、警告）

migrate:
--dir <DIR>             项目根目录，默认 .
--write                 生成 capability、编辑器 schema 与 delta-update-migration.md
--json                  stdout 只输出一个 JSON 对象（配置、写入结果、迁移指南）

serve:
--dir <DIR>             `release --bucket <DIR>` 写出的目录
--port <PORT>           默认 8123；0 由系统分配
--host <HOST>           默认 127.0.0.1；非回环地址时客户端不接受明文 HTTP
```

`serve` 启动后直接打印可粘贴进 `plugins.delta-update.endpoint` 的 URL，并提示本地联调需要 `allow_insecure_http: true`；它是无 TLS 的诊断/演示服务器，不是生产 CDN。

`verify` 从对象存储重新下载清单、验签、抽样下载补丁、应用补丁、校验目标哈希。`doctor` 额外校验：所有备用端点的版本一致性、清单字段合法性、对象可访问性与字节数；`doctor --check-config` 不访问网络，直接使用引擎的配置 schema 校验 `plugins.delta-update`，并用 JSON 解析 `src-tauri/capabilities/*.json`：坏文件与未知的 `delta-update:*` 权限标识（如 `manage-confrim` 拼写错误）都会让检查失败——Tauri 运行时同样会拒绝这些标识，提前在 CI 暴露比启动失败更容易定位。`doctor --watch <SECONDS>` 按 `--interval` 反复执行同一套检查，适合发布后 5–10 分钟的 CDN 传播观察窗口；任何一轮失败都会让整个命令失败。

`migrate --write` 只创建两个新文件（capability 与 `delta-update-migration.md`），**不修改** `tauri.conf.json`、`Cargo.toml` 或 `main.rs`；已存在的文件一律跳过，因此不需要备份或冲突合并。迁移指南包含五节：配置替换、capability、Cargo 依赖、`main.rs` diff，以及官方前端 API（`check()` / `downloadAndInstall` / `relaunch` / `onUpdaterEvent`）到 `check()` / `prepare()` / `restartToApply()` / `listen(STATE_EVENT)` 的对照表与一段可直接粘贴的 `createUpdateController()` 片段。

`inspect --journal-dir` 离线读取 journal 目录，输出待处理提交、PREPARED、回滚与损坏状态，在末尾打印 `worker.log` 里最新一条结构化事件（`--json` 时以 `worker_event` 字段给出，含 `name`/`ts`/`msg`）与最近 3 次更新终态（`--json` 的 `history` 为最近 10 条）；退出码 `1` 表示未完成 Journal（可重启恢复），`2` 表示状态损坏（需人工介入）。

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

### 5.4 `promote` / `rollback`

```bash
delta-update promote --bucket cos://your-bucket --version 2.1.0 \
  --channel stable --key release.key --verify
delta-update rollback --bucket cos://your-bucket --to-version 2.0.0 --key release.key
```

`promote` 把已发布版本切到另一个 channel（典型流程：先 `release --channel canary`，验证后 `promote --channel stable`）。它只改写清单的 `channel`、重签并重写 `latest`/envelope/SBOM，**不重新生成补丁**；`--verify` 在切换前 HEAD 校验全部对象（含多基线补丁），缺对象时拒绝提升。多签名发布必须用 `--additional-signing-key` 传入与清单 `signing_key_ids` 完全相同的集合，否则报错。`--dry-run` 不上传，`--json` 输出 `{v, tool, version, from_channel, channel, manifest_bytes, signatures, dry_run}`。

`rollback` 只把 `latest` 指回已发布版本并标记 `rollback: true`，版本化清单保持不变。

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

**平台判断在编译期完成**：非 Windows 目标直接 `compile_error!`，无法构建出会“静默禁用更新”的二进制；运行期 `ensure_supported()` 再返回 `E_UNSUPPORTED_PLATFORM` 作为纵深，主应用仍可启动，**不会下载任何字节**。引擎本身是平台无关的 Rust crate，可以在其他平台独立使用；限制只存在于插件的文件替换适配层。

## 8. API 参考总结

这份 API 参考的核心判断是：**公开接口的“窄”比“全”更重要**。引擎暴露 `Engine` 一个主句柄和几个 trait，插件只暴露 11 个命令和一个事件，CLI 暴露 11 个子命令。每一个公开名称都是一份维护承诺——新增容易，退役难。`#[non_exhaustive]` 和 `thiserror` 枚举的组合，使新增变体和字段不破坏 SemVer，同时保持调用方可匹配的精确性。`Fetcher` 和 `FileSystem` 的 trait 抽象使引擎可以脱离 Tauri 和网络独立测试，这是故障注入验证能够成立的前提。

二次开发者最可能用到的入口是 `Engine::new` + `Engine::check` + `Engine::diff` + `Engine::prepare`；`Engine::begin_commit` 只应由替换进程调用。如果只需要 Tauri 集成，`tauri_plugin_delta_update::init()` 和 11 个命令已经覆盖全部场景。CLI 的 `release`、`verify`、`doctor` 是发布流水线的三个标准步骤，参数应通过环境变量和 CI secret 注入，而非硬编码。
