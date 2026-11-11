# Contributing

## 环境

- Rust stable（见 `rust-toolchain.toml`）
- Node.js 24+ 与 pnpm 12+
- Windows 10+（完整功能）；Linux AppImage 与 macOS 未签名包为有限支持

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
cargo test --workspace
cargo run -p delta-update-e2e
cargo xtask docs-check
cargo xtask version-sync
```

## 设计变更

任何字段、命令、事件、错误码或目录结构的变更，必须先更新 `docs/`，再改代码。协议文档是唯一契约。
