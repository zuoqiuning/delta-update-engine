# 从零到发布：CI 集成

本文把“第一次在 CI 里发布 delta 更新”拆成可复制的步骤。模板文件在 [`examples/ci/`](../../examples/ci/)；本文解释为什么这样配。

## 1. 准备密钥

```bash
minisign -G -p release.pub -s release.key
delta-update key-info --key release.key   # 打印 base64 公钥与 key ID
```

- 公钥写进 `tauri.conf.json` 的 `plugins.delta-update.signing_public_keys`，随应用二进制分发。
- 私钥内容放进 CI secret（例如 `MINISIGN_SECRET_KEY`），**不要**提交到仓库，也不要放进命令行参数。
- 如果私钥有口令，另存 `MINISIGN_PASSWORD` secret。

第一次发布（`--initial`）也需要同样签名；之后轮换密钥的规则见 [版本与兼容承诺](../reference/versioning.md)。

## 2. 构建与打包

CI 先产出两个目录：

- `build/previous/`：上一个已发布版本的构建产物（从上一个 tag 的 artifact 下载或从缓存恢复）
- `target/release/`：本次构建产物

`--from` 必须是**上一次发布的目录**，不是上一个 tag 的源码；否则补丁会以错误的基准生成，覆盖率骤降。

## 3. 一行发布

```bash
delta-update release \
  --from build/previous --to target/release \
  --version "$VERSION" --from-version "$PREVIOUS_VERSION" \
  --bucket "cos://$COS_BUCKET" --key "$RUNNER_TEMP/release.key" \
  --pipeline --report .delta-update-reports/ --watch 600
```

`--pipeline` 依次执行生成、自测（apply + rollback）、上传、审计；`--watch 600` 在审计后继续检查 10 分钟，覆盖 CDN 传播延迟。任一步失败即停止。

## 4. 报告归档

`--report .delta-update-reports/` 会写 `.delta-update-reports/<version>.json`。把整个目录作为 CI artifact 保存，之后可以：

```bash
delta-update report --history .delta-update-reports/
```

趋势表会标出覆盖率下降超过 20 个百分点的版本与新增回退文件。

## 5. 退出码与重试

| 退出码 | 处理 |
| --- | --- |
| 0 | 成功 |
| 1 | 暂时性失败（网络/文件占用），重试整个 job |
| 2 | 需要人工处理（签名、配置、对象缺失），不要盲目重试 |
| 3 | 内部错误，收集日志并报告 |

```bash
delta-update release ... || {
  code=$?
  if [ "$code" -eq 1 ]; then exit 1; fi   # let CI retry
  echo "release needs a human (exit $code)" >&2
  exit "$code"
}
```

## 6. 发布后的观察

`--watch 600` 已经串进流水线。如果希望独立观察（例如 CDN 与镜像分开）：

```bash
delta-update doctor --endpoint https://cdn.example.com/manifests/latest.json \
  --public-key ./release.pub --watch 600 --interval 60
```

`doctor` 校验签名、版本一致性与对象可访问性；任何一轮失败都会让命令失败。

## 7. 常见问题

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| `inspect --from-release` 报 `missing` | 清单已上传但对象没传完 | 重跑 `release`（幂等）或补传对象 |
| 客户端 `E_SIGNATURE_NO_TRUSTED_KEY` | 客户端公钥与发布私钥不匹配 | `delta-update verify-key` 确认配对 |
| 覆盖率突然下降 | 某个文件膨胀超过 `--patch-max-file-size` | 查看 `report --history` 的 `new fallbacks`；调整阈值或排除调试符号 |
| `--pipeline` 在 self-test 失败 | 补丁能生成但不能在真实安装树上还原 | 保留 `--from` 与 `--to` 产物用于复现；不要跳过自测发布 |
| tag 触发的 release 失败 | tag 版本与 `Cargo.toml`/npm 版本不一致 | 先执行 `cargo xtask version-sync`，再打 tag |

## 8. 最小检查清单

- [ ] `release.key` 只存在于 CI secret，工作区文件在 job 结束后删除
- [ ] `--from` 指向真正的上一版构建产物
- [ ] `--report` 目录作为 artifact 归档
- [ ] `--watch` 的窗口覆盖 CDN 传播时间
- [ ] 退出码 2 不会自动重试
