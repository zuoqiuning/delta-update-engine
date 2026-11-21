# CLI 契约

CI 只依赖这里列出的退出码与 JSON 顶层字段。字段可以新增，但不能重命名或删除；破坏性变更必须提升 `"v"`。

## 1. 退出码

所有子命令共用一套映射，由 `EngineError::exit_code()` 定义：

| 退出码 | 含义 | 典型场景 |
| --- | --- | --- |
| 0 | 成功 | — |
| 1 | 暂时性失败，重试可能成功 | 网络超时、文件被占用、更新锁 |
| 2 | 需要人工处理 | 配置、签名、清单、磁盘、权限 |
| 3 | 内部错误 | 引擎不变量被破坏，需要报告 |
| clap 的 2 | 参数错误 | 缺少必填参数、互斥参数冲突 |

`inspect --journal-dir` 额外约定：`1` = 存在未完成的 Journal（重启应用即可恢复），`2` = 状态损坏（需要人工介入）。其他文件/IO 类错误（非引擎错误）保持 `1`。

`rollback --to-version` 沿用同一映射：旧清单缺失/验签失败/非法版本为 `2`，对象存储暂时不可达为 `1`。事故响应脚本应只在 `0` 时视为已切换 `latest.json`。

CI 推荐：

```bash
delta-update release --pipeline ...
code=$?
if [ "$code" -eq 1 ]; then
  echo "transient failure; retry the job" >&2
elif [ "$code" -ne 0 ]; then
  echo "release needs attention (exit $code)" >&2
  exit 1
fi
```

## 2. JSON 输出

每个 `--json` 输出的顶层都包含：

```json
{ "v": 1, "tool": "delta-update-cli", "...": "..." }
```

- `v` 是格式版本；新增字段不提升版本，删除或改义才提升。
- `tool` 固定为 `delta-update-cli`，便于日志管道区分来源。

### 2.1 `doctor --json`

```json
{
  "v": 1,
  "tool": "delta-update-cli",
  "mode": "endpoints",
  "ok": true,
  "consistent": true,
  "versions": ["2.0.0"],
  "endpoints": [
    { "endpoint": "https://…/latest.json", "ok": true, "version": "2.0.0", "verified": true, "errors": [] }
  ]
}
```

`doctor --check-config --json` 复用同一信封，用 `mode` 区分：

```json
{
  "v": 1,
  "tool": "delta-update-cli",
  "mode": "config",
  "ok": true,
  "config": "src-tauri/tauri.conf.json",
  "endpoint": "https://…/manifests/latest.json",
  "channel": "stable",
  "config_version": 1,
  "install_mode": "currentUser",
  "confirm_health_granted": true,
  "errors": [],
  "warnings": []
}
```

`install_mode` 取自 `bundle.windows.nsis.installMode`（缺失时为 `null`）。`confirm_health_granted` 表示 `src-tauri/capabilities/*.json` 里是否有集合（或直接权限）授予 `confirm_healthy`；它与 `auto_confirm` 都为 `false` 时，`warnings` 会指出回滚保护实际关闭，并建议改用 `delta-update:manage-confirm`、设置 `auto_confirm: true` 或运行 `init --auto`。`src-tauri/capabilities` 下无法解析的 JSON 文件会写进 `errors` 并使 `ok` 为 `false`（退出码非零），因为 Tauri 会在启动时因它失败。

`warnings` 非空不改变退出码，但会指出安装形态与更新策略的冲突：`perMachine` / `both` 搭配 `auto_apply: true` 会在用户未预期时触发 UAC。

`doctor --journal-dir <DIR> --install-root <DIR> --json` 离线校验安装树，同样复用该信封：

```json
{
  "v": 1,
  "tool": "delta-update-cli",
  "mode": "verify-files",
  "ok": false,
  "manifest": "installed",
  "version": "2.0.0",
  "checked": 42,
  "missing": ["assets/icon.png"],
  "mismatched": [
    { "path": "App.exe", "expected": "…", "actual": "…" }
  ]
}
```

`manifest` 为 `installed`（确认健康后的清单）或 `verified`（尚未确认时的更新前快照）；`ok` 为 `false` 时退出码非零，便于 CI 与支持脚本直接判断。

### 2.2 `inspect --journal-dir --json`

`DiagnosticReport` 的字段加上信封；关键字段：

```json
{
  "v": 1,
  "tool": "delta-update-cli",
  "journal_dir": "…",
  "status": { "state": "completed", "version": "2.0.0", "commit_id": "…" },
  "journal": { "file": "commit.journal", "version": "2.0.0", "from_version": "1.9.0", "ops": 12 },
  "attempts": 2,
  "crash_loop_threshold": 3,
  "skipped_version": null,
  "warnings": [],
  "diagnosis": "…",
  "worker_event": { "name": "apply_ok", "ts": "1763000042", "msg": null },
  "history": [
    { "v": 1, "version": "2.0.0", "finished_at_unix": 1763000042, "result": "completed" }
  ]
}
```

`attempts > crash_loop_threshold` 时下一次启动会回滚；`crash_loop_threshold` 在机器首次启动后才有值。`worker_event` 是 `worker.log` 里最新一条结构化事件，只在存在时出现；日志是旧版纯文本时该字段缺失。`history` 是 `update_history.jsonl` 里最新的 10 条终态（`completed` / `rolled_back` / `failed`），没有历史记录时为空数组。

### 2.3 `inspect --from-release --json`

```json
{
  "v": 1,
  "tool": "delta-update-cli",
  "version": "2.0.0",
  "channel": "stable",
  "files": [
    { "path": "bin/app.exe", "strategy": "qbsdiff+zstd", "size": 1048576, "delta_size": 4096, "ratio": 0.0039 }
  ],
  "delta_bytes": 4096,
  "patched_full_bytes": 1048576,
  "missing_objects": []
}
```

`missing_objects` 非空时命令以 `2` 退出。

### 2.4 `release --pipeline` 报告

`--pipeline` 成功后写 `--report <PATH>`（默认 `release-report.json`）：

```json
{
  "v": 1,
  "tool": "delta-update-cli",
  "generated_at_unix": 1763000000,
  "version": "2.0.0",
  "channel": "stable",
  "bucket": "cos://app-releases",
  "prefix": "releases",
  "release_kind": "incremental",
  "files": 128,
  "changed": 6,
  "objects": 7,
  "delta_bytes": 12000,
  "changed_bytes": 1048576,
  "client": {
    "full_download_bytes": 4096,
    "net_savings_bytes": 1032480
  },
  "publish": {
    "uploaded_full_bytes": 8192,
    "uploaded_delta_bytes": 12000,
    "reused_full_bytes": 1036288,
    "reused_full_blobs": 120
  },
  "coverage": {
    "modified": 5,
    "patched": 4,
    "fallback": { "too_large": 1, "ratio": 0, "incompressible": 0 }
  },
  "fallbacks": [{ "path": "bin/large.dll", "reason": "too_large" }],
  "self_test": { "ran": true, "millis": 42 },
  "audit": { "ok": true, "files": 128, "patched": 4, "delta_bytes": 12000, "patched_full_bytes": 1048576, "missing_objects": [] },
  "watch": { "seconds": 600, "interval": 60, "rounds": 11 }
}
```

首次发布（`--initial`）时 `self_test` 为 `{ "ran": false, "reason": "initial release" }`，`release_kind` 为 `"initial"`；未传 `--watch` 时 `watch` 为 `null`。任一步失败都不会写报告，命令以对应退出码退出。

报告把**客户端收益**与**发布侧成本**分开，避免把“上传了多少”误解为“用户要下多少”：

- `client.net_savings_bytes = patched_full - delta_bytes`：`patched_full` 的定义是**实际走 delta 的文件的新版本 full size 之和**（回退成 full 的 Modified 文件不计入），`delta_bytes` 是这些补丁的字节数。`changed_bytes` 包含新增与回退全量的文件，不能当节省额。
- `client.full_download_bytes = changed_bytes - patched_full`：客户端仍需以整文件下载的变化文件（新增 + 回退）。
- `publish.uploaded_full_bytes` / `publish.uploaded_delta_bytes`：本次真正写入发布端的字节。全量对象按内容寻址，键已存在且大小一致时跳过，计入 `reused_full_bytes` / `reused_full_blobs`。
- `baselines`：仅包含 `--baseline` 指定的额外基线，每项 `{version, patches, delta_bytes, client: {patched_full_bytes, net_savings_bytes}}`；未使用该参数时为空数组。顶层 `client` 与 `coverage` 只描述主基线（`--from`），避免多基线把“相邻版本节省”摊薄；每个额外基线的收益在自己的 `client` 里。
- `release_kind`：`initial`（无 `--from`，没有基线）、`incremental`（有 `--from-version`）、`rollback`（`--rollback`，重新发布旧版本）。`initial` 时 `client.net_savings_bytes` 与 `publish.reused_full_blobs` 都是 0，`publish.uploaded_full_bytes` ≈ 全部文件大小，这是正常形态而不是“增量没有收益”。
- `--pipeline` 与 `--dry-run` 互斥，所以正常流程里这些字段总有意义。

### 2.5 `inspect --bundle --json`

```json
{
  "v": 1,
  "tool": "delta-update-cli",
  "bundle": "delta-update-diagnostics-1763000000.json",
  "version": "2.0.0",
  "files": [
    {
      "path": "bin/core.dll",
      "strategy": "qbsdiff+zstd",
      "size": 12000,
      "delta_size": 3000,
      "ops": ["Place (landed)"],
      "fallbacks": ["E_PATCH_APPLY_FAILED"]
    }
  ],
  "patched": 9,
  "fell_back": 1,
  "full_by_design": 2,
  "removed": 1,
  "journal_only": [],
  "worker_event": { "name": "apply_ok", "ts": "1763000042", "msg": null },
  "history": [
    { "v": 1, "version": "2.1.0", "finished_at_unix": 1763000142, "result": "rolled_back", "reason": "crash loop" }
  ]
}
```

`strategy`/`size`/`delta_size` 仅在同时提供 `--from-release` 时存在。`history` 是诊断包里最新 5 条更新终态（新→旧）；旧版插件导出的包没有该字段，此时为 `[]`。

`journal_only` 列出“journal 触碰过、但签名清单没有”的路径（排除被删除的文件）。正常 bundle 应为空——worker 会对每个 `Place` 对照签名清单校验；非空说明 bundle 混了版本或 journal 被篡改。反过来“清单有、journal 无”不列为异常：未变化的文件本来就不会出现在 journal 里。

`worker_event` 是 `worker.log` 里最新一条结构化事件（`event=`），`msg` 只对失败事件存在；日志是旧版纯文本时该字段为 `null`。它让 CI 与工单系统不必解析日志就能看到“这次到底是成功还是失败”。

### 2.6 `report --history --json`

`release --pipeline` 报告的跨版本趋势，供 CI 做覆盖率门禁：

```json
{
  "v": 1,
  "tool": "delta-update-cli",
  "history": ".reports",
  "releases": [
    {
      "version": "2.1.0",
      "changed": 15,
      "modified": 15,
      "patched": 6,
      "coverage_percent": 40.0,
      "delta_bytes": 1024,
      "changed_bytes": 4096,
      "fallbacks": [{ "path": "bin/large.dll", "reason": "too_large" }],
      "coverage_drop": {
        "from": "2.0.0",
        "points": 35.0,
        "new_fallbacks": [{ "path": "bin/large.dll", "reason": "too_large" }]
      }
    }
  ]
}
```

`releases` 按 SemVer 升序；`coverage` 是每个版本一行，`coverage_drop` 相对**上一版本**，单位是**百分点**（从 80% 降到 60% 记 20，不是 25%），仅在下降超过 20 个点时非空。报告文件本身无法解析时命令失败；版本号不是 SemVer 时跳过并告警。

CI 门禁示例（覆盖率下降超过 20 点即失败）：

```bash
delta-update report --history .reports --json > coverage.json
jq -e 'all(.releases[]; ((.coverage_drop // {points: 0}).points <= 20))' coverage.json
```

列出所有下降：

```bash
jq '.releases[] | select(.coverage_drop != null) | {version, drop: .coverage_drop.points}' coverage.json
```

### 2.7 `doctor --simulate-failure <MODE> --json`

`lock` / `patch` / `disk` 三个场景分别用真实代码路径产出 `E_UPDATE_LOCKED`、`E_PATCH_SOURCE_MISMATCH`、`E_DISK_INSUFFICIENT`，用于验证客户端错误界面，不写任何状态、不需要 `--endpoint`：

```json
{
  "v": 1,
  "tool": "delta-update-cli",
  "mode": "simulate",
  "ok": true,
  "scenario": "lock",
  "simulated_error": {
    "code": "E_UPDATE_LOCKED",
    "category": "journal",
    "message": "update is locked by another process",
    "retryable": true,
    "action": "wait",
    "user_action": "另一个实例正在执行更新，请等待其完成或关闭后重试",
    "detail": ""
  }
}
```

命令本身成功（退出码 `0`）：这是模拟，不是故障。CI 断言：

```bash
delta-update doctor --simulate-failure lock --json | jq -e '.simulated_error.code == "E_UPDATE_LOCKED"'
```

`disk` 场景读取真实可用空间并要求多 1 字节，因此 `message` 里的数字随机器变化，断言只用 `code` / `action`。

`patch` 场景可用 `--reason` 选择具体失败分支，信封多一个 `reason` 字段（`lock` / `disk` 时为 `null`）：

| `--reason` | 产出 | 触发的真实路径 |
| --- | --- | --- |
| `source-mismatch`（默认） | `E_PATCH_SOURCE_MISMATCH` | `apply_patch` 比对源文件哈希 |
| `format` | `E_PATCH_FORMAT_MISMATCH` | 容器短于 82 字节固定头 |
| `target-hash` | `E_PATCH_TARGET_HASH_MISMATCH` | 复用 `prepared.json` 时的 `verify_plan` |
| `memory` | `E_PATCH_MEMORY_BUDGET` | `apply_patch` 的内存预算上限 |

```json
{ "scenario": "patch", "reason": "target-hash", "simulated_error": { "code": "E_PATCH_TARGET_HASH_MISMATCH", "...": "..." } }
```

`--reason` 只接受 `--simulate-failure patch`，搭配其他模式会直接报参数错误。

### 2.8 `serve` 的边界

`serve` 没有 JSON 输出，也不属于发布契约。它只绑定 loopback，用于本地开发与演示：**loopback 不是信任边界，它不防御同机恶意进程**；不要用 `socat` / `netsh portproxy` 转发到公网。没有 TLS、鉴权、目录列表或缓存，不是 CDN。

### 2.9 `key-info` / `verify-key` 的 `--json`

```json
{
  "v": 1,
  "tool": "delta-update-cli",
  "public_key": "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3",
  "key_id": "1F2E3D4C5B6A7988"
}
```

`key_id` 为大写十六进制。输出只含公钥信息，脚本可以直接用它填充 `signing_public_keys` 或 `init --public-key`，不必解析人类输出。

`verify-key --json` 输出 `{ "v": 1, "tool": "delta-update-cli", "matches": true, "public_key": "…" }`；不匹配时仍打印该 JSON，并以退出码 `1` 结束，脚本可以同时依赖 `matches` 与退出码。

### 2.10 `init` / `migrate` 的 `--json`

两者默认输出给人看的分步说明；加 `--json` 后 stdout 只有一个 JSON 对象，说明文字全部消失，警告改走 stderr，脚本与 Agent 可以直接解析：

```json
{
  "v": 1,
  "tool": "delta-update-cli",
  "endpoint": "https://example.com/manifests/latest.json",
  "channel": "stable",
  "signing_public_keys": ["RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3"],
  "permission_set": "manage-confirm",
  "auto": false,
  "reused_from_config": ["endpoint"],
  "config_merged": false,
  "schema_written": true,
  "schema_path": "src-tauri/delta-update.schema.json",
  "capability_written": true,
  "capability_path": "src-tauri/capabilities/delta-update.json",
  "config": { "config_version": 1, "endpoint": "https://example.com/manifests/latest.json" },
  "warnings": []
}
```

`--write`/`--merge-config` 还会写入 `src-tauri/delta-update.schema.json` 并在配置里加 `$schema`；`schema_written` 为 `null` 表示本次没有写入权限（只打印配置），`false` 表示文件已存在。`migrate --json` 的信封同样包含这两个字段。

`migrate --json` 返回 `{ v, tool, config, capability, written, schema_written, schema_path, capability_written, guide_written, guide_path, guide }`：`guide` 是完整迁移指南，未加 `--write` 时也可直接落盘。`capability_written` / `guide_written` / `schema_written` 为 `null` 表示本次没有尝试写入，`false` 表示文件已存在、未被覆盖；`reused_from_config` 列出从现有 `tauri.conf.json` 复用的字段。

### 2.11 `promote --json`

```json
{
  "v": 1,
  "tool": "delta-update-cli",
  "version": "2.1.0",
  "from_channel": "canary",
  "channel": "stable",
  "manifest_bytes": 48213,
  "signatures": 1,
  "dry_run": false
}
```

`promote` 只改写 `channel` 并重签，补丁与对象不变。`--dry-run` 时 `dry_run` 为 `true` 且不写任何对象；失败时以非零退出码结束并只输出人类可读错误。

## 3. 门禁

`cargo run -p xtask -- contract-check` 静态检查：

1. `docs/cli-contract.md` 存在且包含 0/1/2/3 四个退出码；
2. 每个 JSON 生产点仍输出 `v` 与 `tool` 信封。

`xtask ci` 会执行该检查，因此新增 `--json` 输出时忘记加信封会导致 CI 失败。

## 4. 语言与本地化策略

CLI 的 `--help` 为英文，错误与 `user_action` 为中文；这是当前目标用户的取舍，不在 rc 阶段引入 i18n。

需要英文错误的脚本必须使用 `--json`，从 `error.code` / `error.action` 做本地化，**不要解析 `error.message` 或 `user_action`**——两者是给人读的，措辞会变。`--json` 的 `code` / `action` 是稳定契约。
