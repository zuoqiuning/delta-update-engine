# 信任模型

本文回答一个问题：**一个能写用户可写目录的攻击者，能把更新流程利用到什么程度？**
结论：没有代码执行路径，最坏是拒绝服务或回滚到签名清单里存在过的旧版本字节。

## 1. 攻击者能力模型

### 能力之内

- 读/写 `journal_root`（默认 `%LOCALAPPDATA%\<app>\delta-update\journal`）
- 读/写 `staging_root`（默认 `%LOCALAPPDATA%\<app>\delta-update\staging`）
- 用户级安装时，读/写安装目录
- 读应用二进制的任意字节

### 能力之外

- 伪造 minisign 签名（私钥不在客户端）
- 修改已签名清单而不破坏验签
- 在用户不确认 UAC 的情况下获得管理员权限
- 修改嵌入式配置（`signing_public_keys` 编译进二进制）
- 修改正在运行进程的内存（这需要调试权限，已超出本威胁模型）

## 2. 信任锚

| 锚点 | 位置 | 为什么攻击者改不了 |
| --- | --- | --- |
| `signing_public_keys` | 应用二进制内的 `tauri.conf.json`（`run_applier_from_context` 从 `tauri::Context` 读取） | 改它等于改二进制；用户级攻击者可以改，但会导致验签失败/启动失败，不产生提权 |
| 签名清单 | 对象存储（HTTPS） | 需要私钥 |
| worker 可执行文件 | 安装目录下的同一二进制 | 提权 worker 由 `ShellExecuteW("runas")` 启动，路径由插件传入；管理员会话中用户已授权 |

验签失败一律失败关闭：`Engine::new` 与所有 worker 入口都要求 Journal 绑定到已验签清单，无签名恢复只存在于 `#[doc(hidden)]` 的 `Engine::new_legacy`，插件与 CLI 从不调用。

## 3. 逐文件信任边界

| 文件 | 攻击者能否篡改 | 篡改后果 | 约束来源 |
| --- | --- | --- | --- |
| `verified_manifest.json` + `.minisig` | 能写，但签名无法伪造 | 验签失败 → 拒绝执行 | `signing_public_keys` |
| `commit.meta.json` | 能（无独立签名） | 改 `version` 后 `Place` 目标不在该版本签名清单中 → 拒绝；改 `commit_id` 与 Journal 不匹配 → 拒绝 | 签名清单 + Journal 的 `commit_id`/哈希绑定 |
| `commit.journal` | 能（checksum 只能防损坏） | 每个 `Place` 的 `sha256`/`size` 必须等于签名清单中的目标；`Backup`/`Trash` 路径必须符合 `.old`/`.trash` 约定 → 不能写入任意新字节 | 签名清单 + `journal::verify` |
| `old_hashes.json` | 能 | 回滚只恢复到“签名清单中存在的旧版本字节”；篡改只能导致恢复失败或恢复到错误**版本**，不能注入新内容 | 旧版本签名清单（`installed_manifest.json`） |
| `commit.status` | 能 | 只能影响崩溃循环计数/回滚时机（DoS），不改变写入内容 | 无（仅状态） |
| `attempts` / `crash_loop_threshold` | 能 | 提前/推迟回滚（DoS） | 无（仅状态） |
| `prepared.json` | 能 | `prepare` 会重新校验每个暂存文件哈希；不匹配则重新下载 | 签名清单 |
| `staging/files/*` | 能 | 替换后哈希不匹配 → 重新下载或失败 | 签名清单 |
| `apply.json` | 能 | worker 校验绝对路径、`install_root == exe 目录`、staging/journal 不在安装根内；`begin_commit` 还会校验 version 与 journal 一致 | worker `validate_scope` |

关键点：**Journal 里的目标哈希来自签名清单**。攻击者可以决定“执行哪些签名清单里已存在的写入”，但不能决定“写什么字节”。因此不存在把任意内容落到任意路径的路径。

## 4. 明确“不是漏洞”的设计

- `commit.meta.json` 没有独立签名：它的作用是绑定，不是信任根。信任根始终是清单签名。
- `old_hashes.json` 可篡改：它只用于发现备份损坏，不是备份的信任来源。
- 用户级安装下攻击者可以替换安装目录中的 exe：这等价于“用户级攻击者已控制该用户的应用”，不在更新引擎的威胁模型内；per-machine 安装通过 UAC 提升到管理员后才写 `Program Files`。
- `allow_unsigned_journal_recovery` 不再是公开配置：它只是测试/迁移构造函数的一部分。

## 5. 残余风险

| 风险 | 影响 | 缓解 |
| --- | --- | --- |
| 攻击者可反复消耗带宽/磁盘（伪造 `prepared.json`、填充 staging） | DoS | `staging_size_limit`、`max_files`、`manifest_max_bytes`；用户可以删除 `journal/` 与 `staging/` 恢复 |
| 攻击者篡改 `old_hashes.json` | 回滚到错误的旧版本（仍是签名过的字节） | 回滚后仍有签名校验；发布者可发布新版本覆盖 |
| 攻击者阻止确认健康（删除 `commit.status`） | 崩溃循环计数异常，可能延迟或提前回滚 | 只影响可用性，不影响完整性 |
| 管理员权限下的攻击者 | 可替换任意文件 | 超出威胁模型；Windows 安装器/代码签名负责该层 |
| 对象存储/CDN 被攻破 | 可返回任意清单 | 清单验签失败即拒绝；私钥不在存储侧 |

## 6. 验证这个模型

- `crates/delta-update-engine/tests/security_contract.rs`：路径逃逸、大小/哈希不匹配、签名不匹配。
- `crates/delta-update-engine/tests/security_fault_injection.rs`：恶意/截断清单、错误哈希。
- `crates/delta-update-engine/tests/committer_contract.rs::production_engine_refuses_a_journal_without_commit_meta`：生产引擎拒绝无签名绑定 Journal。
- `crates/delta-update-engine/tests/committer_contract.rs`：`prepared.json`/`commit.meta.json` 未来 schema 被拒绝、installed manifest 部分缺失被识别、confirm 与 prepare 的锁竞争重试。
- `crates/delta-update-engine/tests/journal_fault_injection.rs`：Journal 篡改与部分执行后的重放。
- `crates/delta-update-cli/tests/serve.rs`：本地 `serve` 的穿越、设备前缀、ADS、符号链接与 Range 边界。
- `docs/design/security.md`：更细的字段级约束。

## 7. 部署者信任边界清单

- **签名私钥是唯一信任锚点**：清单签名 → journal 绑定 → 逐文件哈希校验，最终都回到它。
- **journal root 的写权限不是信任边界**：`prepared.json`、`commit.meta.json`、`old_hashes.json` 都可被同用户进程篡改，但它们必须与签名清单交叉校验；篡改只会失败关闭。
- **`verified_manifest` 的窗口**：`check` 写入、`begin_commit` 读取；被替换时哈希不匹配，提交被拒绝。
- **installed manifest 成对写入**：先写签名、再写清单；只剩一半时返回 `E_JOURNAL_CORRUPT`，不会被当成可信旧版本。
- **`doctor` 的 loopback 放行是诊断特权**：不代表运行时配置允许明文 HTTP。
- **`serve` 只用于本地开发**：loopback 不是信任边界，不防御同机恶意进程；不要用 `socat` / `netsh portproxy` 转发。
- **per-machine elevation**：worker 提权后只写 `install_root`，不能写父目录。
- **无签名恢复不可达**：`allow_unsigned_journal_recovery` 只存在于 `#[doc(hidden)] Engine::new_legacy`，插件与 CLI 从不调用。
