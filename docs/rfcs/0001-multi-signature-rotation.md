# RFC 0001：多签名与密钥轮换

- 状态：Accepted
- 目标版本：1.1.0
- 相关：[版本与兼容承诺](../reference/versioning.md)、[安全模型](../design/security.md)、[RFC 流程](README.md)

## 动机

信任根是客户端二进制内嵌的公钥集合，运行期不可写（见 [安全模型 §3.1](../design/security.md#31-安装形态--信任根)）。这消除了本地篡改，但也让**轮换窗口**成为唯一无法回避的痛点：

- 客户端 v1 内嵌 `{A}`，服务端若直接改用 `{B}` 签名，v1 会因找不到可信密钥而停止更新，且这类客户端无法通过更新通道自救。
- 先发一个内嵌 `{A, B}` 的 v2 客户端、等覆盖率上来再切到 `B` 的做法可行，但切换动作只能整体生效：同一份清单不能同时被 `A` 和 `B` 验证。

多签名的目的因此不是“多信任源”，而是**让新旧密钥在同一份清单上重叠**：服务端同时用 `A` 与 `B` 签名，v1 客户端用 `A` 通过，v2 客户端（内嵌 `{A, B}`）两者都接受。轮换从“必须等覆盖率”变成“可以并行推进”。

## 协议变更

- 主签名保持 `manifest.json.minisig`，语义不变：**旧客户端只读它**。
- 附加签名写到 `manifest.json.minisig.<key_id>`，`key_id` 是公钥 key id 的小写十六进制。
- manifest 增加可选字段 `signing_key_ids: ["<id>", ...]`，列出本次发布的附加签名；缺失表示只有主签名。
- 客户端：先验主签名；未命中时按 `signing_key_ids` 逐个拉取附加签名，**任一通过即接受**，并把命中的 key id 记入 `UpdateRecord` 与诊断包。
- 附加签名文件缺失、无法解析或 key id 不在信任集内时**忽略该条**，不回退为失败，除非所有签名都失败。

## 兼容矩阵

这张表就是 S4 的契约测试来源。

| 客户端 | manifest 的签名 | 结果 |
| --- | --- | --- |
| 旧（只读 `.minisig`） | 主签名 `A` + 附加 `B` | 通过（用 `A`） |
| 旧（只读 `.minisig`） | 只有主签名 `B`，客户端只信任 `A` | 拒绝 `E_SIGNATURE_NO_TRUSTED_KEY`，更新提示升级客户端 |
| 旧（只读 `.minisig`） | 只有 `.minisig.<id>`，主签名缺失 | 拒绝（无主签名即无清单） |
| 新 | 主签名 `A` + 附加 `B`（信任集 `{A,B}`） | 通过，记录命中 key id |
| 新 | 只有主签名（旧格式） | 通过，行为与 1.0 相同 |
| 新 | 全部签名无效 | 拒绝 `E_SIGNATURE_NO_TRUSTED_KEY` |

## S7 发布原子性

**当前实现（1.0.0-rc.x，事实记录）**：`publish_manifests` 的 PUT 顺序是“version 签名 → latest 签名 → version manifest → latest manifest”；客户端 `check()` 并发拉取 manifest 与 `.minisig` 后只验一次，签名失败不重试（`engine.rs` 直接返回 `EngineError::Signature`）。

这会产生一个真实窗口：发布 v2 时，`latest.json.minisig` 已写成 v2 的签名，而 `latest.json` 仍指向 v1。两者之间的读取会得到“v1 manifest + v2 signature”，验签必然失败。反过来先写 manifest 也一样（“v2 manifest + v1 signature”）。**根因是 `latest.json` 与 `latest.json.minisig` 是两个独立对象，任何先后顺序都有一个不匹配窗口**，这不是实现细节。

多签名只是把对象数从 2 变成 2+N，窗口本身不变。S4 的发布顺序因此是：所有附加签名 → 主签名 → version manifest → latest 签名 → latest manifest，把“附加签名缺失”限制在无害区间。

### 方案 A：单对象信封（1.1.0 目标）

把 `latest.json` 与签名合成一个对象，单次 PUT 原子生效：

```json
{ "schema": 2, "manifest": { ...原 latest.json 内容... }, "signature": "..." }
```

**不能直接复用 `latest.json` 这个键**：旧客户端会把信封当 manifest 解析，`schema: 2` 触发 `E_MANIFEST_SCHEMA_TOO_NEW`，等于一次 flag day。因此信封写到新键（例如 `latest.envelope.json`）：

- 新客户端优先读信封，单对象 + 内嵌签名，窗口消失；
- 信封缺失时回退旧的 `latest.json` + `.minisig`，兼容过渡期；
- 旧客户端继续读旧键，行为不变（窗口仍在，见方案 B）。

发布顺序：先写真名清单与旧签名（旧客户端可用），再写信封（新客户端从此无窗口）。信封可重复写，内容不变即幂等。

### 方案 B：客户端重试（1.0.x 必须有的兜底）

旧客户端无法理解信封，只能靠重试跨过窗口：验签失败后重新拉取 manifest 与签名，最多 2 次，退避 200ms / 600ms（窗口通常是两个 PUT 之间，远小于此）。

- 重试期间不得接受任何未通过验签的内容，也不缓存失败结果；
- 重试耗尽后仍返回签名错误（`E_SIGNATURE_INVALID` / `E_SIGNATURE_NO_TRUSTED_KEY`，`retryable` 保持 `false`），但在 `detail` 中写入“manifest 与签名连续 N 次不匹配，发布可能正在进行”；
- **不把耗尽后的结果改成“请稍后重试”的新错误码**：真实的清单篡改同样是“签名不匹配”，把它降级为瞬时错误会让攻击与发布窗口无法区分。提示放在 detail，错误码保持真实。

## 迁移

1. 发布客户端：信任集加入新公钥（作为下一个二进制内嵌的公钥）。
2. 发布侧开始为每份清单写“旧密钥主签名 + 新密钥附加签名”。
3. 观察一个版本窗口，确认客户端命中附加签名的比例接近 100%（`UpdateRecord` 的命中 key id）。
4. 把主签名切到新密钥，附加签名保留旧密钥一个窗口。
5. 从信任集移除旧公钥，按 major 承诺发布。

## 回滚

- 发布回滚：清单与签名一起回滚；附加签名是新增文件，删除它们不影响旧客户端。
- 客户端回滚：第 4 步之前，旧客户端只读主签名，回滚安全；第 4 步之后回滚需要把主签名切回旧密钥。

## 测试计划

契约测试放在 `crates/delta-update-engine/tests/security_contract.rs`，实现前先写、先失败。为了让“先测试”成立，测试针对一个新的 `SignatureSource` 抽象（`primary(manifest_key)` / `attachment(manifest_key, key_id)`），生产实现走 HTTP，测试实现读内存或本地目录；HTTP 细节不在契约里。

1. 旧格式（无 `signing_key_ids`、无附加文件）→ 与 1.0 行为完全一致。
2. 主签名无效、附加签名有效 → 接受，记录附加 key id。
3. 全部无效 → `E_SIGNATURE_NO_TRUSTED_KEY`。
4. 声明了 `signing_key_ids` 但对应文件缺失 → 回退到主签名结果，不因缺文件失败。
5. 未知 `key_id` 的附加文件被忽略。
6. 发布侧：同一次 `release` 写出的附加签名数与 `--signing-key` 数量一致，且每个都能被对应公钥验证。
7. **窗口内读取**：第一次取到 v1 manifest + v2 签名（不匹配），重试后取到匹配对 → 通过，不向用户报错。
8. **重试耗尽**：连续不匹配 → 返回签名错误，`detail` 含“发布可能正在进行”，`retryable` 仍为 `false`。

## 未决问题

- 附加签名的拉取策略：按需（主签名未命中时才拉），避免常态多一次请求。
- 信封键名（`latest.envelope.json` 或 `latest.json.v2`）在实现时定，协议只承诺“新键 + 单对象”。
- 发布侧 `--signing-key` 可重复传入属于实现细节，不写进本 RFC 的契约。
