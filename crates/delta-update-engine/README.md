# delta-update-engine

文件级增量更新引擎：清单解析、minisign 验签、流式下载、补丁应用、暂存、提交日志、回滚与状态机。纯 Rust，不依赖 Tauri，可独立用于其他桌面应用。

核心流程已实现，并通过契约测试、故障注入与端到端测试；设计边界见 [设计文档](../../docs/README.md)。

## 模块

| 目录        | 职责                                             |
| ----------- | ------------------------------------------------ |
| `manifest`  | 清单解析、字段校验、文件差异                     |
| `crypto`    | SHA-256 流式校验、detached 签名验证              |
| `fetch`     | 清单/签名/对象下载、Range 续传、预签名 URL       |
| `patch`     | `.tdp` 容器、流式应用、构建侧差分                |
| `staging`   | 只物化变化文件、暂存校验                         |
| `journal`   | 提交日志格式、幂等重放、跨进程锁                 |
| `apply`     | 替换进程入口、`PrivilegedWriter`                 |
| `rollback`  | 日志求逆与回滚                                   |
| `state`     | 状态枚举、合法迁移、事件 payload                 |
| `fs`/`path` | 文件系统抽象、路径校验、安装根解析               |

## 开发

```bash
cargo test -p delta-update-engine
cargo check -p delta-update-engine --all-targets
```

约束：引擎公开 API 不出现 `tauri`、COS 类型或私钥类型。详见 [目录结构规范](../../docs/guides/directory-structure.md) 与 [API 参考](../../docs/reference/api.md)。
