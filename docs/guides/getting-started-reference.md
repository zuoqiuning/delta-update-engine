# 进阶配置、验收与版本承诺

> 本文件是[接入指南](getting-started.md)的参考部分：通常不需要修改的进阶配置、上线验收清单、版本兼容性承诺与适用规模。

## 9. 进阶配置（通常不需要）

以下配置有安全默认值，只在特殊场景下需要显式设置：

```json
{
  "plugins": {
    "delta-update": {
      "crash_loop_threshold": 3,
      "exit_grace_period_secs": 10,
      "staging_dir": "custom/path",
      "journal_dir": "custom/path",
      "max_concurrent_downloads": 4,
      "staging_size_limit": 1073741824
    }
  }
}
```

| 字段                       | 默认值                                       | 何时修改                               |
| -------------------------- | -------------------------------------------- | -------------------------------------- |
| `crash_loop_threshold`     | `3`                                          | 应用启动较慢或初始化复杂时提高；数值越大越宽松 |
| `exit_grace_period_secs`   | `10`                                         | 正常退出折扣计数的观察窗口；0 表示任何正常退出都折扣 |
| `confirm_delay_ms`         | `3000`                                       | 启动慢、首屏晚时调大；0 表示不自动确认 |
| `staging_dir`              | `app_local_data_dir()/delta-update/staging/` | 磁盘空间紧张且想放到其他盘（可跨卷）   |
| `journal_dir`              | `app_local_data_dir()/delta-update/journal/` | 同上                                   |
| `install_dir`              | 当前 exe 所在目录                            | 生产构建必须精确等于 exe 父目录；debug 开发可指向临时目录 |
| `max_concurrent_downloads` | `4`                                          | 网络环境差时降低，强网可提高           |
| `staging_size_limit`       | `2 GiB`                                      | 磁盘空间严格受限时降低                 |
| `max_patch_file_size`      | `256 MiB`                                    | 有超大 sidecar 且希望差分时提高        |
| `patch_memory_factor`      | `2.5`                                        | 内存紧张时降低，但过低保补丁失败率上升 |
| `manifest_max_bytes`       | `8 MiB`                                      | 文件数极多的大型应用可适当提高         |
| `max_files`                | `20000`                                      | 同上                                   |
| `signing_endpoint`         | 空                                           | 私有 COS 桶需要预签名服务时设置        |
| `allowed_hosts`            | 空                                           | 需要限制内容下载主机时设置             |
| `headers`                  | 空                                           | 清单/签名请求需要鉴权头时设置（不会发给对象下载） |
| `signing_headers`          | 空                                           | 预签名服务自身需要鉴权头时设置（不会发给清单或对象主机） |

**注意**：`staging_dir` 和 `journal_dir` 必须在用户有写权限的位置，且**不能在安装根目录之内**。可以放到其他盘；跨卷时替换进程会自动改用“复制到目标目录临时文件 + 原子 rename”，不会破坏原子性，但会多一次复制开销。


## 10. 接入验收清单

接入完成后，逐项确认：

- [ ] `tauri.conf.json` 中 `endpoint` 可访问，返回合法清单 JSON
- [ ] `signing_public_keys` 至少一把，且与签名私钥配对
- [ ] `capabilities/default.json` 包含 `delta-update:manage-confirm`
- [ ] 前端在 UI 就绪后调用了 `confirm_healthy()`（或接受插件默认行为）
- [ ] 监听 `delta-update://state` 事件并展示 `Prepared` 状态提示
- [ ] CI 中 `delta-update release` 能完整执行并上传到 COS
- [ ] 测试机安装旧版本 → 发布新版本 → 观察 `state = Prepared` → 退出再启动 → 版本更新
- [ ] 测试机新版本启动崩溃 → 连续三次未确认 → 第四次启动自动回滚
- [ ] 安装在 `Program Files` 时，更新流程能正确处理 UAC
- [ ] 安装在 `Program Files` 且提交被中断后，下次启动能提权恢复（取消则保留 Journal）
- [ ] 断网后恢复网络，更新能续传并完成


## 11. 版本兼容性承诺

| 插件版本 | 支持的 Tauri 版本 | 文件级替换支持的形态                              | 协议 schema |
| -------- | ----------------- | ------------------------------------------------- | ----------- |
| 1.0.x    | 2.x               | Windows 10+ x64 桌面（per-user / per-machine / 便携版） | 1           |

**插件版本语义化**：

- **Patch**（1.0.0 → 1.0.1）：Bug 修复，命令和配置不变。
- **Minor**（1.0.x → 1.1.0）：新增命令或配置字段，向后兼容。旧配置无需修改。
- **Major**（1.x → 2.0）：可能包含命令或配置的破坏性变更，会提供迁移指南。

**协议 schema 独立于插件版本**。清单 schema 演进规则见协议与数据格式规范。插件会拒绝 `min_client_schema` 高于自身支持值的清单，并给出明确错误。

**“已验证的 Tauri 版本”指仓库 `Cargo.lock` 中解析出的版本**（`1.1.0` 验证时为 `tauri 2.12.5`），而不是 workspace 里写的 `tauri = "2"`。宽版本配合 lockfile 保证可复现；升级 Tauri 时请重跑 `cargo test --workspace`、`cargo run -p delta-update-e2e` 与 `real-test/demo-app` 的冒烟，再更新本节记录的版本号。

### 11.1 多平台共存

- Windows 走 `delta-update`，macOS / Linux 继续用官方 `tauri-plugin-updater` 或各自平台包管理器。文件级替换只适用于 Windows。
- **不要同时启用两个 updater**：`plugins.updater` 与 `plugins.delta-update` 并存会产生两套检查循环和两份清单。
- 多平台构建在 CI 中按 target 分开：Windows 产物跑 `delta-update release`，其他平台产物不经过本项目。
- 如果必须共用 `main.rs`，用 `#[cfg(target_os = "windows")]` 条件初始化插件。

### 11.2 适用规模

| 规模 | 建议 |
| --- | --- |
| 文件数 < 5k、总大小 < 500 MiB | 默认配置即可；`check` / `prepare` 通常在秒级 |
| 文件数 5k–20k | `HashCache` 对 `check` 有效；`prepare` 的未变化文件校验可能需要几分钟，UI 会收到 `Scanning` 进度事件 |
| 文件数 > 20k | 默认 `max_files = 20000` 会拒绝清单，需要同时调大 `max_files`、`staging_size_limit` 与 `max_concurrent_downloads` |
| 单文件 > 64 MiB | 调整 `--patch-max-file-size` 并降低 `--jobs`；发布端会按 6× 文件大小估算峰值内存，超过 75% 物理内存时告警 |


## 12. 接入总结

接入体验的核心判断是：**开发者不需要理解增量更新引擎的内部机制，只需要做三个正确的决定**——配置正确的 `endpoint` 和公钥、授予正确的权限、在合适的时机调用 `confirm_healthy()`。其余全部由插件在后台完成。

如果接入过程中遇到本文未覆盖的问题，请优先收集 §8.7 列出的诊断信息，再对照错误码表定位类别。错误码是稳定的——它们不会在不同版本间改变含义，可以安全地作为程序分支的依据。
