# Delta Update Engine 技术栈文档

> 本文定义项目开发所需的一切技术依赖与版本基线。在线依赖取当前最新稳定版并在 CI 中锁定。核心判断：**默认管线必须是纯 Rust、可复现、跨平台一致的（qbsdiff + zstd）；Zucchini / HDiffPatch 是可选的构建侧加速器，不是运行时的硬依赖**。主 exe 补丁压到 1% 量级是目标而非承诺，实际取决于编译器布局稳定性与归一化效果。


## 1. 本地环境基线

> 下表的版本号是**基线快照**，实际构建以仓库提交的 `Cargo.lock` / `pnpm-lock.yaml` 为准。本文用于说明依赖关系与选型边界，不作为逐字抄写的版本清单。

| 组件                    | 基线版本                                   | 角色                                   |
| ----------------------- | ------------------------------------------ | -------------------------------------- |
| **Rust (MSVC)**         | **1.99.1** `stable-x86_64-pc-windows-msvc` | 编译引擎、插件、CLI                    |
| **Cargo**               | 1.99.1                                     | 依赖管理与构建                         |
| **Tauri CLI**           | **2.12.4** (`cargo tauri`)                 | 插件项目构建与开发调试                 |
| **Node.js**             | **24.21.0**                                | 前端构建与 pnpm 运行时                 |
| **npm**                 | 11.19.0                                    | 仅作为 pnpm 引导，日常不使用           |
| **VS 2026 Build Tools** | 含“使用 C++ 的桌面开发”                    | `link.exe`、Windows SDK，MSVC 链接必需 |
| **WebView2**            | Win11 自带                                 | Tauri 前端渲染，无需额外处理           |

**对齐点**：`tauri` crate 当前最新稳定版为 **2.12.5**（2026-10-21），与你的 `tauri-cli 2.12.4` 同属 2.12 线，兼容。


## 2. 引擎层 `delta-update-engine`

纯 Rust crate，**零 Tauri 依赖**。负责清单比对、差分、压缩、暂存、Journal、回滚。

### 2.1 差分与压缩（核心管线）

| crate               | 版本                  | 用途                                     | 关键说明                                                     |
| ------------------- | --------------------- | ---------------------------------------- | ------------------------------------------------------------ |
| **`qbsdiff`**       | **1.x 最新稳定版**    | bsdiff 差分与 apply（默认管线）          | 纯 Rust，跨平台一致，无 C++ 构建依赖。大文件内存峰值需用预算约束。 |
| **`zstd`**          | **0.14.x**            | 补丁压缩/解压，含字典模式                | 成熟稳定，解压速度快；作为默认压缩后端。                     |
| **`hdiffpatch-rs`** | **可选，git tag 锁定** | HDiffPatch 差分与 apply（加速器）        | **未发布到 crates.io**，只能 git 依赖。仅在体积收益实测达标且构建可复现时启用。 |
| **`zucchini`**      | **可选，精确锁定**    | PE/ELF 归一化（加速器）                  | 早期版本 API 可能演进；必须封装在 `DiffProvider` 后，禁止泄漏到上层。 |

**管线组合**：默认 `qbsdiff → zstd`；自编译 PE 在加速器可用时走 `zucchini → qbsdiff/hdiff → zstd`；文本资源额外生成 zstd 字典候选；所有候选统一封装为 `.tdp` 容器，取字节最小者。

**为什么默认不用 HDiffPatch/Zucchini**：它们能显著缩小主 exe 补丁，但引入 git 依赖、C++ 工具链或早期 API 风险。把它们放在 `DiffProvider` 之后，意味着**即使某个加速器不可用，发布流水线仍然能产出合法补丁**，只是体积略大。这比“第一天就把项目绑死在一个未发布到 crates.io 的依赖上”稳健得多。

### 2.2 签名与验证

| crate                 | 版本       | 用途                             | 分层原则                                                     |
| --------------------- | ---------- | -------------------------------- | ------------------------------------------------------------ |
| **`minisign-verify`** | **0.2.5**  | 运行侧验签。零外部依赖，流式验证 | **引擎和插件只依赖此 crate**，私钥相关代码不进入用户机器。   |
| **`minisign`**        | **0.10.0** | 构建侧 CLI 签名                  | 仅在 `delta-update-cli` 中引入，支持 Ed25519 与密码保护私钥。 |

### 2.3 序列化、错误、异步与 HTTP

| crate            | 版本        | 用途                                                         |
| ---------------- | ----------- | ------------------------------------------------------------ |
| **`serde`**      | **1.0.229** | Manifest、Journal、事件 payload 的 derive。                  |
| **`serde_json`** | 1.0.151     | JSON 编解码。                                                |
| **`thiserror`**  | **2.0.21**  | 引擎错误枚举。**引擎不使用 `anyhow`**，保持错误可匹配。      |
| **`tokio`**      | **1.54.1**  | 异步运行时（`rt-multi-thread`、`macros`、`sync`、`time`、`fs`）。 |
| **`reqwest`**    | **0.13.5**  | `Fetcher` 生产实现，固定 rustls TLS 后端，支持 Range 流式下载。 |
| **`tokio-util`** | **0.7.x**   | `CancellationToken`：prepare 的协作式取消。 |
| **`sha2`**       | **0.10.x**  | 全链路 SHA-256：清单、文件、补丁容器、Journal。**缺失会导致核心功能无法实现**。 |
| **`hex`**        | **0.4.x**   | 哈希的十六进制编解码，统一小写表示。 |
| **`semver`**     | **1.x**     | 版本号解析与比较。**禁止用字符串比较版本**，预发布版本语义必须正确。 |
| **`async-trait`**| **0.1.x**   | `Fetcher` 等异步 trait 的对象安全实现。 |
| **`futures-util`**| **0.3.x**  | 流式下载：逐块写盘并计数，不把大对象读入内存。 |
| **`uuid`**       | **1.x**     | `commit_id`、临时文件名、管道名。 |
| **`fs4`**        | **1.1.x**   | 跨进程 `update.lock` 独占锁（`try_lock`，Windows/macOS/Linux 一致语义）。 |
| **`url`**        | **2.x**     | endpoint 与签名 URL 解析、HTTPS 校验、主机白名单比对。 |
| **`time`**       | **0.3.x**   | 事件 payload 的 RFC 3339 UTC 时间戳。                     |

`Fetcher` trait 抽象确保测试时可完全替换为内存 mock，不依赖真实网络。

### 2.4 文件系统与路径

| crate          | 版本           | 用途                                         |
| -------------- | -------------- | -------------------------------------------- |
| **`walkdir`**  | 2.x 最新稳定版 | 递归扫描安装目录，不跟随符号链接。           |
| **`tempfile`** | 3.x 最新稳定版 | 测试临时目录与运行侧临时文件。               |
| **`dunce`**    | 1.x 最新稳定版 | Windows 路径规范化，避免 `\\?\` 前缀不一致。 |

路径逃逸检查是引擎内部纯逻辑，**不依赖额外 crate**。


## 3. 插件层 `tauri-plugin-delta-update`

| 依赖                      | 版本                 | 用途                                       |
| ------------------------- | -------------------- | ------------------------------------------ |
| **`tauri`**               | **2.12.5**           | 插件宿主，与 `tauri-cli 2.12.4` 同线兼容。 |
| **`tauri-plugin`**        | **2.7.x（当前 2.7.2）** | 插件构建宏、权限生成、运行时。          |
| **`delta-update-engine`** | `path = "../engine"` | 核心依赖，纯 Rust。                        |
| **`serde`**               | 1.0.229              | 插件 Config 与事件 payload。               |
| **`tauri::async_runtime`** | 随 tauri             | `auto_check` 后台 prepare 的任务调度；不引入第二套运行时。 |
| **`tracing`**             | 0.1.x 最新稳定版     | 结构化日志，字段化诊断。                   |

**命令面收窄**：`build.rs` 用 `tauri_plugin::Builder` 自动生成 `allow-<command>` 权限。默认权限包只含 `check`、`prepare`、`cancel_prepare`、`status`、`confirm_healthy`、`restart_to_apply`。**不暴露任何接收文件路径参数的 commit 命令**。


## 4. 构建侧 CLI `delta-update-cli`

| 依赖                       | 版本                 | 用途                                                         |
| -------------------------- | -------------------- | ------------------------------------------------------------ |
| **`clap`**                 | **4.6.x 最新**       | 参数解析，`derive` 特性。四个子命令：`release`、`verify`、`verify-key`、`doctor`。 |
| **`minisign`**             | 0.10.0               | 清单签名。只从私钥文件路径读取，使用后清除临时文件。         |
| **`delta-update-engine`**  | `path = "../engine"` | 复用 `Differ` 的 diff 逻辑，保证两端格式一致。               |
| **`walkdir`**              | 2.x                  | 扫描 `--to` 目录。                                           |
| **`serde` / `serde_json`** | 1.0.229 / 1.0.151    | 生成 `latest.json`。                                         |
| **`anyhow`**               | 1.x 最新稳定版       | **仅 CLI 顶层**聚合错误，引擎内部仍用 `thiserror`。          |
| **`rayon`**                | 1.x 最新稳定版       | 并行生成多个候选补丁。                                       |
| **`indicatif`**            | 0.18.x 最新稳定版    | 发布进度终端展示（可选）。                                   |
| **`reqwest`**              | 0.13.5               | COS `PUT Object` / `HEAD Object` / `GET Object`。            |
| **`hmac` + `sha1`**        | 0.12 / 0.10          | COS REST 签名 v5（HMAC-SHA1）。直接用 reqwest 时必须自行签名。 |
| **`chrono`**               | 0.4.x                | COS 签名所需的时间戳格式。                                   |

**COS 交互策略**：CLI 只需要 `PUT`、`HEAD`、`GET` 三种操作，**直接用 `reqwest` + 自行实现签名 v5 是比引入不成熟 SDK 更可控的选择**。签名实现必须有单元测试固定向量，避免因参数排序或编码差异导致线上 403。


## 5. 前端工具链

包管理器选用 **pnpm**（严格 `node_modules`，减少幻影依赖）。

| 工具                  | 版本           | 用途                                            |
| --------------------- | -------------- | ----------------------------------------------- |
| **pnpm**              | **12.19.0**    | Node 24 完全满足要求。                          |
| **Vite**              | **8.1**        | 示例 UI 或 npm 包构建，Rolldown 打包器。        |
| **`@tauri-apps/api`** | **2.12.x**     | 前端 `invoke` / `listen`，与 Tauri 主版本对齐。 |
| **TypeScript**        | 5.x 最新稳定版 | 类型定义与示例 UI。                             |

**npm 包命名**：固定为 `tauri-plugin-delta-update-api`，**与 Cargo crate 版本严格同步**。


## 6. 测试与故障注入

| crate / 工具          | 版本                   | 用途                                                         |
| --------------------- | ---------------------- | ------------------------------------------------------------ |
| **`tempfile`**        | 3.x                    | 临时目录树，模拟安装目录和暂存区。                           |
| **`assert_cmd`**      | 2.x 最新稳定版         | CLI 端到端测试（`--dry-run` 等）。                           |
| **`predicates`**      | 3.x 最新稳定版         | 配合 `assert_cmd` 的断言。                                   |
| **`proptest`**        | 1.x 最新稳定版（可选） | 路径逃逸、Journal 幂等性的属性测试。                         |
| **自定义 `FaultyFs`** | 无外部依赖             | 实现 `FileSystem` trait，指定操作注入 I/O 错误或 `abort`。**故障注入不修改生产代码路径**。 |

**测试核心模式**：每个故障注入用例运行后，断言根目录**要么全是旧版本文件，要么全是新版本文件，不存在混合状态**。


## 7. CI/CD 与发布工具

| 工具                             | 用途                                                         |
| -------------------------------- | ------------------------------------------------------------ |
| **GitHub Actions / GitLab CI**   | 构建、测试、发布流水线。                                     |
| **Protected Environment Secret** | 存储 `MINISIGN_SECRET_KEY`，限制分支/tag，配置审批人。**私钥绝不落仓库、日志、命令行参数**。 |
| **`cos-rust-sdk` 或 `reqwest`**  | CLI 内完成对象上传。                                         |

**发布流水线标准步骤**：

```
cargo fmt --check
cargo clippy -- -D warnings
cargo test --workspace
  ├── L1 单元测试
  ├── L2 故障注入（--test-threads=1，不启用重试）
  └── L3 插件生命周期测试
delta-update release --dry-run --require-delta
delta-update release --key ...
delta-update verify --sample 3
```


## 8. 完整 `Cargo.toml` 依赖清单

### `delta-update-engine/Cargo.toml`

```toml
[dependencies]
serde = { version = "1.0.229", features = ["derive"] }
serde_json = "1.0.151"
thiserror = "2.0.21"
tokio = { version = "1.54.1", features = ["rt-multi-thread", "macros", "sync", "time", "fs", "net"] }
tokio-util = { version = "0.7", features = ["rt"] }
reqwest = { version = "0.13.5", default-features = false, features = ["rustls", "stream"] }
futures-util = "0.3"
minisign-verify = "0.2.5"
qbsdiff = "1"
zstd = "0.13"
sha2 = "0.10"
hex = "0.4"
semver = "1"
async-trait = "0.1"
uuid = { version = "1", features = ["v4"] }
fs4 = "1.1"
url = "2"
walkdir = "2"
tempfile = "3"
dunce = "1"
tracing = "0.1"
time = { version = "0.3", features = ["formatting"] }

[features]
default = []
test-faults = []

# 可选差分后端在实现 provider 时启用；当前骨架不声明依赖。
# hdiff = ["dep:hdiffpatch-rs"]
# zucchini = ["dep:zucchini"]

# [dependencies.hdiffpatch-rs]
# git = "<实际仓库 URL>"
# tag = "<锁定 tag>"
# optional = true
#
# [dependencies.zucchini]
# version = "=<锁定版本>"
# optional = true
```

### `tauri-plugin-delta-update/Cargo.toml`

```toml
[dependencies]
tauri = { version = "2.12.5", features = ["plugin"] }
tauri-plugin = "2.12"
delta-update-engine = { path = "../engine" }
serde = { version = "1.0.229", features = ["derive"] }
tracing = "0.1"
```

**插件不直接依赖 tokio**：后台任务使用 `tauri::async_runtime`，避免引入第二套运行时导致 shutdown 行为不一致。

### `delta-update-cli/Cargo.toml`

```toml
[dependencies]
clap = { version = "4.6", features = ["derive"] }
minisign = "0.10.0"
delta-update-engine = { path = "../engine" }
serde = { version = "1.0.229", features = ["derive"] }
serde_json = "1.0.151"
walkdir = "2"
anyhow = "1"
rayon = "1"
indicatif = "0.18"
reqwest = { version = "0.13.5", default-features = false, features = ["rustls"] }
hmac = "0.12"
sha1 = "0.10"
chrono = "0.4"
```


## 9. 写代码前必须验证的三件事

**第一，默认管线（qbsdiff + zstd）的体积与内存是否达标**。用一个真实的旧/新主 exe 跑最小用例，记录：补丁体积、diff 耗时、apply 耗时、峰值内存。验收标准不是“达到某个固定百分比”，而是：

- 补丁体积显著小于全量（目标量级：主 exe 个位数百分比）；
- 峰值内存 ≤ `文件大小 × 2.5`，超限时能安全回退全量；
- apply 耗时不超过同体积下载耗时的十分之一。

**第二，可选加速器是否值得引入**。对同一组样本分别跑 `hdiff`、`zucchini`，只有在“体积收益 ≥ 20% 且构建可复现”时才启用对应 feature。`hdiffpatch-rs` 的 git 依赖必须锁定 tag 并在 CI 中缓存；`zucchini` 必须精确锁定版本并封装在 `DiffProvider` 后，禁止其类型出现在公开 API 中。

**第三，COS 签名 v5 的固定测试向量**。签名实现必须用一组固定的 key/请求/时间戳断言输出完全一致，否则线上会以随机 403 的形式失败，而本地单测全绿。

**总结**：本地环境（Rust MSVC + VS Build Tools）是完备的编译基础。整个技术栈的设计原则是：**运行侧依赖面尽量窄**（引擎和插件不引入 `minisign` 私钥代码，不默认引入 C++ 依赖），**构建侧能力尽量集中**（CLI 复用引擎的 `Differ`，两端格式永不漂移），**可选优化必须可关闭**（加速器是 feature，不是硬依赖）。