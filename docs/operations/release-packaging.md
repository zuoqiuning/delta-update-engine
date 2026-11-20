# 打包、签名与 crates/npm 发布

> 本文件是[发布手册](release.md)的打包部分：Authenticode、NSIS 安装形态、crates.io 与 npm 的发布顺序和密钥轮换边界。

## 12. Windows 代码签名与安装器验收

### 12.1 Authenticode 与 minisign 的分工

两者解决不同问题，缺一不可：

| 机制        | 保护对象                       | 信任来源                              | 失败后果                         |
| ----------- | ------------------------------ | ------------------------------------- | -------------------------------- |
| minisign    | 清单与更新对象                 | 客户端二进制内嵌的公钥集合            | 拒绝更新，`E_SIGNATURE_*`        |
| Authenticode | 应用 exe/DLL 与安装器本身的来源 | Windows 证书链与时间戳                | SmartScreen 警告、企业策略拦截   |

**不要用 Authenticode 代替 minisign**：Authenticode 不覆盖 `.tdp` 补丁和清单内容，也不提供“任意一把受信公钥”的轮换语义。反之，minisign 不能阻止用户下载到一个被替换的安装器。

### 12.2 签名流程

1. 构建完成后、上传安装器之前，对**最终会落盘的所有 PE 文件**签名：主 exe、随包 DLL、NSIS 安装器。更新 worker 是主 exe 的受限模式，与主 exe 共用同一份签名，不需要单独签名。
2. 使用仓库脚本签名并立即验证：

```powershell
pwsh scripts/windows/sign-authenticode.ps1 `
  -Path .\target\release\my-app.exe `
  -Thumbprint $env:CODESIGN_THUMBPRINT

pwsh scripts/windows/sign-authenticode.ps1 `
  -Path .\target\release\bundle\nsis\my-app_1.2.3_x64-setup.exe `
  -Thumbprint $env:CODESIGN_THUMBPRINT
```

脚本使用 SHA-256 摘要与 RFC 3161 时间戳，并在签名后执行 `signtool verify /pa`。**时间戳必须启用**，否则证书过期后已分发的安装器会失去有效性。

3. 签名后的二进制哈希会变化，因此**签名必须发生在 `delta-update release` 扫描产物之前**。当前 CLI 的清单哈希来自 `--to` 目录的实际字节；先扫描再签名会让清单与最终文件不一致。

### 12.3 NSIS perUser / perMachine 手动验收清单

真实 UAC、安装器与 `Program Files` 写入无法在无提权 CI 中自动化，每次发布前在 Windows 虚拟机手动执行。**任一项失败阻塞发布**。

**perUser（默认，无 UAC）**

- [ ] 安装到用户目录后，应用正常启动，`Program Files` 下无残留。
- [ ] 完整更新一次：退出应用无 UAC，重启后版本号为新版本。
- [ ] 更新后首次启动不弹 UAC（已提交待确认不触发恢复提权）。
- [ ] `%LOCALAPPDATA%\<bundleIdentifier>\delta-update\journal\` 在健康确认后只剩 `known_files.json` 与空锁文件。
- [ ] 卸载后安装目录、开始菜单项与更新数据目录按预期清理。

**perMachine（`Program Files`，需要 UAC）**

- [ ] 以管理员安装到 `Program Files`，普通用户启动应用不弹 UAC（未发生更新时）。
- [ ] 完整更新一次：应用退出后弹一次 UAC，更新完成后新版本经 Explorer 降权启动，任务管理器中的完整性级别为“中”。
- [ ] 在提交执行到一半时强制结束 worker，再次启动应用：启动阶段弹一次 UAC，授权后 Journal 被重放或回滚，主窗口正常出现。
- [ ] 上述启动提权选择“否”：应用以 `Failed` 启动，`journal/` 与 `.old/` 保留；再次启动仍会请求授权。
- [ ] 连续三次不确认健康后第四次启动：自动回滚到旧版本，`skipped_version` 写入，计数归零；正常关闭（超过 `exit_grace_period_secs`）会折扣一次计数。
- [ ] perUser：确认健康后当前 commit 的 `.old` / `.trash` 被清理；perMachine：确认在用户级运行，无权限删除的残留会保留（不影响运行），需在管理员会话中人工清理。
- [ ] 卸载后 `Program Files` 目录、注册表卸载项与用户数据目录按预期清理。

**签名检查（两种模式都执行）**

- [ ] `Get-AuthenticodeSignature` 对安装后的主 exe 返回 `Status = Valid`，且时间戳存在。
- [ ] 安装器属性页显示预期的发布者名称，SmartScreen 不出现“未知发布者”。

### 12.4 应用列表版本的边界

文件级更新只替换安装目录中的文件，**不会改写 NSIS 安装器写入的 `DisplayVersion`**。因此“应用和功能”里可能仍显示安装时的版本，而应用自身已经是新版本。需要保持一致时，可在 `confirm_healthy` 之后由应用更新对应卸载键（perUser 为 HKCU、perMachine 为 HKLM），或用新安装包覆盖安装。不要为了同步版本号而让主进程整体提权。

### 12.5 无证书环境的边界

仓库与 CI 不包含代码签名证书。没有证书时**不要伪造签名校验逻辑**：可以构建与测试更新链路，但 `Get-AuthenticodeSignature` 会返回 `NotSigned`，SmartScreen 会告警，企业环境可能直接拦截。证书申请、HSM/密钥保管与 CI 注入属于组织流程，不在本项目代码范围内。


## 13. crates.io 与 npm 发布

### 13.1 发布顺序

1. `delta-update-engine`
2. `tauri-plugin-delta-update`（依赖 engine 的已发布版本）
3. `delta-update-cli`（依赖 engine）
4. `tauri-plugin-delta-update-api`（npm，独立于 crate 发布顺序）

`delta-update-testkit` 与 `e2e`、`xtask` 标记 `publish = false`。testkit 只作为**开发依赖**使用且不写版本号，Cargo 打包时会将它从发布清单中剔除；**不要给 testkit 加 `version`**，否则会要求先发布一个测试夹具 crate。

### 13.2 发布前校验

```bash
# 清单与编译校验（不真正上传）
cargo publish -p delta-update-engine --dry-run --registry crates-io

# 依赖 engine 的 crate 需要 engine 已发布后才能 dry-run；
# 首次发布前用 package 列表检查打包内容
cargo package -p tauri-plugin-delta-update --list --allow-dirty
cargo package -p delta-update-cli --list --allow-dirty

# npm 包内容（dist、README、package.json）
cd packages/plugin-api && npm pack --dry-run
```

发布前确认：`readme`、`keywords`、`categories`、`license` 已填写；`repository` 使用公开仓库 `https://github.com/zuoqiuning/delta-update-engine`（不要写本地路径或占位 URL）。`cargo xtask version-sync` 必须保证 workspace、插件 manifest 与 npm 包版本一致。

### 13.3 预发布（rc）演练

`release.yml` 的 tag 触发会校验 `workspace.package.version` 与 tag（去掉 `v`）**完全一致**。因此不能用旧版本的代码直接打新 tag——先同时修改 `Cargo.toml` 与 `packages/plugin-api/package.json` 的版本，再执行：

```bash
cargo xtask version-sync
cargo xtask ci
cargo test --workspace

# 本地可做的打包检查（不访问网络）
cargo package -p delta-update-engine --offline --no-verify --allow-dirty
cd packages/plugin-api && npm publish --dry-run

git tag v1.1.0
git push origin v1.1.0
```

关键约束：

- **依赖 engine 的 crate 无法在 engine 发布前 `--dry-run`**：`tauri-plugin-delta-update` 与 `delta-update-cli` 会报 `no matching package named delta-update-engine`。这不是配置错误，`release.yml` 的 `wait_for_crate` 正是为此存在；本地只能用 `cargo package --list` 检查打包内容。`wait_for_crate` 直接请求 crates.io 的版本端点（`/api/v1/crates/<crate>/<version>`），返回 200 才算可见。它不解析 `cargo search` / `cargo info` 的输出，因为那些格式不是稳定 API，且预发布版本的显示行为不受保证。
- **npm 预发布走 `next` tag**：版本号含 `-` 时 `release.yml` 自动使用 `--tag next`，避免 `latest` 指向 rc；正式版恢复 `latest`。
- **sccache/代理**：内网环境若把 crates.io 替换为镜像，`cargo publish --dry-run` 需要显式 `--registry crates-io`；镜像不可达时只能依赖 CI。`wait_for_crate` 的 `curl` 遵循 `HTTPS_PROXY` / `NO_PROXY`；发布机需要代理时在 job 环境里配置，不要改轮询逻辑。
- **预发布只接收修复**：版本号带 `-` 的阶段不新增公开 API；发现问题后升一个新的预发布号，不要原地覆盖已推送的 tag。

### 13.4 密钥轮换的当前边界

`key_rotation` **不是清单字段，也不在运行期生效**：清单出现该字段时按未知字段忽略，插件配置出现时只输出一条警告。信任集只来自客户端二进制内嵌的 `signing_public_keys`，因此轮换只能通过“旧密钥签名的新版本内嵌新公钥”完成，不能通过清单或配置在线扩展信任。宿主若在 `main` 开头调用 `set_trusted_keys`，只能**收窄**（`TrustMode::Intersect`，与内嵌公钥取交集）或显式接管（`TrustMode::PinnedOnly`，由宿主自己为信任根负责）；两者都与发布端无关，不能替代“提前一个版本内嵌新公钥”。

这意味着两件事：

- 正常轮换必须**提前一个版本**把新公钥内嵌进客户端；如果旧客户端尚未更新，就不能只用新密钥签名。
- 私钥在轮换生效前泄露时，系统没有带内吊销能力；不要试图用 `key_rotation` 之类的字段“热修复”信任集。

轮换 runbook（详见 [compatibility.md §4](../reference/compatibility.md#4-密钥轮换的迁移路径)）：

1. 用**当前签名密钥**发布桥接版本：二进制内嵌 `{旧, 新1, 新2}`，清单仍由旧密钥签名；确认目标用户版本分布已覆盖该桥接版本。
2. 下一个版本起改用新密钥签名；运行 `delta-update verify --public-key <新公钥>` 确认对象可用。
3. 观察一个发布周期后，再从内嵌集移除旧公钥；旧私钥离线归档，不删除历史签名。

### 13.5 首次稳定版（1.1.0）前置清单

> **适用范围**：`1.1.0` 是首个稳定版，本节的一次性条目只对它成立。后续 `1.2.0` 起按常规发布流程执行，只保留标注 **[长期]** 的条目。
>
> 每条标注 **[一次性]**（只对首次稳定版成立，做完即失效）或 **[长期]**（每次发布都该做）。

| # | 动作 | 类型 |
| --- | --- | --- |
| 1 | 本地过一遍打包与校验：`cargo publish --dry-run -p delta-update-engine --locked` | [长期] |
| 2 | 确认 CHANGELOG 的 `## [1.1.0] - <日期>` 已从 `[未发布]` 收口，并保留“升级说明”小节 | [一次性] |
| 3 | 确认 README 安装命令使用裸包名（`pnpm add tauri-plugin-delta-update-api`） | [一次性] |
| 4 | 确认 [versioning.md](../reference/versioning.md) 说明 `latest` 指向稳定版、预发布走 `next` | [一次性] |
| 5 | 确认远端 tag 不复用：已推送的 `rc.*` 保留为历史，不要删除或重打 | [一次性] |
| 6 | 全量门禁：`cargo xtask ci`、`cargo xtask docs-check`、`cargo xtask version-sync`、`cargo run -p delta-update-e2e`、`pnpm typecheck && pnpm test`，全绿后打 `v1.1.0` 并推送 | [长期] |
| 7 | **干净环境首装演练**（见下） | [一次性] |

**第 1 条的坑**：本机 `~/.cargo/config.toml` 若把 `crates-io` 替换成国内镜像，`cargo publish` 会被直接拒绝——`crates-io is replaced with non-remote-registry source`。先临时移走镜像段，或用干净的 `CARGO_HOME` 再跑 dry-run，不要把它当成打包错误排查。

**第 7 条：干净环境首装演练**。`rc.4` 发布失败的根因是 runner 的 `%TEMP%` 走 8.3 短名（`RUNNER~1`），而本机 `%TEMP%` 没有短名别名，所以本地与常规 CI 都是绿的；这类"同一目录两种拼写"的问题只在干净环境暴露。

1. 启用 Windows Sandbox（`启用或关闭 Windows 功能` → 勾选 `Windows Sandbox` → 重启），把仓库复制进沙箱。
2. 在沙箱里完整跑一遍 `real-test\run.ps1`：`serve --port 0` 的端口解析、update、confirm 都要走通。
3. 用 `cmd /c for %I in ("%TEMP%") do @echo %~sI` 取当前 TEMP 的短名形式，把它赋给 `TEMP` / `TMP` 后复跑一遍。

把"等第一个用户报告"换成一次可复现的证据。1.0 之后不必每次发布都做；当改动涉及路径比较（`long_path`、`install_root` / `journal_root` 校验、`serve` 路径解析）时再按需复跑。
