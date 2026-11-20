# Delta Update Engine（增量更新内核）

面向 **Windows 10+ 桌面 Tauri 2** 的文件级增量更新内核：只下载变化的文件，应用退出后由独立替换进程完成替换，下次启动即为新版本；失败可回滚，签名先于落盘。

> **`1.1.0` 已发布** — 公共 API 保持兼容。本版新增：`init` 默认的 `manage-confirm` 权限集、Rust 侧健康确认、多签名与单对象信封、`skip_version`、诊断包更新史、并行下载与校验。接入反馈请用 [反馈模板](https://github.com/zuoqiuning/delta-update-engine/issues/new?template=rc-feedback.yml)。

| 支持矩阵 | |
| --- | --- |
| 操作系统 | Windows 10+ 桌面（x64 / arm64） |
| 安装形态 | per-user（`%LocalAppData%`）、per-machine（`%ProgramFiles%`，替换进程走 UAC）、便携版（配置 `install_dir`） |
| 不支持 | macOS、Linux、移动端：这些平台由安装器或包管理器持有完整性模型，请使用官方 `tauri-plugin-updater` 或系统包管理器 |
| 非 Windows 构建 | 编译期直接失败（`compile_error!`），不会静默禁用更新 |

> 核心 crate 为 `delta-update-engine`，Tauri 接入层为 `tauri-plugin-delta-update`，发布侧为 `delta-update-cli`。

## 安装

```bash
cargo add delta-update-engine@1.1.0
cargo add tauri-plugin-delta-update@1.1.0
pnpm add tauri-plugin-delta-update-api
```

npm 包的 `latest` 指向当前稳定版；需要锁定次要版本时写 `tauri-plugin-delta-update = "1.1"`。

## 快速接入（零前端代码）

```powershell
# 写入 capability，并把 plugins.delta-update 合并进 tauri.conf.json（保留 .bak）
delta-update init --auto --merge-config --signing-key release.key
delta-update doctor --check-config --dir .
```

```toml
# src-tauri/Cargo.toml
tauri-plugin-delta-update = "1.1.0"
```

```rust
fn main() {
    let context = tauri::generate_context!();
    tauri::Builder::default()
        .plugin(tauri_plugin_delta_update::init())
        .run_with_delta_update(context)
        .expect("failed to run the app");
}
```

`--auto` 打开 `auto_check/auto_apply/auto_confirm`：启动检查并准备、退出后由独立进程替换、下次启动自动确认。需要进度条、“稍后提醒”或自定义 UI 时再接入前端 SDK。完整说明见[开发者接入](docs/guides/getting-started.md)。

## 何时使用

| 场景 | 选择 |
| --- | --- |
| Windows 桌面应用体积大、每次发版只改少量文件 | 本插件：只下载变化的文件 |
| 发布清单、签名与灰度需要自托管 | 本插件：清单 + minisign + COS/S3/本地目录 |
| macOS / Linux / 移动端 | 官方 `tauri-plugin-updater` 或系统包管理器 |
| 接受整包替换、希望维护成本最低 | 官方 `tauri-plugin-updater`（以安装包为单位） |
| 通过应用商店分发 | 商店渠道，不接入自更新 |

与官方 updater 的关系：官方 updater 以**安装包**为单位替换，签名与端到端流程由 Tauri 维护；本插件以**文件**为单位打补丁，需要自己发布清单、维护对象存储与密钥，换来的是升级下载量下降和可回滚的写前日志。两者不叠加——同一应用只接入一种更新通道。

### 为什么选 delta-update

| 维度 | 官方 `tauri-plugin-updater` | 本插件 |
| --- | --- | --- |
| 更新单位 | 完整安装包（NSIS / MSI / bundle） | 变化文件的补丁 + 全量回退 |
| 下载量 | 与安装包同量级 | 只下载差异；未变化文件不传输 |
| 平台 | Windows / macOS / Linux | 仅 Windows 10+ 桌面 |
| 签名模型 | minisign 清单；安装包由安装器/平台校验 | minisign 清单；Journal 与签名清单绑定，worker 重新验签 |
| 应用时机 | 用户确认后运行安装器 | 退出后由替换进程落地，下次启动即新版本 |
| 回滚 | 无内建版本回滚，需重新发布旧版本 | 崩溃循环自动回滚；`rollback --to-version` 一键切回 |
| 失败恢复 | 依赖安装器与用户重试 | 写前 Journal 幂等重放，失败按逆日志恢复 |
| 维护成本 | 低（Tauri 官方维护） | 需要自建清单、对象存储、密钥与发布流水线 |
| 适用 | 跨平台、安装包小、希望少运维 | Windows、体积大、发版频繁、需要可控回滚 |

第三方增量方案（例如 `tauri-delta-updater`）通常也需要自建差分与发布链路。选择时优先确认它的信任模型、回滚语义与 Windows 安装形态，而不是只看压缩率。

Windows 环境矩阵（已验证 / 未验证）见[兼容性文档](docs/reference/compatibility.md#11-windows-环境矩阵已验证--未验证)。

## 仓库结构

```
crates/delta-update-engine         纯 Rust 引擎（无 Tauri 依赖，可被任意 Rust 程序使用）
crates/tauri-plugin-delta-update   Tauri 2 插件
crates/delta-update-cli            构建侧发布 CLI
crates/delta-update-testkit        测试夹具（不发布）
packages/plugin-api                前端 invoke/类型封装
examples/minimal-tauri-app         最小接入示例
e2e                                跨进程端到端测试
xtask                              仓库自动化
docs                               设计、接入、参考、运维与测试文档
```

目录与依赖规则见 [目录结构规范](docs/guides/directory-structure.md)。

## 开发命令

```bash
cargo check --workspace
cargo test --workspace
cargo xtask ci
cargo xtask contract-check   # 退出码 / JSON 信封契约
cargo xtask bench-check      # criterion 基线对比（配合 CI 的 --baseline main）
cargo run -p delta-update-cli -- serve --dir ./channel --port 8123  # 本地托管发布目录
cargo run -p delta-update-cli -- report --history .reports --json   # CI 覆盖率门禁
pnpm install
pnpm typecheck
```

CI 集成只依赖 [CLI 契约](docs/cli-contract.md) 里的退出码与 JSON 顶层字段；发布流水线可用 `delta-update release --pipeline` 一步完成生成、自测、上传与审计。

## 文档

- [文档索引](docs/README.md)
- [开发者接入](docs/guides/getting-started.md)
- [5 分钟演示](real-test/README.md)
- [测试计划](docs/testing/test-plan.md)
- [安全策略](SECURITY.md)
- [版本与兼容承诺](docs/reference/versioning.md)
- [贡献指南](CONTRIBUTING.md)

## 许可

MIT
