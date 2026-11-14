# Security Policy

## 报告漏洞

**不要开公开 issue。** 请使用 GitHub Security Advisories（仓库 → Security → Report a vulnerability）。这是唯一渠道；如果没有响应，请在 issue 中只写“security report sent via advisories”，不要附细节。

请在报告中包含：受影响的版本、复现步骤、影响范围，以及是否涉及签名校验、Journal 执行或提权路径。我们会在 **72 小时内确认收到**，并在修复发布后致谢（除非你要求匿名）。

## 支持范围

| 版本 | 安全修复 |
| --- | --- |
| 最新 minor | 接收，发补丁版本 |
| 上一个 minor | 接收，发补丁版本 |
| 更早版本 | 只发布 advisory，不提供补丁，请先升级 |

## 披露流程

1. **确认**：72 小时内回复收到，7 天内给出初步定级（严重/高/中/低）与是否受影响。
2. **修复**：确认后 14 天内发布修复版本；复杂问题会在 advisory 中说明预计时间，不无限期静默。
3. **私有 advisory**：修复准备期间在 GitHub Security Advisories 建立草稿，邀请报告者参与验证。
4. **公开 advisory**：修复版本发布后同步公开，包含受影响版本范围、影响、缓解措施与修复版本；必要时申请 CVE。
5. **致谢**：默认列出报告者，可按要求匿名。

已公开的 advisory 与受影响版本范围见 [安全公告](docs/security/advisories.md)。自查时先运行 `delta-update version`（或查看应用内的引擎版本），再对照公告表中的版本区间。

## 信任边界（先读这些再报告）

本项目的安全模型是“签名先于落盘、失败关闭”，以下边界是**已知设计**，不是漏洞：

- 客户端只信任 `signing_public_keys` 验证通过的清单；`latest.json` 与版本化清单必须带有效 `.minisig`。
- `commit.meta.json` 不是独立签名，它把 Journal 绑定到已验签清单的哈希；信任来源仍是清单签名。
- `old_hashes.json` 只用于检测备份损坏，不构成对备份目录的独立信任。
- worker 要求 `install_root` 精确等于自身可执行文件目录；per-machine 安装的提权恢复只接受同一二进制。
- 任何“无密钥也可恢复”的路径都会被拒绝；`Engine::new` 始终要求 Journal 绑定到已验签清单，无签名恢复只存在于 `#[doc(hidden)]` 的测试/迁移构造函数，且插件与 CLI 从不调用。

细节见 [信任模型](docs/security/trust-model.md) 与 [故障排查](docs/operations/troubleshooting.md)。

## 不在范围内

- 已获得本机管理员权限的攻击者（提权 worker 的前提就是管理员授权）。
- 对象存储或 CDN 账号被攻破（请在云侧轮换密钥并重新发布）。
- 用户手动修改安装目录中的文件后导致的哈希不匹配（会走全量修复或失败关闭）。
