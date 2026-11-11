# delta-update-e2e

跨进程端到端场景：真实文件系统、真实引擎、MockServer 与真实 CLI 发布管线。每个场景独立构造安装目录，进程内模拟“应用重启”，覆盖提交、恢复、回滚与健康确认。

```bash
cargo run -p delta-update-e2e              # 跑全部场景
cargo run -p delta-update-e2e -- delta_update
cargo run -p delta-update-e2e -- --list
```

| 场景                   | 覆盖                                                     |
| ---------------------- | -------------------------------------------------------- |
| `fresh_install`        | 检查更新 → 后台准备（增量）→ 提交 → 重启 → 健康确认       |
| `delta_update`         | 无更新零下载；5xx 重试；全量对象未被请求；删除与未变化文件 |
| `crash_loop_rollback`  | 连续两次未确认后第三次启动回滚，旧版本可用且跳过坏版本   |
| `uac_cancelled`        | 取消提权后撤销提交点、保留 PREPARED，下次启动复用并重试成功 |
| `cross_volume`         | 暂存目录与安装目录分属不同卷时的替换与清理（同卷则退化为 rename） |
| `rollback_release`     | 真实 `release --rollback` 产物 → 客户端接受降级并应用     |

断言规则：增量成功时 **全量对象不得被请求**；目录最终状态必须与目标清单一致，且 `.old`、`.trash`、staging 与 Journal 清理干净。`delta_update` 还会打印 prepare 耗时并断言补丁/全量下载比低于 50%，作为轻量性能基线。

运行时产物写入 `fixtures/`，已被 `.gitignore` 忽略。验收标准见 [测试计划](../docs/testing/test-plan.md)。
