# Delta Update Engine 构建与发布操作手册

> 本文面向发布工程师与 CI 维护者。构建与发布阶段的每一个操作，都会直接影响客户端能否安全、正确地完成更新。发布事故往往不来自引擎代码，而来自流程：私钥被写进日志、清单先于补丁上传、回滚素材被提前清理。

## 读哪一份

| 你要做的事 | 文件 |
| --- | --- |
| 配置私钥、执行 `release`、理解原子顺序与回滚发布 | [release-guide.md](release-guide.md) |
| 上线前逐项核对、定位常见发布事故 | [release-checklist.md](release-checklist.md) |
| Windows 签名、NSIS 验收、crates.io 与 npm 发布 | [release-packaging.md](release-packaging.md) |

**关键顺序约束**：补丁和全量对象**必须先于**清单到达 COS；签名**必须先于**对应清单到达 COS。客户端只有在清单指向某对象时才会去拉取它，顺序颠倒会产生“清单已更新但对象 404”或“清单已到但签名缺失”的窗口。这是原子发布的核心规则，详见 [release-guide.md §4](release-guide.md#4-原子发布顺序与对象存储操作)。
