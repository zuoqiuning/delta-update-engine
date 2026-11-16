# Contributing

## 环境

- Rust stable（见 `rust-toolchain.toml`）
- Node.js 24+ 与 pnpm 12+
- Windows 10+（唯一支持形态；Plugins/CLI 不再接受 Linux/macOS 自更新）

## 开始

```bash
cargo check --workspace
cargo test --workspace
pnpm install
pnpm typecheck
```

## 目录规则

- 先读 [目录结构规范](docs/guides/directory-structure.md)，再决定新文件放哪里。
- 引擎不得依赖 Tauri；插件不得直接依赖差分、HTTP 或签名私钥实现。
- 不新增 `utils.rs`、`helpers.rs`、`common.rs` 这类万能收集器。
- 测试夹具放 `crates/delta-update-testkit` 或对应 crate 的 `tests/`，不得进入生产路径。

## 提交前

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo doc -p delta-update-engine -p tauri-plugin-delta-update --no-deps
cargo test --workspace
cargo run -p delta-update-e2e
cargo xtask docs-check
cargo xtask version-sync
```

## 设计变更

任何字段、命令、事件、错误码或目录结构的变更，必须先更新 `docs/`，再改代码。协议文档是唯一契约。

## 分支与发布窗口

- `main` 始终可发布；功能分支从 `main` 切出，PR 合回 `main`。
- `1.0.0-rc.x` 只收**修复**：bug、文档、诊断、测试。公开 API 的新增、签名/清单/Journal 格式的变化一律不进入 rc。
- 公开 API 新增进 **minor**（`1.1.0`）；破坏性变更进 **major**。错误码、事件字段、CLI 参数按 `docs/reference/versioning.md` 的承诺处理。
- 弃用：公开 API 标记 `#[deprecated]` 后至少保留一个 minor，再在下一个 minor 移除。
- 冻结期内确需改协议时，先写 `docs/rfcs/` 下的 RFC（动机、兼容性、迁移、回滚、测试计划），评审通过后再写契约测试，最后实现。

## 契约测试

- 协议、签名、配置、CLI 输出与前端事件的变更，先写 `tests/*_contract.rs` 或 `packages/plugin-api` 的用例，再改实现。
- 契约测试断言的是**外部可观察行为**（错误码、退出码、事件字段、文件状态），不是内部函数调用顺序。
- 故障注入测试必须断言注入真的命中（`fired`/`faults_served`），否则“注入没发生”会被误判为通过。
