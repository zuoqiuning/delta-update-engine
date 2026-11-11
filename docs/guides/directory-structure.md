# Delta Update Engine 项目目录结构规范

> 本文定义仓库的唯一目录布局。目标不是“看起来整齐”，而是让**每个目录只承担一个职责、每条依赖只有一个方向、每个新增文件都有唯一合理的落点**。任何人（包括 AI Agent）在不读完全部代码的情况下，都能从路径判断一个文件属于哪一层、能依赖谁、不能被谁依赖。
>
> 本文是目录与命名的唯一契约。新增顶层目录、crate、npm 包或文档分类，必须先改本文，再改仓库。
>

---

## 1. 设计原则

| 原则             | 具体约束                                                                 |
| ---------------- | ------------------------------------------------------------------------ |
| **按职责分层**   | 引擎、插件、CLI、测试夹具、示例、文档各自独立，不按“文件类型”建万能目录。 |
| **依赖单向**     | 上层依赖下层，下层绝不反向依赖；引擎不依赖 Tauri，插件不依赖具体算法实现。 |
| **目录即边界**   | 一个目录对应一个可命名的职责；目录内文件共享同一组依赖与测试策略。       |
| **测试就近**     | 单元测试跟着模块走；集成测试放在 crate 的 `tests/`；跨进程端到端放 `e2e/`。 |
| **产物不入库**   | `target/`、`dist/`、`node_modules/`、生成的大体积夹具、日志、密钥一律忽略。 |
| **命名可预测**   | Rust 用 `snake_case`，crate/包/文档用 `kebab-case`，路径中不出现空格与中文。 |

---

## 2. 顶层结构

```
delta-update-engine/
├── .cargo/                     # Cargo 别名与构建配置
├── .github/                    # CI、发布、Issue/PR 模板
├── crates/                     # 所有 Rust crate（引擎、插件、CLI、测试夹具）
├── packages/                   # npm 包（前端 API）
├── examples/                   # 可运行的示例 Tauri 应用
├── e2e/                        # 跨进程端到端测试（独立 crate，publish = false）
├── xtask/                      # 仓库自动化（cargo xtask ...）
├── scripts/                    # 工具脚本（Windows 签名）
├── docs/                       # 全部文档，按主题分类
├── Cargo.toml                  # workspace 根清单
├── Cargo.lock                  # 应用/二进制仓库应提交
├── rust-toolchain.toml         # 工具链与组件声明
├── rustfmt.toml / clippy.toml  # 格式化与 lint 基线
├── deny.toml                   # 许可证与依赖来源约束
├── .editorconfig / .gitattributes / .gitignore
├── package.json                # pnpm 工作区脚本入口
├── pnpm-workspace.yaml         # npm 包成员与构建脚本白名单
├── pnpm-lock.yaml              # 前端依赖锁定，提交
├── .npmrc                      # npm 国内镜像
├── README.md                   # 项目入口：定位、能力、三步接入、文档索引
├── CHANGELOG.md                # 面向使用者的变更记录
├── CONTRIBUTING.md             # 贡献流程、目录约定、测试命令
├── SECURITY.md                 # 漏洞报告渠道与密钥泄露应急入口
└── LICENSE
```

**根目录只允许上述内容**。临时文件、压缩包、截图、调试日志、发布产物一律不得留在根目录。IDE 自动生成的 `.screenshots/` 属于本地工具目录，已在 `.gitignore` 中忽略，不视为仓库内容。

本文描述的是**目标布局与唯一落位规则**：尚未实现的能力，其模块与测试文件在该能力实现时按本文的命名和位置加入，不得另起目录。

---

## 3. Workspace 与 crate 职责

```toml
# Cargo.toml（示意）
[workspace]
resolver = "2"
members = ["crates/*", "e2e", "xtask"]
exclude = ["examples/minimal-tauri-app/src-tauri"]
```

| 路径                                 | 类型            | 发布          | 职责                                                         | 禁止依赖                                   |
| ------------------------------------ | --------------- | ------------- | ------------------------------------------------------------ | ------------------------------------------ |
| `crates/delta-update-engine`         | 纯 Rust 库      | crates.io     | 清单、验签、下载、差分应用、暂存、Journal、提交、回滚、状态机 | Tauri、插件、CLI、`minisign` 私钥、COS SDK |
| `crates/tauri-plugin-delta-update`   | Tauri 2 插件    | crates.io     | 生命周期映射、命令面、事件、权限、替换进程入口               | CLI、具体差分算法、COS SDK                 |
| `crates/delta-update-cli`            | 二进制 + 库     | crates.io     | 扫描、补丁生成、清单、签名、发布、verify、doctor             | Tauri                                      |
| `crates/delta-update-testkit`        | 测试夹具库      | **不发布**    | `FaultyFs`、MockServer、签名/清单构造、崩溃 harness          | 插件、CLI                                  |
| `packages/plugin-api`                | npm 包          | npm           | 前端类型、`invoke` 封装；无业务逻辑                          | 任何 Rust 代码                             |
| `examples/minimal-tauri-app`         | 示例应用        | **不发布**    | 三步接入的最小可运行样例                                     | 无（独立 workspace）                       |
| `e2e`                                | 测试二进制      | **不发布**    | 跨进程、跨版本、真实安装/替换/回滚场景                       | 被测试代码不得依赖它                       |
| `xtask`                              | 仓库工具        | **不发布**    | CI 聚合、文档链接、版本同步、夹具生成                        | 不依赖业务 crate（保持零耦合）             |

**依赖方向**：

```
                    ┌────────────────────────────┐
                    │        examples/           │
                    └─────────────┬──────────────┘
                                  │ path 依赖
                    ┌─────────────▼──────────────┐
                    │ tauri-plugin-delta-update  │
                    └─────────────┬──────────────┘
                                  │
   ┌──────────────┐    ┌──────────▼───────────┐
   │ delta-update │    │   delta-update-      │
   │    -cli      │───►│      engine          │◄──── e2e（测试）
   └──────────────┘    └──────────▲───────────┘
                                  │ dev-dependency
                    ┌─────────────┴──────────────┐
                    │   delta-update-testkit     │
                    └────────────────────────────┘

   packages/plugin-api 与 Rust 侧无编译依赖，只共享协议字段。
```

**规则**：

1. 引擎的公开 API 不出现 `tauri`、`reqwest::*`、`minisign` 私钥类型、COS 类型。
2. 插件只通过引擎的公开 API 工作，不直接实现差分、下载或文件替换。
3. CLI 复用引擎的 `manifest`、`patch`、`crypto`，保证生成端与应用端格式一致。
4. `delta-update-testkit` 可以被引擎的 dev-dependencies 与 `e2e` 使用；引擎的生产依赖不得包含它。
5. `e2e` 只能通过公开 API / 二进制行为验证，不得把测试专用后门塞进生产 crate。

---

## 4. 核心引擎 `crates/delta-update-engine`

```
crates/delta-update-engine/
├── Cargo.toml
├── README.md                   # 引擎定位、最小示例、feature 说明
├── src/
│   ├── lib.rs                  # 门面：声明模块 + 重导出稳定 API，不写业务逻辑
│   ├── engine.rs               # Engine 句柄与编排（check/diff/prepare/recover）
│   ├── recovery.rs             # 启动恢复：重放、崩溃回滚、复用 PREPARED
│   ├── config.rs               # EngineConfig 与校验
│   ├── error.rs                # EngineError / ErrorCategory
│   ├── limits.rs               # 清单、文件、补丁、内存等硬上限
│   ├── version.rs              # SemVer、通道、rollback 守卫
│   ├── fs/
│   │   ├── mod.rs              # FileSystem trait
│   │   ├── real.rs             # 生产文件系统
│   │   ├── atomic.rs           # 临时文件 + rename、sync_dir
│   │   └── fault_injection.rs  # 仅 test-faults feature 编译
│   ├── path/
│   │   ├── mod.rs
│   │   ├── rel_path.rs         # 相对路径类型与校验
│   │   ├── resolver.rs         # 安装根解析（exe 优先，注册表只读校验）
│   │   └── guard.rs            # 逃逸、保留名、大小写冲突、长度
│   ├── manifest/
│   │   ├── mod.rs
│   │   ├── model.rs            # Manifest / FileEntry / Strategy
│   │   ├── parse.rs            # 原始字节 → 受信结构，含大小/条目上限
│   │   ├── diff.rs             # Unchanged/Modified/Added/Removed/Untracked
│   │   ├── hash_cache.rs       # size + mtime 命中复用安装根文件哈希
│   │   └── history.rs          # known_files.json
│   ├── crypto/
│   │   ├── mod.rs
│   │   ├── hash.rs             # SHA-256 流式校验
│   │   └── minisign.rs         # detached 签名验证（只读公钥）
│   ├── fetch/
│   │   ├── mod.rs              # Fetcher trait 与统一出口
│   │   ├── http.rs             # reqwest 实现、Range、超时、重试分类
│   │   ├── signing.rs          # signing_endpoint 契约、URL 缓存/刷新
│   │   └── resume.rs           # 断点身份与临时文件管理
│   ├── patch/
│   │   ├── mod.rs
│   │   ├── container.rs        # .tdp 头：magic/策略号/源哈希/目标哈希/大小
│   │   ├── apply.rs            # 目标哈希、解压上限、内存预算
│   │   └── provider.rs         # 构建侧 DiffProvider（qbsdiff/hdiff/zucchini）
│   ├── staging/
│   │   ├── mod.rs
│   │   ├── plan.rs             # PREPARED 计划（变化文件 + 目标全集）
│   │   ├── prepare.rs          # 下载 → 应用 → 物化变化文件
│   │   └── verify.rs           # 暂存文件 + 未变化文件原地校验
│   ├── journal/
│   │   ├── mod.rs              # CommitJournal 公开接口
│   │   ├── format.rs           # TDJ1、尾部 checksum、5 类 op
│   │   ├── plan.rs             # PREPARED 计划 → 5 类 FileOp 序列
│   │   ├── state.rs            # apply.json / commit.status / attempts / skipped_version
│   │   ├── replay.rs           # 幂等向前重放、来源/目标哈希
│   │   └── lock.rs             # update.lock
│   ├── apply/
│   │   ├── mod.rs
│   │   ├── writer.rs           # PrivilegedWriter / DirectWriter / WorkerWriter
│   │   └── applier.rs          # --delta-update-apply / --delta-update-recover 入口与 commit.status
│   ├── rollback/
│   │   ├── mod.rs
│   │   └── inverse.rs          # 提交日志求逆、恢复 .old/.trash
│   └── state/
│       ├── mod.rs
│       ├── machine.rs          # 状态枚举与合法迁移
│       └── event.rs            # 事件 payload
├── tests/
│   ├── common/mod.rs           # 目录树、断言工具、测试夹具装配
│   ├── journal_fault_injection.rs
│   ├── committer_fault_injection.rs
│   ├── fetch_fault_injection.rs
│   ├── presign_refresh.rs
│   ├── security_fault_injection.rs
│   ├── staging_contract.rs
│   ├── security_contract.rs
│   ├── error_contract.rs       # 错误码与 retryable 契约
│   ├── diff_contract.rs        # 本地扫描与差异分类
│   ├── check_contract.rs       # 验签、通道、版本守卫与重试
│   ├── committer_contract.rs   # 提交、恢复、崩溃回滚与清理
│   ├── path_property.rs
│   └── manifest_property.rs
└── benches/
    ├── patch_header.rs
    ├── journal_verify.rs       # Journal 重放、50k op 编解码、PREPARED 校验
    └── hash_tree.rs
```

**引擎内部依赖方向**（从上到下，禁止反向）：

```
engine
  ├── staging ──► patch ──► fs
  ├── staging ──► manifest ──► path
  ├── journal ──► fs / path
  ├── apply   ──► journal / fs / path
  ├── rollback──► journal / apply
  ├── recovery──► journal / rollback
  ├── fetch   ──► limits / error
  └── crypto  ──► limits / error

foundation: error, limits, version, fs, path, state
```

**模块规则**：

- `lib.rs` 只做声明与重导出，不写 `impl`、不写业务流程。
- 每个目录的 `mod.rs` 只包含 `pub mod` / `pub use`，不得堆放业务代码。
- 领域模块自己拥有错误映射与不变量校验，不把校验责任推给调用方。
- 平台差异集中在 `fs/atomic.rs`、`path/resolver.rs` 与 `apply/writer.rs`；领域模块不散落 `cfg(windows)`。
- 测试专用代码通过 `test-faults` feature 或 `delta-update-testkit` 提供，生产路径不出现 `if cfg!(test)` 分支。

---

## 5. 插件 crate `crates/tauri-plugin-delta-update`

```
crates/tauri-plugin-delta-update/
├── Cargo.toml
├── build.rs                    # tauri_plugin::Builder：命令权限生成；测试目标 Windows 清单
├── README.md
├── permissions/
│   ├── default.toml            # 手写：默认权限包
│   └── autogenerated/          # 构建生成，提交并由 .gitattributes 标记 generated
├── src/
│   ├── lib.rs                  # init() 与插件注册
│   ├── config.rs               # Config、config_version、未知字段告警
│   ├── commands.rs             # 六个命令的薄适配层
│   ├── lifecycle.rs            # setup 恢复、退出时 spawn、auto_confirm
│   ├── events.rs               # delta-update://state 广播
│   ├── state.rs                # 进程内状态快照与命令串行化
│   ├── applier.rs              # 替换/恢复进程启动：可写探测、UAC 提权、等待父进程
│   └── desktop.rs              # 桌面平台能力判定（非桌面平台注册期拒绝）
└── tests/
    ├── config_contract.rs      # 与协议文档 §9 的字段对齐
    └── lifecycle_contract.rs   # Tauri mock runtime：启动恢复、auto_check/auto_confirm、退出提交、ACL
```

**规则**：

- `commands.rs` 只做参数接收、状态读取、错误映射，不写文件操作。
- 纯映射逻辑（状态、进度、错误类别）使用模块内单元测试；跨进程生命周期由 L4 e2e 覆盖。
- 插件不直接 `use reqwest`、`use qbsdiff`、`use minisign`；这些能力属于引擎。
- 任何新增命令都先改协议文档、权限文档、本目录的 `commands.rs` 与 `permissions/default.toml`。

---

## 6. CLI crate `crates/delta-update-cli`

```
crates/delta-update-cli/
├── Cargo.toml
├── README.md
├── src/
│   ├── main.rs                 # 仅解析参数、初始化日志、调用 commands
│   ├── cli.rs                  # clap 定义与全局选项
│   ├── commands/
│   │   ├── mod.rs
│   │   ├── release.rs
│   │   ├── verify.rs
│   │   ├── verify_key.rs
│   │   └── doctor.rs
│   ├── pipeline/
│   │   ├── mod.rs              # prepare_context → prepare_manifest → patches → finalize → publish
│   │   ├── scan.rs             # 扫描目录、.deltaignore
│   │   ├── plan.rs             # 差异计划与策略路由
│   │   ├── patches.rs          # 候选生成、双候选取小、60% 降级
│   │   ├── manifest.rs         # 清单构建与字段校验
│   │   ├── sign.rs             # minisign detached 签名
│   │   └── publish.rs          # 原子发布顺序
│   ├── publisher/
│   │   ├── mod.rs              # Publisher trait
│   │   ├── fs.rs               # 本地/测试发布
│   │   ├── cos.rs              # COS PUT/HEAD/GET
│   │   └── cos_signer.rs       # COS 签名 v5（固定测试向量）
│   ├── ignore.rs               # .deltaignore 解析与默认排除
│   └── output.rs               # 人读/JSON 输出
└── tests/
    ├── cli_contract.rs         # 参数、退出码、输出稳定性
    ├── release_pipeline.rs     # 发布→验签→抽样验证、dry-run、rollback、prefix
    └── doctor.rs               # 端点验签、对象可访问性、JSON 输出
```

**规则**：

- `main.rs` 不包含业务逻辑；每个子命令的实现落在 `commands/<name>.rs`。
- 发布链路的五个阶段各自可单测，不允许写成一条 500 行的 `release()`。
- 私钥只以文件路径进入 CLI；`pipeline/sign.rs` 之外不得出现私钥读取。
- COS 签名实现与输出格式分离，签名算法必须有固定向量测试。

---

## 7. 测试夹具 `crates/delta-update-testkit`

```
crates/delta-update-testkit/
├── Cargo.toml                  # publish = false
├── README.md
├── src/
│   ├── lib.rs
│   ├── fs.rs                   # FaultyFs：按操作序号注入错误/abort
│   ├── server.rs               # MockServer：Range 开关、断流、403、5xx、请求记录
│   ├── manifest.rs             # 清单构建器
│   ├── sign.rs                 # 测试密钥与 minisign 签名
│   ├── tree.rs                 # 旧/新目录树构造与一致性断言
│   └── bin/
│       └── crash_harness.rs    # 在指定注入点 abort 的极小子进程
```

**规则**：

- 夹具 crate 只服务测试，不做任何生产逻辑。
- 大体积夹具由 `cargo xtask fixtures` 生成到 `target/test-fixtures/`，不提交。
- 小体积文本夹具可提交到使用方 crate 的 `tests/fixtures/`，并在文件名中体现用途。

---

## 8. 端到端测试 `e2e/`

```
e2e/
├── Cargo.toml                  # publish = false
├── README.md
├── src/
│   ├── main.rs                 # 场景调度与过滤（按名字跑单个 case）
│   ├── harness.rs              # 临时安装/暂存/Journal 目录、MockServer、重启与健康确认
│   └── cases/
│       ├── mod.rs
│       ├── fresh_install.rs
│       ├── delta_update.rs
│       ├── crash_loop_rollback.rs
│       ├── uac_cancelled.rs
│       ├── cross_volume.rs
│       └── rollback_release.rs
└── fixtures/                   # 运行时生成，gitignore
```

**规则**：

- 每个场景一个文件，公共步骤进 `harness.rs`，不得互相调用私有测试函数。
- 场景只通过真实二进制、真实文件系统、MockServer 验证，不直接调用内部函数“模拟成功”。
- 断言必须检查“增量路径成功时全量对象未被请求”，防止测试全绿但实际永远回退。

---

## 9. npm 包 `packages/plugin-api`

```
packages/plugin-api/
├── package.json                # name: tauri-plugin-delta-update-api
├── tsconfig.json
├── src/
│   ├── index.ts                # 公开导出
│   ├── types.ts                # 与协议字段一一对应
│   └── commands.ts             # invoke 封装，禁止业务判断
├── tests/
│   └── commands.test.ts
└── README.md
```

**规则**：

- 包名固定为 `tauri-plugin-delta-update-api`，版本与 Cargo crate 严格同步。
- `types.ts` 的字段名与协议文档保持一致；协议变更先改文档，再改这里。
- 不在前端包里做验签、版本比较、回退决策——这些属于引擎。

---

## 10. 示例 `examples/minimal-tauri-app`

```
examples/minimal-tauri-app/
├── package.json
├── vite.config.ts
├── index.html
├── src/
│   ├── main.ts
│   └── update.ts               # 监听 state、调用 confirm_healthy
├── src-tauri/
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   ├── capabilities/default.json
│   └── src/main.rs
└── README.md
```

**规则**：

- 示例的 `src-tauri` 不加入根 workspace，避免污染主工作区依赖。
- 示例只演示“三步接入 + 一个状态提示”，不堆叠所有高级配置。
- 示例使用的插件通过 `path` 依赖本地 crate，发布前改为版本号验证一次。

---

## 11. 文档 `docs/`

文档按**读者任务**分类，不按文件格式或写作时间分类。每个目录只回答一类问题。

```
docs/
├── README.md                   # 文档索引：读者路径、阅读顺序、维护规则
├── design/                     # 为什么这样设计
│   ├── architecture.md
│   ├── modules.md
│   ├── protocol.md
│   ├── security.md
│   └── permission-model.md
├── guides/                     # 怎么接入、怎么理解目录
│   ├── getting-started.md
│   └── directory-structure.md
├── reference/                  # 精确查阅
│   ├── api.md
│   ├── tech-stack.md
│   └── compatibility.md
├── operations/                 # 发布、运维、容量
│   ├── release.md
│   ├── troubleshooting.md
│   └── capacity.md
└── testing/                    # 验证标准
    └── test-plan.md
```

**文档规则**：

- 文件名统一英文 `kebab-case`，中文只出现在标题和正文；避免空格、括号、版本号进入文件名。
- 每份文档开头用一句话说明“读者与用途”，不写口号式导语。
- 交叉引用使用相对路径链接，不使用“见某文档”这种无法点击的描述。
- 一个主题一个文件；超过约 600 行时按“设计/操作/参考”拆分，而不是继续堆叠。
- `docs/README.md` 是唯一索引，禁止在根 README 里维护第二份文档目录。

---

## 12. 脚本、CI 与发布配置

```
.github/
├── workflows/
│   ├── ci.yml                  # fmt + clippy + test + e2e + deny + docs-links
│   ├── release.yml             # tag 触发，版本一致性 + 发布
│   └── audit.yml               # 定时依赖审计
├── ISSUE_TEMPLATE/
│   ├── bug.yml
│   └── feature.yml
└── pull_request_template.md

scripts/
└── windows/
    └── sign-authenticode.ps1   # 仅 Windows 签名，需 signtool
```

**落位规则**：

- 跨平台自动化放 `xtask`，例如 `cargo xtask ci`、`cargo xtask docs-check`、`cargo xtask version-sync`、`cargo xtask fixtures`。
- `scripts/` 只放必须调用原生工具的脚本；一个脚本只做一件事，文件名体现动作与平台。
- CI 中每个独立验证（故障注入、插件生命周期、文档链接、版本一致性）是独立 job，不淹在一条 `cargo test` 里。
- 发布凭证只存在于 CI secret，仓库内不出现任何密钥文件。

---

## 13. 命名与文件规范

| 对象             | 规则                                                             | 示例                                      |
| ---------------- | ---------------------------------------------------------------- | ----------------------------------------- |
| Rust crate       | `kebab-case`                                                     | `delta-update-engine`                     |
| Rust 目录/模块   | `snake_case`                                                     | `journal/`, `rel_path.rs`                 |
| Rust 类型/trait  | `UpperCamelCase`                                                 | `PreparedUpdate`, `Fetcher`               |
| Rust 函数/变量   | `snake_case`                                                     | `recover_and_cleanup`                     |
| 测试文件         | 场景 + 类型                                                      | `journal_fault_injection.rs`              |
| npm 包           | `kebab-case`                                                     | `tauri-plugin-delta-update-api`           |
| TS 文件          | `kebab-case`                                                     | `commands.ts`, `update-state.ts`          |
| 文档             | `kebab-case.md`                                                  | `directory-structure.md`                  |
| 夹具             | `case-<name>/` 或 `<purpose>.bin`                                | `case-cross-volume/`                      |
| 生成文件         | 放入 `generated/` 或构建目录，文件头标注 `@generated`             | `permissions/autogenerated/`              |

**禁止的命名**：

- `utils.rs`、`helpers.rs`、`common.rs`、`misc.rs`、`types.rs` 作为万能收集器。
- `new/`、`old/`、`backup/`、`tmp/`、`temp/`、`other/`、`stuff/`。
- `mod.rs` 里堆业务实现。
- 同一概念出现多种拼写（`signing-key` / `sign_key` / `signKey` 混用）。

---

## 14. 新增文件落位决策树

```
要新增一个文件？
│
├─ 是生产 Rust 代码？
│   ├─ 属于清单/验签/下载/补丁/暂存/日志/提交/回滚/状态？ → crates/delta-update-engine/src/<领域>/
│   ├─ 属于 Tauri 生命周期/命令/事件/权限？               → crates/tauri-plugin-delta-update/src/
│   └─ 属于发布流水线/CLI 输出/发布器？                    → crates/delta-update-cli/src/
│
├─ 是测试夹具或故障注入工具？ → crates/delta-update-testkit/src/
├─ 是跨进程端到端场景？       → e2e/src/cases/
├─ 是前端调用封装或类型？     → packages/plugin-api/src/
├─ 是仓库自动化？             → xtask/src/tasks/
├─ 是必须调用原生工具的脚本？ → scripts/<platform>/
├─ 是文档？                   → docs/<design|guides|reference|operations|testing>/
└─ 是构建产物/临时文件？      → target/ 或 dist/（不提交）
```

**判断标准**：如果一个文件无法用一句话说明“它属于哪个职责、依赖谁、被谁依赖”，说明目录设计或文件职责还不清楚，先解决归属，再写代码。

---

## 15. 模块拆分与文件大小

- **按职责拆分，不按行数机械切割**。一个模块可以因为“解析”和“I/O”混在一起而拆分，但不能因为“超过 300 行”就随机切成两半。
- 单个 Rust 文件建议控制在 **300–500 行**；超过 800 行必须能说明为什么不能拆（例如大段测试数据、生成的常量表）。
- 一个目录内文件数建议不超过 **12 个**；超过时按子职责建子目录。
- 公共类型放在它所属的领域模块；只有真正跨领域且无行为的数据结构才允许单独的 `model.rs`。
- 一个函数只做一件事；公开 API 的每个参数都要能回答“为什么调用方必须提供它”。

---

## 16. 禁止事项

1. 不在仓库根放 `dist/`、`target/`、`node_modules/`、日志、截图、压缩包、临时脚本。
2. 不提交任何私钥、`.env`、CI token、预签名 URL。
3. 不把 Tauri 类型、COS 类型、具体差分算法类型带进引擎公开 API。
4. 不把算法 crate 的类型暴露给插件或前端；只暴露协议里的字符串策略标识。
5. 不建“按文件类型”划分的万能目录（`models/`、`services/`、`controllers/`）。
6. 不在 `src/` 里放测试夹具、样例产物或大体积二进制。
7. 不用 `#[cfg(test)]` 改写生产控制流；故障注入通过 trait 实现或 feature 提供。
8. 不让两个 crate 互相依赖；需要共享时把公共部分下沉到引擎或 testkit。
9. 不在 docs 根目录继续堆文件；新文档必须进入分类目录。
10. 不提交生成文件而不标注来源；生成文件必须能由 `cargo xtask` 复现。

---

## 17. 目录整洁验收清单

发布或合并前逐项确认：

- [ ] 根目录只有第 2 节列出的标准文件与目录。
- [ ] `cargo metadata` 只显示预期的 workspace 成员，示例的 `src-tauri` 未混入。
- [ ] `crates/delta-update-engine` 的 `Cargo.toml` 不含 `tauri`、`minisign`、COS 相关依赖。
- [ ] 插件 crate 不含 `reqwest`、`qbsdiff`、`hdiffpatch`、`zucchini` 的直接依赖。
- [ ] 所有新增模块都能在本文的目录树中找到归属。
- [ ] `docs/` 下没有根级散落文档，所有链接可点击且有效。
- [ ] `cargo xtask ci` 全绿，`cargo deny check` 通过。
- [ ] `git status` 在 `cargo xtask fixtures` 之后保持干净（夹具生成到 target/）。
- [ ] 没有 `utils.rs` / `helpers.rs` / `common.rs` 作为业务代码收集器。
- [ ] 没有未标注的生成文件、没有密钥、没有构建产物。

---

**本文的核心判断**：目录结构不是“文件放哪儿”的美学问题，而是**依赖方向、变更边界与测试策略的物理表达**。一个文件如果无法从路径判断它的职责与依赖，最终一定会演变成跨层调用和无法测试的耦合。先把目录定对，再写第一行代码。
