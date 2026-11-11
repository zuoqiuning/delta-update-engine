# delta-update-testkit

测试夹具库，不发布。为引擎、插件与 e2e 提供可复用的故障注入与数据构造能力。

| 模块          | 提供                                             |
| ------------- | ------------------------------------------------ |
| `fs`          | `FaultyFs`：按操作序号注入错误或 abort           |
| `server`      | `MockServer`：Range、403、5xx、断流              |
| `manifest`    | 清单构建器                                       |
| `sign`        | 测试密钥与签名                                   |
| `tree`        | 目录树构造与一致性断言                           |
| `bin/crash_harness` | 在指定注入点 abort 的极小子进程            |

```rust
use delta_update_testkit::FaultyFs;

let fs = FaultyFs::new(Some(3), None);
```

大体积夹具由 `cargo xtask fixtures` 生成到 `target/test-fixtures/`，不提交。

`MockServer` 已支持对象映射、Range（206）、可配置的 5xx/403 注入；签名夹具在运行时生成临时 minisign 密钥对。断流与慢响应随 L4 测试补齐。
