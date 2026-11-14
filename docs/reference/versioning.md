# 版本与兼容承诺

本页说明哪些变化会破坏兼容性、客户端如何拒绝过新/过旧的输入，以及密钥轮换的时间线。字段级细节见 [版本兼容与迁移](compatibility.md)。

## 1. Rust crate

- 版本遵循 SemVer。
- `0.x.y`：`x` 增加可以包含 breaking change，`y` 只包含修复与兼容新增。
- `1.0.0` 之后：breaking change 只出现在 `x`。
- 所有 workspace crate 与 npm 包共用同一个版本号（`cargo xtask version-sync` 校验）。

候选版本需要在依赖里写完整版本（SemVer 的 `^1.0` 不匹配 `1.0.0-rc.3`），例如 `delta-update-engine = "1.0.0-rc.3"`；正式版发布后再改成 `version = "1.0"`，并提交 `Cargo.lock`。

## 2. Manifest schema

- 清单的 `schema` 字段**只增不减**：新增可选字段不提升 `schema`；删除字段、改变字段语义或改变校验规则必须提升。
- `min_client_schema` 表示发布者要求的最低客户端 schema。客户端遇到高于自身支持的 `schema` 时返回 `E_MANIFEST_SCHEMA_TOO_NEW`，`action` 为 `update_client`。
- **拒绝未来 schema 是永久承诺**：这保证旧客户端不会误解新清单，而不是“等客户端升级后自动放宽”。发布者需要兼容旧客户端时，降低 `schema` 或提供双清单。
- 客户端向后兼容旧 schema 的读取；新增字段一律带默认值。

## 3. Journal 与 `config_version`

- Journal 自带格式版本与校验和；遇到更高版本返回 `E_JOURNAL_FORMAT_TOO_NEW`，`action` 为 `update_client`，不会尝试猜测格式。
- Journal 是**会被执行的写操作记录**：升级客户端时必须继续能读旧 Journal，直到旧版本的回滚窗口结束。
- `config_version` 低于当前实现返回 `E_CONFIG_MIGRATION_REQUIRED`；高于当前实现返回 `E_CONFIG_INVALID`。字段新增必须带 `#[serde(default)]`。

## 4. 补丁策略

- `Strategy` 是开放枚举：未知策略不会被拒绝，客户端回退全量并记录警告。
- 已实现策略（当前 `qbsdiff+zstd`）的容器格式变更需要新的 `strategy_id`，不复用旧 id。
- 发布端 `--require-delta` 可以在 CI 中阻止“本应补丁却回退全量”的发布。

## 5. 密钥轮换

- `signing_public_keys` 支持多把公钥并行验证。
- 轮换步骤：先加入新公钥（旧公钥保留）→ 用新私钥发布一个版本 → 至少保留一个 minor 周期 → 移除旧公钥。
- **移除旧公钥是 breaking change**：仍信任旧公钥的客户端会拒绝新清单。需要发布 major（或 `0.x` 的 `x`）版本并给出迁移窗口。
- 已发布的清单不重新签名；轮换只影响之后发布的版本。

## 6. 1.0 的稳定承诺

`1.0.0` 起以下视为稳定；删除、重命名或改变语义需要 major：

- `EngineConfig` 字段：新增必须带默认值；删除或改变含义属于 breaking。
- `EngineState` 变体与状态名：枚举是 `#[non_exhaustive]`，`1.x` 可以新增变体，下游必须保留 `_` 分支。
- `ErrorCategory` / `ErrorAction`：同为 `#[non_exhaustive]`；新增取值属于兼容变更，删除或改变含义属于 breaking。
- manifest `schema`：只增不减；新增可选字段不提升版本。
- Journal `format`：升级客户端必须能读旧格式，直到旧版本的回滚窗口结束。
- `UpdateRecord` 的 JSONL 字段：只新增，不重命名；提升 `v` 才允许破坏性变更。

`1.x` 允许：新增带默认值的配置/清单字段、新增状态变体与错误取值、新增命令与 CLI 参数、新增可选事件字段。**不要**在 `1.x` 改变既有字段的单位或含义。

## 7. 变更检查表

提交 PR 前确认：

- 改了清单字段 → 是否需要提升 `schema` 或 `min_client_schema`？
- 改了 Journal 格式 → 是否需要提升格式版本并保留旧格式读取？
- 改了 `Config` 字段 → 是否加了 `#[serde(default)]` 并更新 `docs/reference/api.md`？
- 加了命令或 CLI 参数 → 是否更新 `docs/cli-contract.md` 并让 `cargo xtask contract-check` 通过？
