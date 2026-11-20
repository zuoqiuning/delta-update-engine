# 配置、演进与示例

> 本文件是 [协议规范](protocol.md) 的配置与演进部分：插件配置 schema、崩溃循环/更新锁、签名服务、schema 演进与最小示例。

## 9. 插件配置协议

```json
{
  "plugins": {
    "delta-update": {
      "config_version": 1,
      "endpoint": "https://…/manifests/latest.json",
      "signing_public_keys": ["RWQ…", "RWQ…"],
      "auto_check": true,
      "auto_apply": true,
      "channel": "stable"
    }
  }
}
```

| 字段                  | 类型   | 必填 | 默认       | 语义                                |
| --------------------- | ------ | ---- | ---------- | ----------------------------------- |
| `config_version`      | u32    | 是   | —          | 配置格式版本，当前 `1`。            |
| `endpoint`            | string | 是   | —          | 清单 URL，必须是 HTTPS（本地调试见 §9.4）。 |
| `signing_public_keys` | array  | 是   | —          | 受信 minisign 公钥集合，至少 1 把。 |
| `auto_check`          | bool   | 否   | `true`     | setup 后是否自动后台 check + prepare。 |
| `auto_apply`          | bool   | 否   | `false`    | 应用正常退出时是否自动执行已就绪的更新；显式开启。 |
| `auto_confirm`        | bool   | 否   | `false`    | 主窗口页面加载后是否自动健康确认；显式开启。 |
| `channel`             | string | 否   | `"stable"` | 更新通道，与清单 `channel` 比对。   |

**高级配置**（可省略，有安全默认值）：

| 字段                       | 默认                                        | 语义                                      |
| -------------------------- | ------------------------------------------- | ----------------------------------------- |
| `crash_loop_threshold`     | `2`                                         | 启动尝试计数超过该值即回滚（见 §9.2）     |
| `confirm_delay_ms`         | `3000`                                      | `auto_confirm` 的延迟；0 表示不自动确认   |
| `check_interval_secs`      | 空（关闭）                                  | 定时 check + prepare 间隔，最小 60 秒     |
| `staging_dir`              | `app_local_data_dir()/delta-update/staging/` | 暂存区；跨卷时自动降级为复制+原子替换     |
| `journal_dir`              | `app_local_data_dir()/delta-update/journal/` | Journal 与锁文件目录                     |
| `install_dir`              | 自动解析                                    | 便携版等自动解析失败时显式指定安装根      |
| `max_concurrent_downloads` | `4`                                         | 并发下载数                                |
| `staging_size_limit`       | `2 GiB`                                     | 暂存区上限；只物化变化文件，通常远低于此  |
| `max_patch_file_size`      | `268435456`（256 MiB）                      | 超过该大小的文件直接走全量，不进差分管线  |
| `patch_memory_factor`      | `2.5`                                       | 补丁应用内存预算 = 文件大小 × 该系数      |
| `manifest_max_bytes`       | `8388608`（8 MiB）                          | 清单大小上限                              |
| `max_files`                | `20000`                                     | 清单条目数上限                            |
| `signing_endpoint`         | 空                                          | 私有桶的预签名服务；见 §9.5               |
| `allowed_hosts`            | 空（不限制，仅允许 https）                  | 允许的内容下载主机白名单                  |
| `allow_insecure_http`      | `false`                                     | 仅本地测试；生产必须为 `false`            |
| `proxy`                    | 空                                          | HTTP/SOCKS 代理 URL，企业内网使用          |
| `headers`                  | 空                                          | 附加到清单/签名请求的请求头；不发送到对象下载 |

### 9.1 配置校验时机

配置在任何网络请求之前完成校验。以下情况在 `setup` 阶段直接失败，返回 `E_CONFIG_INVALID` / `E_ENDPOINT_INSECURE`：

- 缺少 `config_version`、`endpoint` 或 `signing_public_keys`。
- `endpoint` 或 `signing_endpoint` 不是 `https://`（`allow_insecure_http` 仅允许 `http://127.0.0.1` 与 `http://localhost`）。
- 公钥不是合法的 minisign 公钥。
- `channel` 为空字符串或包含空白字符。
- `staging_dir` / `journal_dir` 位于安装根目录之内（会把更新素材暴露给替换操作）。
- `install_dir` 不存在、不是目录、不是绝对路径，或在 release 构建中不等于当前 exe 父目录。

**未知字段**：插件配置与清单都不启用 `deny_unknown_fields`，未知字段被忽略；插件配置中的未知字段会额外输出一条警告日志，便于发现拼写错误。`key_rotation` 目前不生效，出现在配置中时只警告。

### 9.2 崩溃循环判定

```
每次启动（setup 最早期）：attempts += 1
if attempts > crash_loop_threshold: 触发回滚
confirm_healthy(): attempts = 0
```

默认 `crash_loop_threshold = 2` 的含义是：**连续两次启动都没有确认健康，第三次启动时回滚**。阈值 `1` 表示第二次启动即回滚。阈值不随更新重置——持续崩溃的旧版本同样会被检测。

**回滚成功后必须把 `attempts` 重置为 0**，否则旧版本在下一次启动时会再次满足 `attempts > threshold`，触发一次无素材可用的“空回滚”。回滚完成同时记录 `skipped_version`。

### 9.3 更新锁

`journal_dir/update.lock` 是跨进程独占锁，覆盖 check 之外的 prepare、commit、recover、rollback 全过程。

- 获取失败立即返回 `E_UPDATE_LOCKED`，不阻塞等待。
- 锁文件本身是空哨兵文件，**OS 文件锁才是权威**；不要依赖文件内容判断持有者。诊断进程时用 `inspect`/`worker.log`。
- 进程崩溃后由操作系统释放锁；残留文件不影响下一次加锁。
- **同一进程内**再用一个异步互斥量串行化命令，避免同一应用的两个 WebView 并发触发 prepare。

**状态文件清单**（全部位于 `journal_dir/`，写入方式均为“临时文件 + 原子 rename”）：

| 文件               | 内容                                                         |
| ------------------ | ------------------------------------------------------------ |
| `apply.json`       | 替换进程配置：`v`、安装根、暂存根、journal 根、版本、`commit_id`、父进程 PID |
| `prepared.json`    | PREPARED 计划：版本、清单哈希、暂存文件与目标哈希、新版本文件全集 |
| `attempts`         | 启动尝试计数                                                 |
| `skipped_version`  | 回滚后跳过的远程版本                                         |
| `known_files.json` | 历史官方文件集合                                             |
| `hash_cache.json`  | 安装根文件哈希缓存（size + mtime 命中才复用；更新确认后删除）；仅加速扫描，不是信任源 |
| `commit.meta.json` | 绑定 `commit_id`、`manifest_hash`、`version`、`from_version`、`install_root`；worker 用它确认 journal 对应哪份签名清单 |
| `verified_manifest.json` / `.minisig` | 最近一次验签通过的原始清单与 detached 签名；worker 用内嵌公钥重新验签 |
| `installed_manifest.json` / `.minisig` | 已确认健康版本的签名清单；回滚 `Restore` 用它校验旧哈希 |
| `old_hashes.json`  | 提交前对被替换/删除文件的哈希快照；回滚用它校验备份，而不是信任备份文件当前字节 |
| `worker.log`       | 替换/恢复 worker 与 `prepare` 的追加日志（worker stdio 被重定向到 null）：每行一条 `event=<name> ts=<unix> key=value ...`，成功与失败都记录；超过 1 MiB 裁到最新 256 KiB |
| `commit.status`    | `{"state":"completed","version":…,"commit_id":…}` 或 `{"state":"failed","op_index":…,"message":…}` |
| `commit.journal`   | 当前提交日志；成功完成后保留到 `confirm_healthy`             |
| `rollback.journal` | 回滚进行中的逆日志；成功后删除                               |
| `update_history.jsonl` | 终态更新记录（`completed` / `rolled_back` / `failed`），每行一条 JSON；超过 256 KiB 保留最后 200 行。仅本地可读，引擎不上报 |

这些文件都**不是信任源**：它们只影响流程状态，任何内容都必须与重新验签的清单、暂存文件哈希交叉校验后才能使用。删除或篡改它们最多导致更新流程失败，不会让未校验字节进入安装目录。

### 9.4 本地调试的明文 HTTP

`allow_insecure_http: true` 只允许回环地址。这不是“生产可用的开关”，而是为了避免开发者在本地为了联调而把整个更新链路降级为明文。发布检查会拒绝该字段为 `true` 的构建。

### 9.5 签名服务协议（可选）

公开读桶不需要签名服务。私有桶配置 `signing_endpoint` 后，客户端按以下契约换取下载 URL：

**请求**

```http
POST {signing_endpoint}
Content-Type: application/json

{
  "keys": ["manifests/latest.json", "manifests/latest.json.minisig"],
  "ttl_seconds": 900
}
```

**响应**

```json
{
  "urls": {
    "manifests/latest.json": "https://bucket.cos.example.com/manifests/latest.json?q-signature=…",
    "manifests/latest.json.minisig": "https://bucket.cos.example.com/manifests/latest.json.minisig?q-signature=…"
  },
  "expires_at": "2026-11-10T08:15:00Z"
}
```

**硬约束**

- 签名服务必须使用 HTTPS；客户端拒绝 `http://` 的 `signing_endpoint`。
- 返回的 URL 必须是 HTTPS，且主机必须在 `allowed_hosts` 内（若配置了该字段）。
- `ttl_seconds` 上限 900 秒；服务端可以返回更短的有效期。
- 客户端在 URL 到期前 60 秒主动刷新；刷新失败时若当前 URL 仍有效则继续使用，否则返回 `E_NETWORK_SIGNING_SERVICE`。
- 签名服务只返回 URL，**不参与验签**。验签始终在客户端完成，服务端被攻破也无法注入未签名字节。


## 10. Schema 版本演进规则

### 10.1 兼容性矩阵

| 客户端 schema | 清单 schema | 行为                                  |
| ------------- | ----------- | ------------------------------------- |
| `1`           | `1`         | 正常                                  |
| `1`           | `2`         | **拒绝**，`E_MANIFEST_SCHEMA_TOO_NEW` |
| `2`           | `1`         | 正常（向后兼容）                      |
| `2`           | `2`         | 正常                                  |

### 10.2 演进原则

1. **新增可选字段**：不升 `schema`，旧客户端忽略未知字段。
2. **新增必填字段**：升 `schema`，旧客户端拒绝。
3. **字段语义变更**：升 `schema`，**禁止复用旧字段名**。
4. **字段删除**：升 `schema`，旧客户端因缺少必填字段拒绝。
5. **取值集合扩展**（如新增 `strategy` 字符串）：不升 `schema`。旧客户端遇到未知值时按 §2.3 处理：有 `full_key` 回退全量，无 `full_key` 视为该条目不可用；**不得**因未知值导致整份清单解析失败。

### 10.3 首次迁移策略

从“无清单旧版本”升级到本协议的第一个版本时：

- 客户端没有“历史官方文件集合”，所有本地存在但清单中不存在的文件被归类为 `Untracked`。
- **不执行任何删除操作**。
- 仅执行 `Added` 和 `Modified` 的下载与替换。
- 首次 `confirm_healthy` 后，将当前清单的文件集合写入“历史官方文件集合”，从此 `Removed` 判定生效。

**历史官方文件集合的存储**：`journal_dir/known_files.json`，写入方式为“临时文件 + 原子 rename”。它只用于判断 `Removed` 与 `Untracked`，**不参与版本、签名或路径的信任判断**。文件损坏或缺失时退化为“首次迁移”语义——所有本地多余文件按 `Untracked` 处理，不删除任何东西。

这保证了：**第一次迁移不会误删用户数据**，哪怕用户把个人文件放在了安装目录里。


## 11. 附录：完整最小示例

### 11.1 `latest.json`

```json
{
  "schema": 1,
  "version": "2.1.0",
  "channel": "stable",
  "published_at": "2026-11-10T08:00:00Z",
  "min_client_schema": 1,
  "rollback": false,
  "files": {
    "App.exe": {
      "sha256": "9f2c…",
      "size": 10485760,
      "strategy": "qbsdiff+zstd",
      "delta_key": "artifacts/2.0.0-to-2.1.0/App.exe.tdp",
      "delta_size": 78234,
      "full_key": "blobs/<sha256>",
      "executable": true
    },
    "bin/core.dll": {
      "sha256": "1a8b…",
      "size": 2097152,
      "strategy": "qbsdiff+zstd",
      "delta_key": "artifacts/2.0.0-to-2.1.0/bin/core.dll.tdp",
      "delta_size": 12048,
      "full_key": "blobs/<sha256>",
      "executable": true
    },
    "resources/icon.png": {
      "sha256": "c4d5…",
      "size": 5120,
      "strategy": "full",
      "full_key": "blobs/<sha256>"
    }
  }
}
```

### 11.2 事件示例

```json
{
  "v": 1,
  "state": "Prepared",
  "version": "2.1.0",
  "progress": null,
  "error": null,
  "timestamp": "2026-11-10T08:01:23Z"
}
```

### 11.3 错误示例

```json
{
  "code": "E_PATCH_TARGET_HASH_MISMATCH",
  "category": "patch",
  "message": "bin/core.dll: staged hash does not match the manifest",
  "retryable": true,
  "user_action": "已自动回退到全量下载，无需操作",
  "detail": ""
}
```

文件路径、期望/实际哈希等诊断信息包含在 `message`（英文、面向日志与支持）中；`detail` 只放结构化数值与枚举（状态码、字节数、schema 版本、迁移起止版本），不含路径或服务器原文，因此可以直接进 UI 与工单。`action` 才是 UI 本地化的依据。


**本文档是三端实现的共同契约。任何实现与本文档不一致，以本文档为准；任何变更先改文档，再改代码。**
