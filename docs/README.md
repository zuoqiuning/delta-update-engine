# 文档索引

本目录是 Delta Update Engine（`delta-update-engine`）的唯一文档入口。文档按读者任务分类，不按写作时间或文件格式分类。

## 接入（怎么用）

- [开发者接入指南](guides/getting-started.md)：四步接入与前五节；构建发布与排查见[后续篇](guides/getting-started-operations.md)，进阶配置与验收见[参考篇](guides/getting-started-reference.md)。
- [CI 发布教程](tutorials/ci-release.md)：从生成密钥到 `release --pipeline` 与观察窗口。
- [目录结构规范](guides/directory-structure.md)：仓库布局、依赖方向、新增文件落位规则。

## 参考（精确查阅）

- [API 参考](reference/api.md)：索引；引擎见[api-engine](reference/api-engine.md)、错误见[api-errors](reference/api-errors.md)、插件见[api-plugin](reference/api-plugin.md)、CLI 见[api-cli](reference/api-cli.md)。
- [CLI 契约](cli-contract.md)：退出码、`--json` 信封与发布报告 schema（CI 依赖）。
- [技术栈](reference/tech-stack.md)：依赖选型、版本基线与验证项。
- [版本与兼容承诺](reference/versioning.md)：crate SemVer、schema 承诺、密钥轮换。
- [版本兼容与迁移](reference/compatibility.md)：schema、密钥、策略与配置的演进规则。

## 设计（为什么这样做）

- [架构与不变量](design/architecture.md)：模块边界、执行顺序与失败关闭原则。
- [协议 RFC 流程与索引](rfcs/README.md)：需要先写 RFC 的变更、评审与实现顺序。
- [模块职责](design/modules.md)：每个模块负责什么、依赖方向如何。
- [协议与清单](design/protocol.md)：索引；数据格式见[protocol-data](design/protocol-data.md)，运行时见[protocol-runtime](design/protocol-runtime.md)，配置与演进见[protocol-config](design/protocol-config.md)。
- [安全模型](design/security.md)：信任边界、签名绑定与提权路径。
- [信任模型](security/trust-model.md)：攻击者能力、逐文件约束与残余风险。
- [权限模型](design/permission-model.md)：`default`/`manage`/`monitor` 的取舍。
- [崩溃循环分级](design/crash-loop-grading.md)：三种启动的区分与阈值取舍。
- [灰度发布设计](design/gray-release.md)：channel、client_id 路由与清单内 percent 的取舍（仅设计）。
- [机器级安装](design/per-machine-install.md)：启动恢复的提权提示，以及 `install_root` 是否可放宽的取舍（RFC）。

## 运维（发布与排障）

- [构建与发布](operations/release.md)：索引；流程见[release-guide](operations/release-guide.md)，检查清单见[release-checklist](operations/release-checklist.md)，打包与 crates/npm 发布见[release-packaging](operations/release-packaging.md)。
- [故障排查](operations/troubleshooting.md)：日志、状态、错误码与手动清理。
- [性能与容量](operations/capacity.md)：补丁体积、内存、磁盘与流量估算。
- [安全公告](security/advisories.md)：已公开 advisory 与受影响版本范围。

## 测试（验收标准）

- [测试计划与故障注入矩阵](testing/test-plan.md)：L1–L4、Journal/Committer/Rollback 场景与发布门禁。

## 集成（接入生态）

- [Sentry 上报](integrations/sentry.md)：把 `UpdateRecord` 转成事件、面包屑与成功/回滚指标。

## 阅读顺序

1. 第一次接触项目：开发者接入指南 → 目录结构规范。
2. 准备发布：构建与发布 → 故障排查。
3. 排查线上问题：故障排查 → 测试计划中的对应场景。

## 维护规则

- 新增文档必须进入 `guides/`、`tutorials/`、`reference/`、`operations/`、`testing/`、`design/`、`security/`、`integrations/`、`rfcs/` 或 `reviews/`，不得堆在 `docs/` 根目录。
- `reviews/` 只追加带日期的历史评审快照，不改写、不拆分；其他文档超过约 600 行时拆分为索引 + 分册。
- 文件名使用英文 `kebab-case`；中文只出现在标题和正文。
- 交叉引用使用相对路径链接，确保可点击。
- 任何协议、命令、错误码或目录结构变更，先改对应文档，再改代码。
