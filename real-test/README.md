# 真实更新测试

这个文件夹用来真实跑一次“生成补丁 → 客户端更新”的全流程，不需要 Tauri 界面，也不需要 COS。

先说明一个容易误解的点：**更新器不是对两个安装包做差分，而是对“安装目录里的文件”做差分**。真实的第一次安装仍然由安装包完成；之后的增量更新只下载变化的文件/补丁，再由替换进程写入安装目录。

这里准备了两个软件目录：

```
work/v1/     Demo App v1.0.0
work/v2/     Demo App v2.0.0
```

两个目录里有：

- `demo-app.exe`：同一个演示程序，启动时读取旁边的 `version.txt` 并打印版本。
- `version.txt`：v1 是 `1.0.0`，v2 是 `2.0.0`。
- `resources/payload.bin`：约 2 MiB 的资源文件，v2 只改了其中 256 字节。
- `demo-config.json`：更新地址与测试公钥。

## 前置条件

| 依赖        | 用途                                   |
| ----------- | -------------------------------------- |
| PowerShell 7+ | 执行脚本                             |
| Rust stable | 编译演示程序、真实 CLI 与内置静态服务 |

本地 HTTP 服务由 `delta-update serve --dir <DIR> --port <PORT>` 提供，不再需要 Python。也可以用它托管任意 `release --bucket <DIR>` 的输出。

## 运行

在仓库根目录执行：

```powershell
pwsh -ExecutionPolicy Bypass -File .\real-test\run.ps1
```

脚本会自动完成：

1. 生成一对**测试专用** minisign 密钥（`real-test/keys/`）。
2. 编译 `real-test/demo-app` 演示程序。
3. 构造 v1、v2 两个软件目录。
4. 调用真实 CLI `delta-update release` 生成补丁、清单、签名，发布到 `real-test/work/public`。
5. 启动本地 HTTP 服务。
6. 把 v1 安装到 `real-test/work/install`，运行更新，再验证版本变成 v2.0.0，最后执行健康确认。

## 你应该看到

- CLI 输出里，`resources/payload.bin` 的策略是 `qbsdiff+zstd`，补丁远小于 2 MiB。
- `demo-app update` 打印本次下载字节数和完整文件字节数，两者差距很大。
- 更新后 `demo-app version` 输出 `Demo App v2.0.0`。
- `confirm` 之后 `real-test/work/state/journal` 中的 Journal/PREPARED 被清理。

产物都留在 `real-test/work/`，可以直接打开看：

```
work/public/manifests/latest.json      # 清单
work/public/manifests/latest.json.minisig
work/public/artifacts/...              # 补丁和全量对象
work/install/                          # 更新后的软件目录
work/state/                            # staging / journal
```

## 说明

- `real-test/keys/` 是测试密钥，**绝不能用于正式发布**。删掉这个目录后重新运行会生成新密钥。
- 想重新跑一遍，直接再执行脚本即可；`work/` 会被清空，`keys/` 会复用。
- 这个测试只验证文件级增量更新与替换流程。真实 UAC、perMachine 安装和 Authenticode 签名需要 Windows 虚拟机手动验收，清单见 [发布手册 §12.3](../docs/operations/release.md)。
- 崩溃回滚等故障场景用现成的端到端测试：

```powershell
cargo run -p delta-update-e2e -- crash_loop_rollback
```
