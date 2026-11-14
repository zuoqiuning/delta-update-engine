## 变更说明

<!-- 一句话说明这个 PR 做了什么，以及为什么。 -->

## 兼容性

- [ ] 未修改 manifest schema（若修改，已提升 `schema` / `min_client_schema` 并更新 `docs/reference/versioning.md`）
- [ ] 未修改 Journal 格式（若修改，已提升格式版本并保留旧格式读取）
- [ ] 未修改 `Config` 字段（若修改，已加 `#[serde(default)]` 并更新 `docs/reference/api.md`）
- [ ] 未新增命令或 CLI 参数（若新增，已更新 `docs/cli-contract.md`，`cargo xtask contract-check` 通过）
- [ ] 未改变退出码语义

## 验证

- [ ] `cargo xtask ci`
- [ ] `cargo xtask docs-check`
- [ ] `cargo xtask version-sync`
- [ ] `pnpm typecheck && pnpm test`（若修改前端包）
- [ ] 新路径有测试覆盖（故障注入 / 真实退出路径优先）

## 风险与回滚

<!-- 这个改动最坏情况下会怎样？如何回滚？ -->
