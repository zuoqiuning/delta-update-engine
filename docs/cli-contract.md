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
  "errors": []
}
```

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
  "diagnosis": "…"
}
```

`attempts > crash_loop_threshold` 时下一次启动会回滚；`crash_loop_threshold` 在机器首次启动后才有值。

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
  "files": 128,
  "changed": 6,
  "objects": 7,
  "delta_bytes": 12000,
  "changed_bytes": 1048576,
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

首次发布（`--initial`）时 `self_test` 为 `{ "ran": false, "reason": "initial release" }`；未传 `--watch` 时 `watch` 为 `null`。任一步失败都不会写报告，命令以对应退出码退出。

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
  "removed": 1
}
```

`strategy`/`size`/`delta_size` 仅在同时提供 `--from-release` 时存在。

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

### 2.7 `serve` 的边界

`serve` 没有 JSON 输出，也不属于发布契约。它只绑定 loopback，用于本地开发与演示：**loopback 不是信任边界，它不防御同机恶意进程**；不要用 `socat` / `netsh portproxy` 转发到公网。没有 TLS、鉴权、目录列表或缓存，不是 CDN。

## 3. 门禁

`cargo run -p xtask -- contract-check` 静态检查：

1. `docs/cli-contract.md` 存在且包含 0/1/2/3 四个退出码；
2. 每个 JSON 生产点仍输出 `v` 与 `tool` 信封。

`xtask ci` 会执行该检查，因此新增 `--json` 输出时忘记加信封会导致 CI 失败。

## 4. 语言与本地化策略

CLI 的 `--help` 为英文，错误与 `user_action` 为中文；这是当前目标用户的取舍，不在 rc 阶段引入 i18n。

需要英文错误的脚本必须使用 `--json`，从 `error.code` / `error.action` 做本地化，**不要解析 `error.message` 或 `user_action`**——两者是给人读的，措辞会变。`--json` 的 `code` / `action` 是稳定契约。
