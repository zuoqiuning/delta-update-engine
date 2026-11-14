# Delta Update Engine 权限与提权设计说明

> 本文定义 Delta Update Engine（`delta-update-engine`）在桌面平台下的权限模型与提权路径，以 Windows UAC 为主线。核心设计约束是一条：**用户级主进程不主动请求提权**；提权仅发生在受限的、操作语义封闭的 worker 中，且 worker 的执行范围被 Journal 和受信配置严格约束。本文回答三个问题：为什么不能整体提权、worker 如何被安全地启动、提权失败时系统如何降级。


## 1. 为什么不能整体提权

### 1.1 Tauri 应用的权限现实

Tauri 的 NSIS 安装器支持 `perUser` 和 `perMachine` 两种模式。`perUser` 安装到用户目录（如 `%LOCALAPPDATA%`），全程无需提权；`perMachine` 安装到 `Program Files`，安装时需要管理员权限。

**一个常见的错误设计**是：为了让更新能写入 `Program Files`，让整个应用以管理员身份运行。这引入了一组不可接受的风险与副作用：

| 风险                           | 具体表现                                                     |
| ------------------------------ | ------------------------------------------------------------ |
| **WebView 被攻破即系统沦陷**   | Tauri 的 WebView 是弱链接。如果主进程持有管理员令牌，XSS 或依赖链攻击直接获得系统级写权限。 |
| **子进程继承提权令牌**         | 从提权进程启动的任何子进程（包括 sidecar）默认继承相同的完整性级别和令牌，扩大攻击面。 |
| **与外部工具不兼容**           | 部分开发工具或运行时（如 Unity）拒绝从提权进程启动，导致 sidecar 或插件失效。 |
| **安装器启动的进程可能仍提权** | Tauri 的 `perMachine` NSIS 安装器在安装完成后启动应用时，启动的进程可能仍保持管理员权限，这是一个已知的行为边界。 |

因此，**用户级主进程 + 受限提权 worker** 不是可选的优化，而是安全架构的硬性要求。Tauri 社区推荐的做法正是将特权逻辑分离到独立的二进制或受限进程中，而非整体提权。

### 1.2 worker 的职责边界

提权 worker **只做一件事**：在 Journal 的保护下，对根目录 R 执行 `Place`、`Backup`、`Trash`、`Restore`、`RemoveTrash` 五类文件操作。它不下载补丁、不解析清单、不执行补丁应用、不与前端通信、不持有除目标路径之外的任何写权限。

**worker 的操作集合是封闭的**。所有可执行的操作在编译期由枚举定义，worker 不接受“任意文件写入”或“任意命令执行”的指令。特权操作被显式枚举，确保提权进程没有任意代码执行面。这是受限提权 helper 的通用原则，不是本项目的独创。

**非提权安装不需要 worker**：per-user 与便携版由替换进程以当前用户身份直接执行同一份 Journal 逻辑。两条路径共用 `PrivilegedWriter` 抽象，差别只在“谁有权限写根目录”。


## 2. 双进程架构

### 2.1 进程拓扑

```
┌──────────────────────────────────────────────────────────────────┐
│  用户级主进程（非提权）                                           │
│                                                                   │
│  · Tauri UI / WebView                                            │
│  · 清单获取、验签、差异比对                                       │
│  · 补丁下载、差分应用、暂存区构建（prepare）                       │
│  · Journal 的 begin() 与 commit() 决策                            │
│  · 状态机、事件广播                                               │
│  · 不直接写入根目录 R                                             │
└───────────────────────────────┬──────────────────────────────────┘
                                │ 启动 worker（UAC 提示）
                                │ 命令行只传 commit_id
                                ▼
┌──────────────────────────────────────────────────────────────────┐
│  提权 worker（管理员）                                            │
│                                                                   │
│  · 仅接收已序列化的 Journal 操作列表                               │
│  · 校验 checksum、路径、签名清单与每个 Place 的目标哈希           │
│  · 执行 Place / Backup / Trash                                    │
│  · 写 Journal 状态（completed / failed）                          │
│  · 不接受其他任何指令                                             │
│  · 操作完成后退出，不常驻                                         │
└──────────────────────────────────────────────────────────────────┘
```

**关键分离**：`prepare` 阶段完全在用户级进程完成。暂存区位于用户可写的应用数据目录。**只有 `Commit` 阶段的文件替换需要提权**，而这一阶段的全部决策已经由用户级进程通过 Journal 固化，worker 只负责执行，不参与决策。

### 2.2 worker 的身份

worker 是**主可执行文件的受限模式**，而非另一个独立分发的二进制。主进程以 `--delta-update-apply <journal_dir> <commit_id> [--relaunch]`（提交）或 `--delta-update-recover <journal_dir> <crash_loop_threshold>`（启动恢复）参数启动自身，worker 模式在 `main()` 的最早期检测到该参数后，绕过 Tauri 启动流程，进入 worker 事件循环。

**为什么用同一二进制**：代码签名和分发完整性。如果 worker 是独立二进制，它需要单独签名、单独分发、单独验证。使用同一已签名的可执行文件，确保提权启动的代码与用户级进程是同一份受信代码，没有额外的信任锚。

**worker 模式的入口约束**：worker 在启动后**不加载 WebView、不初始化 Tauri、不读取前端资源**。它只加载文件操作和 Journal 相关的最小依赖。这从物理上消除了 worker 被前端攻击面波及的可能。


## 3. worker 的启动与通信

### 3.1 启动时机

worker 在**应用退出后**由 supervisor 启动，而非在应用运行期间启动。这带来两个好处：

1. 运行中的 exe/dll 不再被主进程锁定，`rename` 和 `move` 的锁定冲突面最小。
2. 用户不会在应用使用过程中被打断。UAC 提示出现在应用已经退出之后，语义清晰：“更新即将应用，需要一次授权”。

时序的判定标准是“用户是否还在使用应用”：应用退出前只做决策与写日志，退出后才执行文件替换，因此 UAC 提示不会出现在用户操作过程中，也不会与运行中的 exe/dll 抢锁。

**唯一的例外是启动期恢复 worker**（见 §3.4）：上次提交/回滚中断时，恢复必须在主窗口创建前完成，因此它以提权方式在应用启动阶段同步运行并等待退出。它不参与新更新的决策，只消费磁盘上已经固化的 Journal。

### 3.2 通信通道

supervisor 与 worker 之间**没有 IPC 通道**。这是一个刻意的简化：提交 worker 在 supervisor 退出后才开始运行，两者没有重叠的生命周期；启动期恢复 worker 虽然与父进程同时存在，但也只读取磁盘上的 `apply.json` 与 Journal，父进程仅通过等待进程退出获知完成。命名管道只会增加一个可被本地进程争夺的攻击面。

**唯一的输入是磁盘上的 Journal**：

| 项           | 内容                                                                 |
| ------------ | -------------------------------------------------------------------- |
| 启动参数     | `--delta-update-apply <journal_dir> <commit_id> [--relaunch]`（提交模式）；`--delta-update-recover <journal_dir> <crash_loop_threshold>`（启动期恢复模式）。不传操作列表，`--relaunch` 仅用于 `restart_to_apply` 的重启路径 |
| 启动等待     | 提交模式先等待父进程 PID 退出（有上限，超时则放弃并写 `failed`）；恢复模式不等待父进程，直接拿锁执行 |
| 路径来源     | `journal_dir/apply.json`（supervisor 写入）：安装根、暂存根、journal 根；worker 校验三者都是绝对路径、`install_root` **精确等于 worker 可执行文件所在目录**、staging/journal 不在 `install_root` 内 |
| 操作来源     | `journal_dir/commit.journal`，worker 从磁盘读取并校验 checksum 与每个 `Place`/`Restore` 的哈希 |
| 唯一性       | 全局 `update.lock` 保证同一时刻只有一个 worker 在执行                 |
| 输出         | 进程退出码 + 原子写入的 `commit.status`（`completed` / `failed { op_index, message }`） |

**为什么这样更安全**：worker 不连接任何 IPC 端点，就没有“谁在向 worker 发指令”的问题；它只执行磁盘上已经由 supervisor 固化、且每个操作都带内容哈希的 Journal。即使本地攻击者创建了同名 `commit_id` 的文件，也会在 checksum 或哈希校验处失败。`install_root` 必须包含当前 worker 可执行文件这一约束，确保被第三方启动的 worker 也只能改写自己所在的安装目录。

### 3.3 操作语义的封闭性

worker 只识别五种操作：

```rust
enum WorkerOp {
    Backup  { src: RelPath, dst: RelPath },
    Place   { staging_src: RelPath, dst: RelPath, sha256: Hash, size: u64 },
    Trash   { src: RelPath, trash_dst: RelPath },
    Restore { backup_src: RelPath, dst: RelPath, sha256: Hash, size: u64 },
    RemoveTrash { trash_path: RelPath },
}
```

**没有 `WriteArbitrary`、没有 `Execute`、没有“按任意路径删除”**。所有路径在 worker 侧重新执行逃逸检查（拼接后必须仍在根目录 R 之下），并且只接受相对路径。worker 不信任 Journal 中的路径未经检查——即使 Journal 由 supervisor 生成，worker 也独立验证。

**`Place`/`Restore` 的哈希是硬要求**：worker 执行前校验来源、执行后校验目标。没有哈希，worker 只能“把某个文件搬到某个位置”，无法证明搬过去的是不是清单里的字节。

### 3.4 启动期恢复 worker

上次更新在提交或回滚中途中断时，下一次启动必须先恢复 Journal。per-machine 安装的恢复同样需要写安装根，因此插件在 `setup` 最早期判断：

1. 读取 `commit.journal` / `rollback.journal` / `commit.status` 与 `attempts`，判断本次恢复是否需要写安装根（`recovery_needs_install_write`）。**“已提交、等待健康确认且未超过崩溃阈值”的正常启动不触发提权**。
2. 需要写时再探测安装根可写性。可写则直接在用户级执行；不可写则通过 `ShellExecuteExW("runas")` 启动同一二进制的 `--delta-update-recover` 模式，并等待 worker 退出（上限 120 秒）。
3. worker 读取 `apply.json`，重新执行绝对路径、`install_root` 包含自身、staging/journal 不在安装根内三项校验，然后按正常恢复语义重放或回滚；它不等待父进程退出，因为启动恢复必须在主窗口创建前完成。
4. worker 把本次启动计入一次尝试；父进程随后用 `recover_after_elevation` 读取结果，不再重复计数。父进程崩溃或 worker 超时都不会导致尝试次数翻倍。

取消 UAC 时启动恢复无法进行，插件进入 `Failed`（`E_PERMISSION_UAC_CANCELLED`），Journal 与回滚素材原样保留；下次启动会再次请求授权。**不会在用户级强行恢复**，避免留下半更新状态。


## 4. 提权决策矩阵

提权与否不是由“是否安装到 Program Files”这一个因素决定的，而是由**目标路径的实际可写性**决定。

| 安装形态                    | 根目录 R                 | prepare 阶段                   | Commit 阶段                                       | UAC 提示时机              |
| --------------------------- | ------------------------ | ------------------------------ | ------------------------------------------------- | ------------------------- |
| `perUser` 安装              | `%LOCALAPPDATA%\<App>`   | 用户级，无需提权               | 用户级可直接写入                                  | **无 UAC**                |
| 便携版 / 绿色版             | 用户选择的可写目录       | 用户级                         | 用户级                                            | **无 UAC**                |
| `perMachine` 安装           | `C:\Program Files\<App>` | 用户级（暂存区在应用数据目录） | **需要提权**                                      | 应用退出后，worker 启动前 |
| `perMachine` 且主进程已提权 | `C:\Program Files\<App>` | 用户级（主进程通常不提权）      | 子进程继承提权令牌，**不再弹 UAC**                 | 无（父进程提权时）         |

**判定方式**：在 `prepare` 完成后，supervisor 用**带 PID、序号与时间戳的唯一文件名**在根目录 R 中创建探测文件（失败时重试一次，避免瞬时占用误判）。创建成功说明替换进程可以以当前用户令牌写入；失败（`ACCESS_DENIED`）则标记该次提交需要提权。**这比仅检查安装路径更可靠**，因为它同时覆盖了 ACL 被修改、组策略限制等非标准场景。

**“已经是管理员”的特殊情况**：如果主进程本身已经以管理员身份运行（例如从提权安装器直接启动），它**仍然 spawn 替换进程**，只是子进程继承提权令牌、不再弹 UAC。**不在主进程内执行 Commit**——这条不变式比省一次进程创建更重要：主进程内的 WebView、业务线程和第三方依赖都不应该出现在文件替换的调用栈上。

**提权后的降权重启**：`restart_to_apply` 的 `--relaunch` 由 worker 通过 Explorer 启动新版本，而不是直接 `CreateProcess`。否则提权 worker 会把管理员令牌传给应用主进程，违反“用户级主进程”约束。


## 5. 提权失败与降级路径

### 5.1 用户取消 UAC

这是最常见的提权失败场景。worker 进程无法启动，supervisor 收到启动失败信号。

**系统行为**：

1. **不**尝试在用户级执行 Commit（必然失败，且可能留下半更新状态）。
2. **保留 `PREPARED` 计划**。暂存区的变化文件、新版本文件全集、已下载的补丁全部原样保留。
3. **撤销提交点**：删除未执行的 `commit.journal` 与 `apply.json`，保留 `prepared.json`。状态保持 `Prepared`（PREPARED 仍然有效，比 `Failed` 更准确）；`restart_to_apply` 返回 `E_PERMISSION_UAC_CANCELLED`，退出路径记录日志。
4. 下一次启动时，`recover_and_cleanup()` 检测到 `PREPARED` 计划仍然有效，**直接复用暂存区**，下次退出时重新尝试提权提交。用户不需要重新下载或重新 prepare。

**用户可读建议**： “更新需要管理员权限才能完成，请允许授权后重试。” 不需要解释技术细节。

### 5.2 worker 执行中失败

如果 worker 已启动但在执行 Journal 操作时失败（磁盘满、文件被锁定、权限被 ACL 拒绝），worker 将错误写入 **`commit.status`**（原子写入）并返回非零退出码。Journal 本身在提交后不可变，**不会**被 worker 修改。下一次启动时主管进程读取状态：

| worker 结果                   | 下次启动行为                                                 |
| ----------------------------- | ------------------------------------------------------------ |
| `completed`                   | 迁移到 `Committed`，等待 `confirm_healthy`                    |
| `failed { op_index, message }` | 迁移到 `Failed`，**保留 Journal 和 `.old` 备份**；下次启动先幂等重放，仍失败则按逆日志回滚并跳过该版本 |
| worker 异常退出（无状态写入） | 视为 `failed`，行为同上                                      |

**关键原则**：worker 失败**不意味着回滚**。`.old` 备份和 `.trash` 文件是回滚素材，不是失败证据。是否回滚由 `recover()` 在下次启动时根据 journal 和根目录状态决定，而非 worker 即时决定。

**per-machine 回滚的自动化**：回滚和 `.old` 清理同样要写安装根。下次启动时，插件先判断恢复是否需要写安装根，再探测可写性；不可写时自动以 `--delta-update-recover` 提权执行回滚，用户只需在启动时确认一次 UAC，不再需要“以管理员身份启动应用”或手工清理。取消授权或提权被策略禁用时，Journal 与回滚素材保留，状态进入 `Failed`，下次启动重试。`confirm_healthy` 的备份清理仍是尽力而为：它只删除 `.old`/`.trash`，失败只会留下残留目录，不影响应用运行，也不改变更新状态。

### 5.3 提权 worker 无法启动的其他原因

| 原因              | 检测方式                                                    | 降级行为                                         |
| ----------------- | ----------------------------------------------------------- | ------------------------------------------------ |
| UAC 被组策略禁用  | `CreateProcess` 返回 `ERROR_ELEVATION_REQUIRED`             | `E_PERMISSION_DENIED`，保留 `PREPARED`           |
| 安全软件拦截      | worker 进程被终止或无法创建                                 | `E_PERMISSION_DENIED`，日志记录，保留 `PREPARED` |
| worker 二进制损坏 | 签名验证失败（如果 supervisor 验证 worker 的 Authenticode） | `E_INTERNAL`，不启动 worker，保留 `PREPARED`     |

**提权账户必须能访问 journal**：journal 与暂存区默认位于当前用户的 `%LOCALAPPDATA%`。如果 UAC 使用“另一管理员账户”完成提权（over-the-shoulder），该账户可能无法读取当前用户的 AppData，worker 会以 `E_NOT_PREPARED` 或权限错误失败。标准做法是让管理员账户完成安装并使用同一账户的 split-token 提权；不要为了绕过此限制把 journal 目录放宽到所有用户可写。


## 6. 与 Journal 和 Committer 的协作

### 6.1 职责切分

| 模块                       | 运行位置             | 职责                                               |
| -------------------------- | -------------------- | -------------------------------------------------- |
| **Journal `begin()`**      | supervisor（用户级） | 生成操作列表、flush 到磁盘（提交点）               |
| **Journal `recover()`**    | 替换进程 / 下次启动  | 重放判定；需要提权时由 worker 执行                 |
| **Committer（提权安装）**  | **worker（提权）**   | 在 Journal 保护下执行五类文件操作                  |
| **Committer（非提权）**    | 替换进程（当前用户） | 目标路径可写时，不经 worker 直接执行               |

**Journal 的 `begin()` 始终在用户级完成**。这意味着操作列表的生成和提交点的建立不依赖提权。worker 只消费已经固化的操作列表。即使 worker 被攻破或行为异常，它也无法改变“要替换哪些文件”的 intent——intent 在它启动之前已经写入 Journal。

### 6.2 worker 不参与决策

worker 不比较清单、不计算差异、不决定备份到何处。这些决策全部在 `begin()` 之前由 Manifest 和 Staging 模块完成。worker 的输入是**已固化的操作列表**，输出是**每个操作的成功/失败状态**。这使 worker 的实现极小，可审查面窄，且不需要包含任何网络、差分或 JSON 解析依赖。


## 7. 安全约束汇总

| 约束                       | 实现方式                                            |
| -------------------------- | --------------------------------------------------- |
| worker 不接受任意代码      | 操作类型编译期枚举，无 `Execute` / `WriteArbitrary` |
| worker 不信任路径          | 在 worker 侧重新执行逃逸检查                        |
| worker 不接受外部指令      | 没有 IPC；唯一输入是磁盘上已固化的 Journal          |
| worker 不接收操作列表      | 从磁盘读取 Journal，每个 `Place`/`Restore` 校验哈希 |
| worker 不常驻              | 完成或失败后退出，不保留管理员令牌                  |
| 同名 Journal 不可伪造      | checksum + 路径逃逸检查 + `commit.meta.json` 绑定签名清单 + `Place` 哈希必须来自签名清单 + install_root 精确等于 worker 目录；旧 `run_applier_from_args` 迁移路径不做签名绑定 |
| 并发唯一                   | 全局 `update.lock` 独占                              |
| 用户级进程不持有管理员令牌 | 即使从提权安装器启动，也把文件操作委托给替换进程    |


## 8. 平台边界

Windows 的 UAC 提权模型是本设计的基础，其他平台按“是否真的需要提权”收敛：

| 平台 / 形态               | 提权路径                                                     | 是否支持文件级替换 |
| ------------------------- | ------------------------------------------------------------ | ------------------ |
| Windows per-user / 便携版 | 不需要提权，替换进程以当前用户运行                           | 是                 |
| Windows per-machine       | UAC 启动同一二进制的替换模式                                 | 是                 |
| Linux AppImage / tar      | 插件不支持；AppImage 只读挂载，tar 不保留可执行位            | 否                 |
| Linux deb / rpm           | 属于包管理器，应用不应直接改；插件不支持                     | 否                 |
| Linux Flatpak / Snap      | 只读沙箱，插件不支持                                         | 否                 |
| macOS 未签名开发包        | 插件不支持；资源不在 exe 父目录                              | 否                 |
| macOS 签名/公证 .app      | 修改 bundle 内部文件会使签名失效，插件不支持                 | 否                 |

**结论**：插件只支持 Windows per-user / per-machine / 便携版。其他平台在 setup 阶段直接进入 Disabled/Failed 并返回 `E_UNSUPPORTED_PLATFORM`，不下载、不替换；不要用提权或文件替换破坏 macOS 签名或 Linux 包管理器状态。worker 协议与 Journal 协作逻辑本身是平台无关的。


## 9. 设计检查清单

- [ ] 主进程不主动请求提权；即使从提权安装器启动，也不在进程内执行 Commit
- [ ] 替换进程是同一已签名二进制的受限模式，不加载 WebView / Tauri
- [ ] worker 要求 install_root 精确等于自身可执行文件目录，拒绝更宽祖先目录
- [ ] worker 的操作类型是编译期枚举，无任意写入/执行
- [ ] `Place` / `Restore` 携带目标哈希，执行前后均校验
- [ ] worker 没有 IPC，唯一输入是磁盘上的 Journal
- [ ] Journal checksum 与每个 `Place`/`Restore` 的来源/目标哈希均被校验
- [ ] worker 在用户侧和 worker 侧双重执行路径逃逸检查
- [ ] 提权决策基于目标路径实际可写性，而非仅安装形态
- [ ] 可写性探针使用唯一文件名，失败时重试一次
- [ ] 用户取消 UAC 时保留 `PREPARED` 计划与暂存区
- [ ] 启动期恢复只在需要写安装根时提权，正常“已提交待确认”启动不弹 UAC
- [ ] 启动期提权 worker 与父进程共享同一次尝试计数，不重复累加
- [ ] worker 失败不即时回滚，回滚决策由 `recover()` 在下次启动时做出
- [ ] 跨卷安装走“目标目录临时文件 + 原子 rename”，不直接 move


**本文的核心判断**：提权设计的“安全”不来自 UAC 弹窗本身，而来自 **worker 的执行范围被 Journal 和枚举约束封闭**。只要 worker 只能做 `Backup` / `Place` / `Trash` / `Restore` / `RemoveTrash`，且只操作根目录内的已校验路径，并且 `Place` / `Restore` 的目标哈希被强制执行，即使 worker 的输入被部分篡改，也无法将恶意字节写入系统。提权是一种**执行权限的委托**，而非**决策权限的委托**——决策始终在用户级进程完成并固化，worker 只是受约束的执行者。