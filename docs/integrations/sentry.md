# 把更新历史上报到 Sentry

插件只把终态记录写在本地 `journal/update_history.jsonl`，**不发起任何网络请求**。是否上报、上报到哪里由应用决定。本页给出一个最小实现，把 `getUpdateHistory()` 的结果转成 Sentry 事件或日志。

## 1. 读取与去重

`get_update_history` 返回最近的记录（默认 50 条）。应用需要自己记住“上次上报到哪一条”，否则每次轮询都会重复上报。`version + finished_at_unix` 足以作为幂等键。

```ts
import * as Sentry from '@sentry/browser';
import { getUpdateHistory, type UpdateRecord } from 'tauri-plugin-delta-update-api';

let lastReported = 0;

export async function reportUpdateHistory(): Promise<void> {
  const records = await getUpdateHistory(50);
  for (const record of records) {
    if (record.finished_at_unix <= lastReported) continue;
    emit(record);
    lastReported = Math.max(lastReported, record.finished_at_unix);
  }
}
```

`lastReported` 只保存在内存里：应用重启后最多重复上报最近 50 条。需要跨重启去重时，把 `version + finished_at_unix` 的哈希写进应用自己的设置存储，不要写回 journal。

## 2. 映射到 Sentry

```ts
function emit(record: UpdateRecord): void {
  const tags = {
    'update.result': record.result,
    'update.version': record.version,
    'update.from_version': record.from_version ?? 'unknown',
  };

  if (record.result === 'failed') {
    Sentry.captureMessage(`update failed: ${record.error_code ?? 'unknown'}`, {
      level: 'error',
      tags,
      extra: { duration_ms: record.duration_ms, version: record.version },
    });
    return;
  }

  Sentry.addBreadcrumb({
    category: 'delta-update',
    level: record.result === 'rolled_back' ? 'warning' : 'info',
    message: `update ${record.result} to ${record.version}`,
    data: { ...tags, duration_ms: record.duration_ms, reason: record.reason },
  });
}
```

只管终态：`completed` / `rolled_back` / `failed`。下载进度、补丁字节数等高频数据留在 `worker.log`，不要进入 Sentry。

## 3. 建议的指标

| 指标 | 来源 | 用途 |
| --- | --- | --- |
| 更新成功率 | `result == completed` / 总数 | 发布健康度 |
| 回滚率 | `result == rolled_back` / 总数 | 新版本稳定性 |
| 升级耗时 | `duration_ms` 的 p50 / p95 | 大版本或网络变化 |
| 失败错误码 | `error_code` 分布 | 定向排障 |

按 `version` 与 `from_version` 分组，可以直接对比“发布者构建时覆盖率”和“客户端实际命中率”。

## 4. 隐私

`UpdateRecord` 不含路径、用户名或设备标识。不要为了“更好分组”额外附加 `client_id`、机器名或安装路径——这些信息一旦进入第三方日志就难以撤回。需要关联诊断时，让用户在应用内导出诊断包，再人工附到工单。
