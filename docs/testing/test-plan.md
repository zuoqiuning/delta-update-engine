# Delta Update Engine 测试计划与故障注入验证矩阵

> 本文定义系统的可验证性标准。一个模块的“完成”不以“正常路径跑通”为标志，而以“在本文列出的故障注入场景下给出确定且可恢复的结果”为标志。差分算法的正确性可以借用成熟实现来保证；**崩溃原子性、回滚自愈、传输韧性这三类语义只能通过故障注入来建立信心**。本文是发布门禁的唯一依据。


## 1. 测试哲学与分层原则

### 1.1 三条测试原则

**原则一：引擎可脱离 Tauri 测试。** `delta-update-engine` 是纯 Rust crate，零 Tauri 依赖。所有核心逻辑（Manifest 比对、Journal 重放、Committer 替换、Rollback 决策）必须能在**两棵目录树**上完成测试，不启动 GUI、不依赖 WebView、不需要网络。

**原则二：故障注入优先于正常路径覆盖。** 正常路径的测试价值有限——它们证明代码能工作。真正决定生产可靠性的是：**在第 3 个文件替换时断电，恢复后目录处于什么状态**。本文的故障注入矩阵是核心资产，正常路径测试是它的补充。

**原则三：每个测试有确定的断言，而非“不崩溃即通过”。** “进程没有 panic”不是通过标准。通过标准是：**注入故障后，恢复逻辑将系统带到某个明确的可判定状态，且该状态满足预先声明的断言**。例如“journal 重放后，根目录的每个文件要么是旧版本、要么是新版本，不存在混合状态”。

### 1.2 测试分层

```
┌─────────────────────────────────────────────────────────────┐
│  L4  端到端验收测试（真实 Tauri 应用 + 真实 COS）            │
│      频率：每次发布前                                       │
│      覆盖：完整用户旅程                                     │
├─────────────────────────────────────────────────────────────┤
│  L3  插件生命周期测试（Tauri test harness，mock COS）        │
│      频率：每次 PR                                          │
│      覆盖：setup 钩子顺序、事件广播、命令语义                │
├─────────────────────────────────────────────────────────────┤
│  L2  故障注入集成测试（目录树 + 注入点）                     │
│      频率：每次 PR                                          │
│      覆盖：Journal 原子性、Committer 替换、Rollback 自愈     │
├─────────────────────────────────────────────────────────────┤
│  L1  模块单元测试（纯函数，无 I/O）                          │
│      频率：每次提交                                         │
│      覆盖：Manifest 解析、差异分类、路径校验、错误码映射     │
└─────────────────────────────────────────────────────────────┘
```

| 层   | 工具                                | 执行时间目标 | 是否阻塞合并 |
| ---- | ----------------------------------- | ------------ | ------------ |
| L1   | `cargo test`（单元测试）            | < 30 秒      | 是           |
| L2   | `cargo test`（集成测试 + 临时目录） | < 3 分钟     | 是           |
| L3   | Tauri test harness + mock HTTP      | < 5 分钟     | 是           |
| L4   | 真实应用 + 测试 COS 桶              | < 30 分钟    | 是（发布前） |


## 2. L1：模块单元测试

### 2.1 Manifest 模块

| 用例                   | 输入                                   | 期望                               |
| ---------------------- | -------------------------------------- | ---------------------------------- |
| 清单解析——合法         | 完整 `latest.json`                     | 解析成功，字段值正确               |
| 清单解析——缺少必填字段 | 缺 `sha256`                            | `E_MANIFEST_PARSE`                 |
| 清单解析——schema 过高  | `schema: 2`，客户端支持 1              | `E_MANIFEST_SCHEMA_TOO_NEW`        |
| 版本单调性——远程更高   | 本地 2.0.0，远程 2.1.0                 | 允许                               |
| 版本单调性——远程相同   | 本地 2.1.0，远程 2.1.0                 | `available: false`，不报错         |
| 版本单调性——远程更低   | 本地 2.1.0，远程 2.0.0，`rollback` 缺省 | `E_VERSION_ROLLBACK`               |
| 降级——显式 rollback    | 本地 2.1.0，远程 2.0.0，`rollback: true` | 接受，清除该版本的崩溃跳过标记     |
| 降级——本地不高于目标   | 本地 2.0.0，远程 2.0.0，`rollback: true` | `available: false`，不重复降级     |
| 版本号非法             | 远程 `version: "latest"`               | `E_VERSION_INVALID`                |
| 差异分类——Unchanged    | 哈希一致                               | `Unchanged`                        |
| 差异分类——Modified     | 哈希不同，本地存在                     | `Modified`                         |
| 差异分类——Added        | 本地不存在，远程存在                   | `Added`，且标记 `force_full`       |
| 差异分类——Removed      | 本地存在，远程不存在，在历史官方集合中 | `Removed`                          |
| 差异分类——Untracked    | 本地存在，远程不存在，不在历史集合中   | `Untracked`，不处理                |
| 历史官方集合为空时     | 首次迁移                               | 所有远程不存在文件归为 `Untracked` |
| 哈希缓存——命中         | 文件 size/mtime 与缓存一致             | 复用缓存哈希，不重新读取文件内容   |
| 哈希缓存——失效         | 文件 size/mtime 变化，或缓存损坏/换根  | 重新哈希该文件并更新缓存，不阻塞   |

### 2.2 Crypto 模块

| 用例                     | 输入                           | 期望                                           |
| ------------------------ | ------------------------------ | ---------------------------------------------- |
| 验签——合法               | 有效清单 + 有效签名 + 受信公钥 | 通过                                           |
| 验签——签名覆盖原始字节   | 对原始字节签名                 | 通过                                           |
| 验签——签名对解析后 JSON  | 对规范化 JSON 签名             | **失败**（证明实现没有错误地对解析后数据验签） |
| 验签——多公钥，第二把匹配 | 第一把不匹配，第二把匹配       | 通过                                           |
| 验签——无匹配公钥         | 所有公钥不匹配                 | `E_SIGNATURE_NO_TRUSTED_KEY`                   |
| 验签——清单被篡改一个字节 | 修改 `version` 一个字符        | 失败                                           |
| 目标哈希校验——通过       | 目标文件哈希一致               | 通过                                           |
| 目标哈希校验——不匹配     | 目标文件被篡改                 | `E_PATCH_TARGET_HASH_MISMATCH`                 |

### 2.3 Path Resolver 模块

| 用例         | 输入相对路径                     | 期望               |
| ------------ | -------------------------------- | ------------------ |
| 合法路径     | `bin/core.dll`                   | 通过               |
| 包含 `..`    | `../evil.dll`                    | `E_PATH_ESCAPE`    |
| 包含 `.`     | `./app.exe`                      | `E_PATH_INVALID`   |
| 绝对路径     | `/etc/passwd`                    | `E_PATH_INVALID`   |
| Windows 盘符 | `C:\evil.dll`                    | `E_PATH_INVALID`   |
| UNC 前缀     | `\\server\share\x.dll`           | `E_PATH_INVALID`   |
| 空段         | `bin//core.dll`                  | `E_PATH_INVALID`   |
| 结尾斜杠     | `bin/`                           | `E_PATH_INVALID`   |
| 规范化后逃逸 | `bin/../../evil.dll`             | `E_PATH_ESCAPE`    |
| 大小写规范化 | `Bin/Core.DLL` vs `bin/core.dll` | 比对时视为同一文件 |
| 保留设备名   | `CON`、`NUL.txt`、`COM1`         | `E_PATH_INVALID`   |
| 段尾空格/句点 | `bin/core.dll `、`bin/foo.`     | `E_PATH_INVALID`   |
| 大小写冲突   | 清单同时含 `App.exe` 与 `app.exe` | `E_PATH_COLLISION` |
| 路径超长     | 1025 字节或 65 段                | `E_PATH_INVALID`   |

### 2.4 Error Taxonomy 模块

| 用例             | 输入                       | 期望                     |
| ---------------- | -------------------------- | ------------------------ |
| 错误码稳定       | 构造已知错误               | 码值与协议文档 §7.3 一致 |
| `retryable` 语义 | 构造 `E_NETWORK_TIMEOUT`   | `retryable == true`      |
| `retryable` 语义 | 构造 `E_SIGNATURE_INVALID` | `retryable == false`     |
| `detail` 脱敏    | 包含用户路径的错误         | `detail` 中不含用户名    |


## 3. L2：目录树集成测试与故障注入

### 3.1 测试基础设施

**测试环境构造**：每个测试用例使用临时目录构造两棵树：

```
/tmp/test-xxxx/
├── root/              # 模拟安装目录 R
│   ├── App.exe        # 旧版本
│   ├── bin/core.dll   # 旧版本
│   └── resources/...  # 旧版本
├── staging/           # 模拟暂存区
│   └── (prepare 产出)
├── remote/            # 模拟 COS（本地文件系统）
│   ├── latest.json
│   ├── latest.json.minisig
│   └── artifacts/...
└── appdata/           # 模拟应用数据目录
    ├── journal/
    └── .attempts
```

**故障注入方式**：

| 注入类型     | 实现方式                                                     |
| ------------ | ------------------------------------------------------------ |
| **进程崩溃** | 在指定操作后调用 `std::process::abort()`，测试以子进程运行，父进程检查目录状态 |
| **断电模拟** | 使用 `fail` crate 或自定义 `FaultyFs` wrapper，在第 N 次 `write` / `rename` 时返回 I/O 错误 |
| **文件锁定** | 在测试中打开目标文件并持有句柄，模拟运行中的 exe/dll         |
| **磁盘满**   | 使用 `FaultyFs` 在写入时返回 `ENOSPC`                        |
| **权限拒绝** | 使用 `FaultyFs` 在 `rename` / `write` 时返回 `EACCES`        |
| **网络中断** | mock `Fetcher` 在第 N 字节后返回连接重置                     |
| **URL 过期** | mock `Fetcher` 在第一次请求时返回 403，验证刷新逻辑          |

**目录状态断言工具**：

```rust
/// 断言根目录处于“全旧”或“全新”的一致状态，不存在混合
fn assert_consistent(root: &Path, old_manifest: &Manifest, new_manifest: &Manifest);

/// 断言根目录的每个文件哈希与指定清单一致
fn assert_matches_manifest(root: &Path, manifest: &Manifest);

/// 断言根目录中不存在 .old / .trash 残留
fn assert_clean(root: &Path);
```

### 3.2 Journal 故障注入矩阵（核心）

**这是整份文档最重要的部分。** Journal 的原子性是“重启生效”和“自愈回滚”的物理基础。

| #    | 注入点                                   | 注入方式                                 | 期望状态                         | 恢复后断言                                                   |
| ---- | ---------------------------------------- | ---------------------------------------- | -------------------------------- | ------------------------------------------------------------ |
| J-01 | Journal flush **之前**                   | 在 `begin()` 内 flush 前 abort           | 根目录未被触碰                   | 恢复：无 journal 存在，根目录全旧                            |
| J-02 | Journal flush **之后**、第一个 op 之前   | 在 flush 后、执行 op 前 abort            | journal 已存在，根目录未变       | 恢复：重放全部 ops，根目录**全新**（提交语义是向前完成）     |
| J-03 | 第 1 个 `Backup` 完成、第 2 个之前       | 在第 1 个 op 后 abort                    | 1 个文件已备份                   | 恢复：重放跳过第 1 个，完成剩余，根目录**全新**              |
| J-04 | 第 3 个 op 执行中（`rename` 返回前）     | `FaultyFs` 在第 3 个 rename 时 abort     | 前 2 个已完成，第 3 个状态未知   | 恢复：通过幂等判定第 3 个是否落地，完成剩余                  |
| J-05 | 所有 op 完成、写 `commit.status` 之前     | 在最后一个 op 后 abort                   | 全部文件已替换，journal 仍在     | 恢复：重放全部（幂等跳过），写入 completed；journal 保留至 confirm |
| J-06 | 写 `commit.status` 过程中                | `FaultyFs` 在写状态文件时 abort          | 文件全替换，状态文件不存在（只有临时文件） | 恢复：视为未完成 → 重放（幂等）→ 重新原子写入 completed |
| J-07 | `commit.status=completed` 之后、confirm 清理之前 | 标记完成后 abort | 全部替换，标记完整，journal 仍在 | 恢复：识别 Completed，等待健康确认；崩溃循环则用保留的 journal 回滚 |
| J-08 | confirm 清理删除 journal 时中断          | `FaultyFs` 在 unlink 时 abort            | unlink 原子；journal 要么完整存在要么不存在 | 恢复：status=completed 时等待确认；journal 仍完整则重放；checksum 失败则 `E_JOURNAL_CORRUPT`，不猜测目录状态 |
| J-09 | Journal 文件被外部篡改                   | 测试中修改 journal 字节                  | —                                | 恢复：checksum 失败 → `E_JOURNAL_CORRUPT`，**不尝试重放**    |
| J-10 | Journal 重放过程中再次崩溃               | 在重放第 2 个 op 时 abort                | 第 1 个 op 已完成                | 恢复：再次重放，幂等跳过第 1 个，完成剩余                    |
| J-11 | 同一份 journal 连续重放 3 次             | 不删除 journal，连续调用 `recover()`     | —                                | 三次结果一致，**幂等性成立**                                 |
| J-12 | 根目录被外部修改后重放                   | 在备份文件后手动修改根目录文件           | —                                | 目标哈希不匹配 → 从暂存源重新落地并覆盖外部修改；来源不可用则 `E_JOURNAL_REPLAY_FAILED` |

**J-08 的特殊说明**：删除 journal 时中断导致 journal 部分存在——这与“journal 写入时中断”不同。删除是 `unlink` 系统调用，在 POSIX/Windows 上要么成功要么失败，**不会产生“部分删除”的文件**。因此不引入“根据目录状态猜测是否完成”的分支：如果确实读到 checksum 失败的 journal，一律返回 `E_JOURNAL_CORRUPT`，**不尝试重放，也不猜测目录状态**。测试应断言这一确定行为。

### 3.3 Committer 故障注入矩阵

| #    | 注入点                       | 期望状态                                                    | 断言                             |
| ---- | ---------------------------- | ----------------------------------------------------------- | -------------------------------- |
| C-01 | 替换运行中 exe（文件被锁定） | rename-then-replace 成功，旧 exe 在内存中继续运行           | 新 exe 在磁盘上就位，`R/.old/<commit_id>/` 存在 |
| C-02 | 上次提交残留的 `.old` 目录   | 新提交使用新的 `commit_id` 子目录，互不冲突                 | 两个 commit 目录可独立清理       |
| C-03 | 替换到一半磁盘满             | 重放返回 `E_JOURNAL_REPLAY_FAILED` 并停止；下次启动幂等重放后续完 | 恢复后根目录与新版本一致         |
| C-04 | 替换到一半权限拒绝           | 重放返回 `E_JOURNAL_REPLAY_FAILED`，不 unwind；下次启动重试或回滚 | 恢复后根目录一致                 |
| C-05 | Trash 操作时目标目录不存在   | 自动创建 `.trash/<commit_id>/`                              | 旧文件被移入，不丢失             |
| C-06 | 替换后目标文件哈希不匹配     | 删除目标文件；下次启动无法续完时回滚备份                    | 根目录回到替换前状态             |
| C-07 | 两个进程同时 Commit          | 第二个拿不到 `update.lock`，返回 `E_UPDATE_LOCKED`          | 只有一个 Commit 执行             |

### 3.4 Staging 模块测试

| #    | 用例                         | 期望                                          |
| ---- | ---------------------------- | --------------------------------------------- |
| S-01 | 下载中断后续传               | 从断点继续，最终文件完整                      |
| S-02 | URL 过期后刷新续传           | 换新 URL，从当前偏移继续，不重新开始          |
| S-03 | 补丁应用后哈希不匹配         | 回退到 `full_key` 重新下载，不进入暂存区      |
| S-04 | 全量回退也失败               | prepare 整体失败，返回 `E_PATCH_APPLY_FAILED` |
| S-05 | 暂存区磁盘空间不足（下载前） | `E_DISK_INSUFFICIENT`，不开始下载             |
| S-06 | 暂存区超过上限               | `E_DISK_STAGING_LIMIT`，拒绝 prepare          |
| S-07 | PREPARED 缓存复用            | 第二次启动不重新下载，直接识别                |
| S-08 | 暂存区文件被外部篡改         | 全树校验发现哈希不匹配，prepare 失败          |


## 4. L2：Rollback 崩溃循环测试

### 4.1 测试方法

Rollback 的测试需要一个**可控的“启动失败”模拟器**：

```rust
/// 在测试应用启动早期调用。
/// 行为由环境变量控制：
///   TEST_CRASH_ON_STARTUP=1  → 模拟启动崩溃（在 confirm_healthy 前 abort）
///   TEST_CRASH_ON_STARTUP=2  → 模拟连续两次启动崩溃
fn maybe_crash_for_test() {
    if env::var("TEST_CRASH_ON_STARTUP").map(|v| v == "1").unwrap_or(false) {
        std::process::abort();
    }
}
```

测试以子进程运行应用，父进程控制环境变量并检查目录状态。

### 4.2 用例矩阵

| #    | 场景                         | 步骤                                  | 期望                                                  |
| ---- | ---------------------------- | ------------------------------------- | ----------------------------------------------------- |
| R-01 | 正常启动                     | 无注入                                | `confirm_healthy` 后计数归零                          |
| R-02 | 单次启动崩溃                 | `TEST_CRASH_ON_STARTUP=1` 一次        | 计数 +1，不触发回滚                                   |
| R-03 | 连续两次启动崩溃             | 两次注入                              | 第三次启动触发回滚                                    |
| R-04 | 回滚后可用                   | R-03 后启动                           | 以旧版本成功启动，计数归零                            |
| R-05 | 回滚后跳过同一远程版本       | R-04 后触发 check                     | 不自动 prepare 同一版本                               |
| R-06 | 回滚后远程有新版本           | R-04 后远程发布 2.2.0                 | 正常 prepare 2.2.0                                    |
| R-07 | 回滚自身失败                 | 注入回滚时磁盘满                      | `E_ROLLBACK_FAILED`，记录诊断日志，不丢旧版本文件     |
| R-08 | 主程序完全无法启动           | 崩溃发生在 `recover_and_cleanup` 之前 | 回滚逻辑不可达 → 这是系统边界，**必须记录为已知限制** |
| R-09 | `confirm_healthy` 后立即崩溃 | 前端 UI 就绪后调用确认，然后崩溃      | 计数已归零 → **不触发回滚**（应用曾被确认健康）       |
| R-10 | 阈值配置为 1                 | `crash_loop_threshold: 1`             | 单次崩溃即回滚                                        |
| R-11 | per-machine 启动恢复决策     | 安装根不可写，分别构造“已提交待确认”和“待重放 Journal” | 前者不提权；后者启动提权 worker 完成恢复              |
| R-12 | 提权恢复只计一次启动         | 提权 worker 完成后父进程检查 state    | `attempts` 只 +1，不因 worker/父进程两次检查而翻倍    |

**R-08 的关键判断**：如果新版本二进制在 `setup` 之前就崩溃（例如动态链接失败），`recover_and_cleanup()` 永远没有机会执行。这是**本系统的已知边界**，必须记录在文档中。缓解方式：Tauri 的 bootstrap 层应尽量早地加载插件，但无法早于进程加载本身。这通常由安装器或外部 launcher 兜底。

**R-09 的语义**：`confirm_healthy()` 是“应用已进入可用状态”的显式声明。一旦声明，崩溃不再被归因为“更新失败”——它可能是普通业务 bug。这个语义边界必须在测试中明确验证。


## 5. L2：Fetcher 网络韧性测试

| #    | 注入                          | 期望                                                  |
| ---- | ----------------------------- | ----------------------------------------------------- |
| F-01 | 下载到 50% 时连接重置         | 从断点续传，最终文件完整                              |
| F-02 | 下载到 50% 时 URL 过期（403） | 刷新 URL，从断点续传                                  |
| F-03 | URL 刷新 3 次后仍过期         | 返回 `E_NETWORK_PRESIGN_EXPIRED`，不无限循环          |
| F-04 | 服务端不支持 Range            | 回退全量重下，记录 `E_NETWORK_RANGE_UNSUPPORTED` 告警 |
| F-05 | 下载后补丁载荷被篡改          | 容器头校验通过但 apply 后目标哈希不匹配 → 丢弃并回退全量 |
| F-06 | 重试 3 次后仍失败             | 返回错误，不无限重试                                  |
| F-07 | 5xx 错误                      | 可重试，指数退避                                      |
| F-08 | 4xx 错误（非 403）            | 不重试，直接返回错误                                  |
| F-09 | 并发下载 10 个文件            | 并发数被限制为配置值（默认 4）                        |
| F-10 | 慢速网络                      | 下载超时被正确触发，可续传                            |


## 6. L2：安全测试用例

| #      | 用例                                   | 期望                                            |
| ------ | -------------------------------------- | ----------------------------------------------- |
| SEC-01 | 篡改清单 `version` 字段                | 验签失败 `E_SIGNATURE_INVALID`                  |
| SEC-02 | 篡改清单 `delta_key` 指向其他对象      | 验签失败                                        |
| SEC-03 | 篡改清单 `sha256` 字段                 | 验签失败                                        |
| SEC-04 | 用旧版本清单重放                       | 签名可能有效，但 `E_VERSION_ROLLBACK`           |
| SEC-05 | 路径包含 `..`                          | `E_PATH_ESCAPE`，不写入任何文件                 |
| SEC-06 | 补丁对象被替换为恶意内容               | 目标哈希校验失败 → 回退全量                     |
| SEC-07 | 全量对象被替换                         | 目标哈希校验失败 → prepare 失败                 |
| SEC-08 | 签名对象缺失                           | `E_SIGNATURE_SIG_MISSING`                       |
| SEC-09 | 无匹配受信公钥                         | `E_SIGNATURE_NO_TRUSTED_KEY`                    |
| SEC-10 | 清单携带 `key_rotation` 字段           | 验签通过但字段被忽略，信任集不变，输出告警      |
| SEC-11 | 新密钥签名，但客户端二进制未内嵌该密钥 | `E_SIGNATURE_NO_TRUSTED_KEY` → 预期行为，必须先发版 |
| SEC-12 | WebView 调用不存在的 `commit` 命令     | 命令不存在，运行时拒绝                          |
| SEC-13 | WebView 调用 `prepare` 传路径参数      | 命令不接收路径参数，参数被忽略                  |
| SEC-14 | 伪造 Journal 指向根目录外              | 重放时路径逃逸检查拒绝                          |
| SEC-15 | Journal checksum 损坏                  | `E_JOURNAL_CORRUPT`，不重放                     |
| SEC-16 | 客户端二进制已内嵌新密钥后新密钥签名   | 验签通过                                        |
| SEC-17 | 篡改暂存区文件并重算 Journal checksum  | 重放时来源哈希不匹配 → `E_JOURNAL_REPLAY_FAILED` |
| SEC-18 | 清单 9 MiB / 20 001 个条目             | `E_MANIFEST_TOO_LARGE`，解析前拒绝              |
| SEC-19 | 补丁解压输出超过清单 `size`            | `E_PATCH_DECOMPRESSION_LIMIT`，立即中止         |


## 7. L3：插件生命周期测试

使用 Tauri 的 test harness（或最小化 Tauri 应用）验证生命周期编排。

| #    | 用例                                     | 期望                                |
| ---- | ---------------------------------------- | ----------------------------------- |
| P-01 | `setup` 最早期调用 `recover_and_cleanup` | 在主窗口创建前执行                  |
| P-02 | `auto_check: true` 时后台 prepare        | 主线程不被阻塞，UI 正常显示         |
| P-03 | prepare 完成发出 `delta-update://state`  | 前端收到 `state = Prepared` 事件    |
| P-04 | `auto_check: false` 时不自动 prepare     | 只暴露命令，不后台执行              |
| P-05 | 六个命令的默认权限                       | `delta-update:default` 允许六个命令 |
| P-06 | 未授予权限时命令调用被拒绝               | Tauri 运行时拒绝                    |
| P-07 | 状态机非法迁移                           | `E_INVALID_TRANSITION`              |
| P-09 | 配置缺少必填字段                         | `E_CONFIG_INVALID`                  |
| P-10 | 配置 `endpoint` 非 HTTPS                 | `E_ENDPOINT_INSECURE`               |
| P-11 | 崩溃循环检测在 setup 中执行              | 窗口创建前完成回滚判断              |
| P-12 | `cancel_prepare` 中止下载                | 状态迁移到 `Cancelled`，临时文件保留 |
| P-13 | 无 `Prepared` 时 `restart_to_apply`      | `E_NOT_PREPARED`，应用不退出         |
| P-14 | `auto_apply: true` 时正常退出            | spawn 替换进程，Journal 已 flush     |
| P-15 | 两个实例同时触发 prepare/commit          | 后到者 `E_UPDATE_LOCKED`             |
| P-16 | 启动期 UAC 取消                          | 状态为 `Failed`，Journal/回滚素材保留 |
| P-17 | `check_interval_secs` 定时检查           | 到点后空闲状态执行 check+prepare，失败可重试 |
| P-18 | 启动期自动回滚                           | 发出 `RolledBack` 状态，UI 可提示回滚版本 |
| P-19 | 忘记调用 `run_applier_from_args()`       | setup 返回 `E_CONFIG_INVALID`，不静默开第二个实例 |
| P-20 | `check` 后立即 `prepare`                 | 复用验签结果，清单与签名各只请求一次 |
| P-21 | prepare 持锁期间自动健康确认             | `confirm_healthy` 仍重置启动计数，不被 `E_UPDATE_LOCKED` 阻塞 |

**平台范围**：Windows 10+ x64 为完整验收基线（per-user / per-machine、UAC、真实替换）；Linux AppImage/tar 与 macOS 未签名包按 [API 平台矩阵](../reference/api.md) 提供有限支持，当前 CI 门禁只在 Windows 执行。非桌面平台仍由注册期检查返回 `E_UNSUPPORTED_PLATFORM`，避免误用时静默失败。


## 8. L4：端到端验收测试

### 8.1 端到端测试环境

```
┌──────────────┐     ┌──────────────┐     ┌──────────────┐
│  测试应用     │────►│  测试 COS 桶  │◄────│  测试签名服务 │
│  (真实 Tauri) │     │  (独立桶)     │     │  (临时密钥)  │
└──────────────┘     └──────────────┘     └──────────────┘
        │
        ▼
┌──────────────┐
│  测试虚拟机   │  ← 可快照恢复，用于模拟断电/重装
└──────────────┘
```

### 8.2 验收场景

| #      | 场景                                           | 验收标准                           |
| ------ | ---------------------------------------------- | ---------------------------------- |
| E2E-01 | 首次安装 → 检查更新 → 后台准备 → 重启生效      | 用户不感知下载，重启后版本为新版本 |
| E2E-02 | 无更新时不下载任何补丁                         | 网络请求只包含清单和签名           |
| E2E-03 | 断网 → 恢复网络 → 续传                         | 更新成功，补丁不重新下载           |
| E2E-04 | 更新后新版本崩溃 → 自动回滚                    | 连续两次未确认，第三次启动回到旧版本可用 |
| E2E-05 | 签名错误 → 拒绝更新                            | 用户看到明确错误，旧版本正常可用   |
| E2E-06 | 安装在 `Program Files` → 提权更新              | UAC 提示 → 更新成功                |
| E2E-07 | 安装在用户目录 → 无提权更新                    | 无 UAC 提示，更新成功              |
| E2E-08 | 发布端执行 `delta-update release` → 客户端更新 | 全流程无手工干预                   |
| E2E-09 | 回滚发布（2.1.0 → 2.0.0）                      | 特殊发布流程，客户端接受降级       |
| E2E-10 | 首次从“无清单旧版本”迁移                       | 只做新增/修改，不删除用户文件      |
| E2E-11 | `Program Files` 安装中断提交后启动             | 启动时 UAC → 恢复完成后主窗口出现；取消则状态 `Failed` 且 Journal 保留 |

**自动化覆盖状态**：`cargo run -p delta-update-e2e` 覆盖 E2E-01/02/04/07/08/09，并以 5xx 注入验证 E2E-03 的重试路径；E2E-05 由 L2 验签契约测试覆盖；E2E-06 与 E2E-11 的真实 UAC 交互需要提权环境手动执行（`uac_cancelled` 覆盖取消授权后撤销提交点、保留 PREPARED 并重试的语义）；E2E-10 不在此二进制内。场景清单与断言见 [e2e/README.md](../../e2e/README.md)。


## 9. 发布门禁：最小测试集

以下测试**必须全绿**才能发布。任何一项失败阻塞发布。

### 9.1 每次 PR 必须通过

- [ ] 全部 L1 单元测试
- [ ] L2 Journal 故障注入 J-01 至 J-12
- [ ] L2 Committer 故障注入 C-01 至 C-07
- [ ] L2 Staging / Fetcher S-01 至 S-08、F-01 至 F-10
- [ ] L2 安全测试 SEC-01 至 SEC-19
- [ ] L3 插件生命周期 P-01 至 P-21

**当前自动化状态**：J-01..J-12 由 `tests/journal_fault_injection.rs` 以磁盘状态注入覆盖（模拟崩溃后的确定状态，不依赖系统调用内中断）；C-01..C-07 与 R-09/R-10 由 `tests/committer_fault_injection.rs` 覆盖（C-01 用真实运行进程验证镜像重命名备份，C-03/C-04/C-06 用 `FaultyFs` 注入重命名失败与落盘损坏）；R-11/R-12 由 `tests/committer_contract.rs` 覆盖（是否需要写安装根、提权恢复不重复计数）；F-01/F-04/F-05/F-06/F-08/F-09/F-10 由 `tests/fetch_fault_injection.rs` 与 `tests/staging_contract.rs` 覆盖，F-02/F-03 由 `tests/presign_refresh.rs` 覆盖（F-07 由 `check_contract` 与 e2e 覆盖）；S-04/S-05/S-07/S-08 在 `tests/staging_contract.rs`（S-05 用超大声明体积触发真实可用空间不足，断言下载前失败）。SEC-01/02/03/08/14/16/17/18/19 由 `tests/security_fault_injection.rs` 覆盖，SEC-04/05/06/07/09/15 由 check/staging/journal 测试覆盖；SEC-10 的行为断言已覆盖（告警日志未断言），SEC-12/13 由插件 `tests/lifecycle_contract.rs` 的命令面测试覆盖。P-01/P-02/P-03/P-04/P-06/P-11/P-14 由插件 `tests/lifecycle_contract.rs` 通过 Tauri mock runtime 覆盖（启动恢复、崩溃循环、后台 prepare 与 `Prepared` 事件、退出提交、无 ACL 拒绝），同文件覆盖 `auto_confirm` 的窗口就绪延迟确认与 `confirm_healthy` 命令的备份清理契约；P-05/P-07/P-09/P-10/P-12/P-13 已覆盖，P-15 的 Commit 锁由 C-07 覆盖，P-16 的真实 UAC 交互需在提权环境手动执行；P-17 的配置校验由 `config_contract` 覆盖，定时循环本身不等待 60 秒，需在真实应用中观察；P-18 由启动回滚测试断言 `RolledBack`，P-19 由 `worker_mode_requested` 单元测试覆盖，P-20 由 `prepare_reuses_a_recent_check` 断言清单只请求一次，P-21 由 `confirm_healthy_does_not_wait_for_a_prepare_lock` 覆盖。`check` 的 notes/mandatory 与 64 KiB 上限由 `tests/check_contract.rs` 覆盖，下载进度累计值与单调性由 `tests/staging_contract.rs` 断言，`ErrorPayload.action` 与 `proxy` 配置由 `error_contract` / `config_contract` 覆盖，前端控制器由 `packages/plugin-api/tests/controller.test.ts` 覆盖，React hook 由 `pnpm typecheck` 校验。平台范围仅 Windows 桌面，移动端不作为门禁项。桌面环境可自动化的矩阵项已全部覆盖。

### 9.2 每次发布前必须通过

- [ ] 全部 L1 + L2 + L3
- [ ] L4 端到端 E2E-01 至 E2E-11
- [ ] 真实版本发布 dry-run（CLI 生成 + 上传 + 客户端验证）
- [ ] 故障注入全矩阵连续 3 次运行无 flaky

### 9.3 性能基线

| 指标                          | 基线                                    | 测试方法       |
| ----------------------------- | --------------------------------------- | -------------- |
| 单文件补丁应用内存峰值        | ≤ 2.5 × 文件大小（100 MB 文件 ≤ 250 MB） | 大文件差分测试 |
| Journal 重放 100 个操作耗时   | < 1 秒                                  | 批量操作测试   |
| 单文件补丁应用（10 MB）       | < 2 秒                                  | 基准测试       |
| prepare 校验（1000 文件）     | < 5 秒                                  | 哈希计算基准   |

`e2e` 的 `delta_update` 场景会打印 prepare 耗时，并断言补丁/全量下载比低于 50%，作为每次 CI 的轻量回归基线。

`cargo bench -p delta-update-engine` 提供可复现的本地基线：

| Bench                          | 覆盖指标                                   |
| ------------------------------ | ------------------------------------------ |
| `journal/replay_100_ops`       | 100 个 Journal 操作（50 个文件替换）的重放 |
| `prepare/verify_plan_1000_files` | 1000 个暂存文件的尺寸与 SHA-256 复核     |
| `patch_header/parse`           | 补丁容器头解析                             |
| `hash/sha256_1mib`             | 1 MiB 流式哈希吞吐                         |

基准只用于观察回归，不设硬性阈值：不同机器、磁盘与杀毒软件差异会显著影响 I/O 密集项。


## 10. 测试基础设施设计

### 10.1 `FaultyFs` 抽象

Committer 和 Journal 的文件操作通过一个 trait 抽象，测试时可注入故障：

```rust
pub trait FileSystem {
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn write_atomic(&self, path: &Path, data: &[u8]) -> io::Result<()>;
    fn remove(&self, path: &Path) -> io::Result<()>;
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    fn sync_dir(&self, path: &Path) -> io::Result<()>;
}

pub struct FaultyFs {
    inner: RealFs,
    fail: Mutex<Option<(PathBuf, io::ErrorKind)>>,
    corrupt: Mutex<Option<PathBuf>>,
}
```

故障按 rename 的**目标路径**一次性命中，不依赖操作序号；`corrupt` 在重命名成功后写入损坏内容，用于验证落盘后的哈希校验。

**关键**：生产代码使用 `RealFs`，测试使用 `FaultyFs`。**故障注入不修改生产代码路径**——生产代码和生产测试运行的是同一份逻辑，只是文件系统实现不同。

### 10.2 子进程崩溃测试

对于 `abort()` 注入，测试以子进程方式运行被测代码：

```rust
#[test]
fn journal_crash_after_flush_before_first_op() {
    let dir = setup_test_tree();
    let child = Command::new(env!("CARGO_BIN_EXE_crash_harness"))
        .env("CRASH_AT", "after_flush")
        .env("TEST_DIR", dir.path())
        .spawn().unwrap();
    let status = child.wait().unwrap();
    assert!(!status.success());  // 确认崩溃发生

    // 父进程检查状态并触发恢复
    let outcome = CommitJournal::recover(&dir.path().join("root")).unwrap();
    assert_consistent(&dir.path().join("root"), &OLD, &NEW);
}
```

`crash_harness` 是一个极小的测试二进制，在指定注入点调用 `abort()`。

### 10.3 CI 流水线

```yaml
# 简化的 CI 结构
stages:
  - lint        # cargo fmt + clippy
  - unit        # L1
  - integration # L2（含故障注入）
  - plugin      # L3
  - e2e         # L4（仅发布分支）

integration:
  script:
    - cargo test --test journal_fault_injection -- --test-threads=1
    - cargo test --test committer_fault_injection -- --test-threads=1
    - cargo test --test security
  retry: 0  # 故障注入测试不允许 flaky 重试
```

**关键规则**：故障注入测试**不启用自动重试**。如果它们 flaky，说明系统行为不确定，这是比测试失败更严重的问题。


## 11. 已知边界与不在测试范围内的场景

诚实记录系统无法覆盖的场景，比假装覆盖更专业：

| 边界                               | 原因                       | 缓解                                     |
| ---------------------------------- | -------------------------- | ---------------------------------------- |
| 新版本在插件 `setup` 之前崩溃      | 恢复逻辑不可达             | 依赖安装器/外部 launcher 兜底；文档记录  |
| WebView 内存破坏导致 Rust Core RCE | Tauri 威胁模型不防御       | CSP、Isolation Pattern、及时更新 WebView |
| 操作系统级文件系统损坏             | 系统信任 OS 的文件系统语义 | 依赖 OS 恢复机制                         |
| 签名私钥泄露后的持续滥用           | 密钥安全依赖离线保管       | 多密钥轮换、泄露应急预案                 |
| 用户手动篡改安装目录文件           | 用户有权控制自己的机器     | 哈希校验失败时回退全量或报错             |
| 构建环境在签名前被攻陷             | 系统外问题                 | CI 安全策略、审批门                      |
| 替换进程被 kill                    | 进程无法保证自身完成       | 下次启动重放 Journal，幂等续完           |
| 跨卷安装的 `move` 非原子           | 文件系统语义               | 统一使用“目标目录临时文件 + 原子 rename” |
| macOS 签名 `.app` 被修改后签名失效 | 代码签名语义               | 该形态不支持文件级替换，走 PKG/全量      |
| 本地攻击者删除 Journal / 暂存区    | 用户对自己目录有删除权     | 可用性受影响，但不会写入未校验字节       |


## 12. 测试文档总结

这份测试计划的核心判断是：**系统的可靠性不取决于正常路径的覆盖率，而取决于故障注入矩阵的完备性**。J-01 至 J-12、C-01 至 C-07、R-01 至 R-10 是这份文档的骨架——它们定义了“崩溃原子性”和“自愈回滚”这两个最核心承诺的**可验证边界**。

建议的实现顺序：**先写 `FaultyFs` 和测试基础设施，再写 Journal 和 Committer 的生产代码**。故障注入测试不是事后补充，而是设计的一部分——如果 Journal 的接口不是为可测试性设计的（例如 I/O 没有抽象为 trait），故障注入将无法进行，原子性承诺也就无法被验证。