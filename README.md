# Delta Update Engine（增量更新内核）

面向 Tauri 2 的文件级增量更新内核：只下载变化的文件，应用退出后由独立替换进程完成替换，下次启动即为新版本；失败可回滚，签名先于落盘。

> 核心 crate 为 `delta-update-engine`，Tauri 接入层为 `tauri-plugin-delta-update`，发布侧为 `delta-update-cli`。当前状态：引擎、插件生命周期与 CLI 发布管线均已实现；Windows per-user/per-machine 更新、启动期提权恢复、崩溃回滚与端到端门禁均已通过。

## 仓库结构

```
crates/delta-update-engine         纯 Rust 引擎（无 Tauri 依赖，可被任意 Rust 程序使用）
crates/tauri-plugin-delta-update   Tauri 2 插件
crates/delta-update-cli            构建侧发布 CLI
crates/delta-update-testkit        测试夹具（不发布）
packages/plugin-api                前端 invoke/类型封装
examples/minimal-tauri-app         最小接入示例
e2e                                跨进程端到端测试
real-test                          真实更新演练（本地 HTTP + 真实 CLI/引擎）
xtask                              仓库自动化
docs                               设计、接入、参考、运维与测试文档
```

目录与依赖规则见 [目录结构规范](docs/guides/directory-structure.md)。

## 开发命令

```bash
cargo check --workspace
cargo test --workspace
cargo xtask ci
pnpm install
pnpm typecheck

# 导出关键源码到 项目完整源码.md（交接/分享用）
pnpm export:source

# 真实演练：构造 v1/v2 两个软件目录，生成补丁并实际更新（需要 Python 3 提供本地静态 HTTP 服务）
pwsh -ExecutionPolicy Bypass -File real-test/run.ps1
```

## 文档

- [文档索引](docs/README.md)
- [架构设计](docs/design/architecture.md)
- [协议与数据格式](docs/design/protocol.md)
- [开发者接入](docs/guides/getting-started.md)
- [测试计划](docs/testing/test-plan.md)

## 许可

MIT
