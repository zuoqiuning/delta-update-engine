## 变更内容

<!-- 一句话说明改了什么、为什么改 -->

## 协议与目录

- [ ] 若涉及字段、命令、事件、错误码或目录结构，已先更新 `docs/`
- [ ] 未新增 `utils.rs` / `helpers.rs` / `common.rs` 万能收集器
- [ ] 新增依赖符合 `docs/reference/tech-stack.md`

## 验证

- [ ] `cargo xtask ci`
- [ ] `cargo xtask docs-check`
- [ ] `cargo xtask version-sync`
- [ ] `pnpm typecheck && pnpm build && pnpm test`
- [ ] 涉及安装/替换/回滚时，已补充或更新对应 e2e 场景
