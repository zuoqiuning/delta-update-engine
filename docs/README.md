# 文档索引

本目录是 Delta Update Engine（`delta-update-engine`）的唯一文档入口。文档按读者任务分类，不按写作时间或文件格式分类。

## 接入（怎么用）

- [开发者接入指南](guides/getting-started.md)：三步接入、前端事件、排查清单。
- [目录结构规范](guides/directory-structure.md)：仓库布局、依赖方向、新增文件落位规则。

## 参考（精确查阅）

- [API 参考](reference/api.md)：公开类型、trait、命令、错误映射与平台矩阵。
- [技术栈](reference/tech-stack.md)：依赖选型、版本基线与验证项。
- [版本兼容与迁移](reference/compatibility.md)：schema、密钥、策略与配置的演进规则。

## 运维（发布与排障）

- [构建与发布](operations/release.md)：签名、原子上传、回滚发布与检查清单。
- [故障排查](operations/troubleshooting.md)：日志、状态、错误码与手动清理。
- [性能与容量](operations/capacity.md)：补丁体积、内存、磁盘与流量估算。

## 测试（验收标准）

- [测试计划与故障注入矩阵](testing/test-plan.md)：L1–L4、Journal/Committer/Rollback 场景与发布门禁。

## 阅读顺序

1. 第一次接触项目：开发者接入指南 → 目录结构规范。
2. 准备发布：构建与发布 → 故障排查。
3. 排查线上问题：故障排查 → 测试计划中的对应场景。

## 维护规则

- 新增文档必须进入 `guides/`、`reference/`、`operations/` 或 `testing/`，不得堆在 `docs/` 根目录。
- 文件名使用英文 `kebab-case`；中文只出现在标题和正文。
- 交叉引用使用相对路径链接，确保可点击。
- 任何协议、命令、错误码或目录结构变更，先改对应文档，再改代码。
