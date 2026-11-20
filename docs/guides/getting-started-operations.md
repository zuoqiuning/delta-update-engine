# 构建、发布与常见问题

> 本文件是[接入指南](getting-started.md)的后续部分：构建发布侧、最小示例结构、常见问题排查。

## 6. 构建与发布侧（简要）

本节只给出最小必要流程。完整参数和 CI 配置见构建发布操作手册。

### 6.1 发布一条命令

```bash
# 先把私钥从 secret 解码到临时文件
echo "$MINISIGN_SECRET_KEY_BASE64" | base64 -d > /tmp/release.key

delta-update release \
  --from  ./dist/2.0.0 \
  --from-version 2.0.0 \
  --to    ./dist/2.1.0 \
  --version 2.1.0 \
  --bucket cos://your-bucket \
  --key   /tmp/release.key

rm -f /tmp/release.key
```

CLI 内部会：
1. 扫描 `to/` 目录，生成 `latest.json`。
2. 逐文件比对 `from/` 与 `to/`，对可差分的变化文件生成 qbsdiff+zstd `.tdp`；超过阈值时回退全量。
3. 用 minisign 私钥对清单签名。
4. **先上传补丁和全量对象，再上传签名，最后上传清单**（原子发布顺序）。

### 6.2 CI 中注入私钥

```yaml
# GitHub Actions 示例
- name: Release delta update
  env:
    MINISIGN_SECRET_KEY_BASE64: ${{ secrets.MINISIGN_SECRET_KEY_BASE64 }}
  run: |
    echo "$MINISIGN_SECRET_KEY_BASE64" | base64 -d > /tmp/release.key
    delta-update release \
      --from ./previous-dist \
      --from-version 1.4.2 \
      --to   ./dist \
      --version 1.5.0 \
      --bucket cos://your-bucket \
      --key  /tmp/release.key
    rm -f /tmp/release.key
```

**私钥绝不落仓库、绝不落日志、绝不出现在命令行参数里**。CLI 只接受私钥文件路径；临时文件在步骤结束前删除。

### 6.3 首次发布

第一次发布时没有 `from` 目录——所有文件都是新增，全部走全量。这是预期行为：

```bash
delta-update release --to ./dist --version 1.0.0 --bucket cos://your-bucket --key /tmp/release.key --initial
```

从第二个版本开始，`--from` 提供上一版本的产物目录，CLI 才能生成差分补丁。


## 7. 最小可运行示例项目结构

```
my-tauri-app/
├── src/                          # 前端
│   ├── main.ts
│   └── update-ui.ts              # 更新提示 UI（可选）
├── src-tauri/
│   ├── src/
│   │   └── main.rs               # 注册插件
│   ├── capabilities/
│   │   └── default.json          # 授予 delta-update:manage-confirm
│   ├── tauri.conf.json           # 插件配置
│   └── Cargo.toml
├── dist/                         # 构建输出（用于发布）
│   ├── 2.0.0/
│   └── 2.1.0/
└── .github/workflows/
    └── release.yml               # CLI 发布流水线
```

**最小验证步骤**：

1. 发布 2.0.0（`--initial`）。
2. 安装 2.0.0 到一台测试机。
3. 修改一点代码，构建 2.1.0。
4. 发布 2.1.0（`--from ./dist/2.0.0`）。
5. 在测试机上启动 2.0.0，观察日志或事件。
6. 等待 `delta-update://state` 事件出现 `Prepared` 后，关闭应用（替换进程会应用更新），再重新打开。
7. 应用版本应为 2.1.0，且**全程没有出现安装器界面**。


## 8. 常见问题排查

### 8.1 启动后没有任何更新事件

| 可能原因                | 排查方式                                                     |
| ----------------------- | ------------------------------------------------------------ |
| `auto_check` 为 `false` | 检查配置                                                     |
| `endpoint` 不可达       | 浏览器直接访问 `endpoint` URL，确认返回清单 JSON             |
| 清单验签失败            | 检查公钥是否配置正确、清单签名是否由对应私钥生成             |
| 版本未变化              | 确认远程 `version` 严格大于当前已安装版本                    |
| 权限未授予              | 检查 `capabilities/default.json` 是否包含 `delta-update:manage-confirm` |
| 看不到任何插件日志      | 插件只发出 `tracing` 事件，宿主未安装 subscriber 时不会输出；启用插件的 `logging` feature 或自装 subscriber，见[故障排查 §1.1](../operations/troubleshooting.md) |

### 8.2 状态停在 `Failed`，错误为 `E_SIGNATURE_INVALID`

- 检查 `signing_public_keys` 中的公钥是否与签名私钥配对。
- 确认签名覆盖的是**原始清单字节**，而非解析后的 JSON。
- 若刚执行过密钥轮换，确认客户端版本是否已内嵌新公钥（信任集不会在运行期更新）。

### 8.3 状态停在 `Failed`，错误为 `E_NETWORK_PRESIGN_EXPIRED`

- 预签名 URL 有效期建议设为 **10–15 分钟**（安全上限 15 分钟）。若频繁过期，检查服务端返回的 `expires_at` 与客户端时钟偏差，而不是无限延长有效期。
- 检查签名服务是否可达、是否返回了清单中所有 key 的 URL。

### 8.4 更新准备成功但重启后仍是旧版本

| 可能原因                    | 排查方式                                                     |
| --------------------------- | ------------------------------------------------------------ |
| 安装目录定位错误            | 更新器只以当前 exe 所在目录为准；确认启动的是同一安装路径，检查 `install_dir` 是否精确指向 exe 父目录 |
| Commit 阶段权限不足         | 错误码应为 `E_PERMISSION_DENIED`，检查是否安装在 `Program Files` |
| `.old/` 目录残留导致状态异常 | 检查安装目录下是否有 `.old/<commit_id>/` 目录，确认对应 Journal 状态 |
| 重启的是另一个安装实例      | 确认启动的是同一个安装路径                                   |

### 8.5 更新后应用反复回滚

- 检查新版本是否有启动崩溃。
- 确认前端是否在 UI 就绪后调用了 `confirm_healthy()`。
- 如果应用确实启动正常但总是回滚，可能是 `confirm_healthy` 调用时机过早（在核心数据加载完成前就崩溃）。
- 查看应用数据目录下的日志，定位崩溃点。

### 8.6 磁盘占用异常增长

- 检查应用本地数据目录下的 `delta-update/staging/` 大小。
- 已成功确认健康的更新会清理暂存区；如果暂存区持续增长，说明 `confirm_healthy()` 未被调用，或清理逻辑异常。
- 检查 `journal/` 目录是否有残留的 Journal 文件。

### 8.7 如何收集诊断信息

当用户报告更新问题时，请收集：

1. 应用本地数据目录下 `delta-update/staging/` 和 `delta-update/journal/` 的**文件名列表**（不要内容，可能包含敏感路径）。
2. 应用日志中 `delta-update` 相关的全部行。
3. 用户安装目录的绝对路径（帮助判断是否在 `Program Files`）。
4. `status` 命令的返回值。
5. 是否使用管理员权限运行。


