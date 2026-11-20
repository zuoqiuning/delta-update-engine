# Delta Update Engine API 参考

> 引擎、插件与 CLI 的公开接口索引。任何实现与文档不一致时以文档为准；接口变更必须先更新文档，再改代码。

## 读哪一份

| 你要查的内容 | 文件 |
| --- | --- |
| `delta-update-engine` 的类型、trait 与稳定性策略 | [api-engine.md](api-engine.md) |
| 错误枚举、wire payload 与错误码映射 | [api-errors.md](api-errors.md) |
| Tauri 命令、事件、权限集与前端 SDK | [api-plugin.md](api-plugin.md) |
| CLI 子命令、参数、feature flags 与平台矩阵 | [api-cli.md](api-cli.md) |

接口契约的边界：

- 引擎的公开类型与错误码按 SemVer minor 演进；`#[non_exhaustive]` 枚举新增变体不算破坏。
- 插件命令名、事件名与权限标识是契约；命令返回字段只增不改，旧字段语义不变。
- 前端 SDK 的 `types.ts` 与 Rust wire 类型由 `types_contract` 测试锁定。
- CLI 参数是公开接口，`--json` 输出以 [cli-contract.md](../cli-contract.md) 为准，人类可读文案不承诺稳定。
