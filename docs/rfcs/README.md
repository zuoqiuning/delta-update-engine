# RFC：协议与安全演进流程

任何改变**线上可观察契约**的提案先写成 RFC，评审通过后再写契约测试，最后实现。适用于：manifest schema、签名格式与信任根、Journal 格式、worker 协议、事件与错误码、发布侧文件布局。

## 何时需要 RFC

| 变更 | 需要 RFC |
| --- | --- |
| 新增可选 manifest 字段、旧客户端忽略 | 否，按 `docs/reference/versioning.md` 走 |
| 签名格式、信任根、密钥轮换协议 | 是 |
| Journal / 提交协议格式变化 | 是 |
| 前端事件字段的语义变化 | 是 |
| 纯文档、诊断、测试、内部重构 | 否 |

## 流程

1. 复制下方模板到 `docs/rfcs/NNNN-<slug>.md`，编号从 `0001` 递增，不复用。
2. 状态先写 `Draft`，在 PR 中讨论；同意后改 `Accepted` 并给出目标版本窗口。
3. 实现顺序固定：RFC → 契约测试（`tests/*_contract.rs`）→ 实现 → 文档索引更新。
4. 实现完成且发布后改 `Implemented`；被取代时改 `Superseded by NNNN`，不删除文件。

## 模板

```markdown
# RFC NNNN：标题

- 状态：Draft | Accepted | Implemented | Superseded by NNNN
- 目标版本：1.1.0
- 相关：docs/... 与 tests/...

## 动机

要解决的真实问题，以及不解决的后果。

## 协议变更

字段、文件、顺序、错误码。给出旧客户端看到的内容。

## 兼容性

旧客户端 / 新客户端 / 混合发布期各自的行为，明确哪条路径必须保持可用。

## 迁移

发布者分几步切换，客户端分几个版本跟进。

## 回滚

发布或客户端回滚后，磁盘与远端留下什么，如何复原。

## 测试计划

契约测试文件与用例清单；指出哪些用例在实现前就应失败。
```

## 索引

- [RFC 0001：多签名与密钥轮换](0001-multi-signature-rotation.md)（Accepted，目标 1.1.0）
