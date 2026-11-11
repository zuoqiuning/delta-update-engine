# delta-update-cli

构建侧发布 CLI：扫描产物、生成补丁、构建清单、minisign 签名、发布到 COS 或本地目录。

```bash
cargo install delta-update-cli

# 增量发布
delta-update release \
  --from old/ --from-version 1.2.2 \
  --to new/ --version 1.2.3 \
  --bucket cos://your-bucket --region ap-guangzhou \
  --key release.key

# 首次发布到本地目录（离线验证）
delta-update release --to dist --version 1.0.0 --bucket ./public --key release.key --initial

# 发布后验证
delta-update verify \
  --bucket cos://your-bucket --region ap-guangzhou \
  --version 1.2.3 --public-key release.pub

delta-update verify-key --key release.key --public "$MINISIGN_PUBLIC_KEYS"
delta-update doctor --endpoint https://example.com/manifests/latest.json --public-key release.pub
```

| 命令         | 作用                                             |
| ------------ | ------------------------------------------------ |
| `release`    | 生成补丁与清单、签名、原子发布                   |
| `verify`     | 从发布端回读清单、验签、抽样验证补丁             |
| `verify-key` | 校验私钥可用性与公钥匹配                         |
| `doctor`     | 检查端点、清单、对象可访问性与版本一致性         |

`--bucket` 接受 `cos://<bucket>` 或本地目录（`file://` 也可）；本地目录用于离线验证与静态托管。
COS 凭证从 `COS_SECRET_ID` / `COS_SECRET_KEY` 读取，可选 `COS_SESSION_TOKEN`；区域可用 `--region` 或 `COS_REGION`。
加密私钥的密码从 `MINISIGN_PASSWORD` 读取，**不要**把私钥或密码放进命令行参数。

发布顺序、参数与回滚流程见 [构建发布手册](../../docs/operations/release.md)。
