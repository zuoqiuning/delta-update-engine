# Delta Update Engine 开发者接入指南

> 本文面向使用 Delta Update Engine（`tauri-plugin-delta-update`）的 Tauri 应用开发者。目标：从零到跑通完整更新链路，理解两种工作模式，掌握前端集成方式。构建发布与排查见[构建、发布与常见问题](getting-started-operations.md)；进阶配置与验收见[进阶配置、验收与版本承诺](getting-started-reference.md)。


## 1. 你将获得什么

接入完成后，你的 Tauri 应用将具备以下行为：

- **启动时静默检查**更新，发现新版本后在后台下载并生成补丁。
- **全程不打断用户**：下载、差分、暂存都在后台完成，软件照常使用。
- **下次启动即为新版本**：无需运行安装器，无需用户点击“安装”。
- **更新失败时自动回退**：如果新版本启动即崩溃，自动回滚到上一个可用版本。
- **细粒度差分**：只下载变化的文件，已压缩/未变化的文件零传输。

最少三步：加依赖、注册插件、写配置并授予权限。§1.1 把后两步压成一条 `init` 命令；需要自定义 UI 时再按 §3 接前端 SDK。

### 1.1 最快接入：零前端代码

如果只需要“启动检查、退出替换、下次启动自动确认”，不写任何前端代码：

```powershell
# 1) 一条命令：写入 capability，并把 plugins.delta-update 合并进 tauri.conf.json
#    （原文件保留为 tauri.conf.json.bak；已有该段时不覆盖）
delta-update init --auto --merge-config --signing-key release.key

# 2) 校验配置与权限是否自洽
delta-update doctor --check-config --dir .
```

```toml
# src-tauri/Cargo.toml
tauri-plugin-delta-update = "1.2.0"   # 需要控制台日志时加 features = ["logging"]
```

```rust
// src-tauri/src/main.rs
fn main() {
    let context = tauri::generate_context!();
    tauri::Builder::default()
        .plugin(tauri_plugin_delta_update::init())
        .run_with_delta_update(context)
        .expect("failed to run the app");
}
```

完成。`--auto` 已写入 `auto_check/auto_apply/auto_confirm = true`，`--merge-config` 同时写出 `delta-update:manage-confirm` 的 capability（含 `confirm_healthy`），因此回滚保护是可用的。需要进度条、“稍后提醒”或自定义更新 UI 时，再按 §3 接入前端 SDK。

### 1.2 先看效果：5 分钟跑一次真实更新

不想先读文档，可以直接跑仓库里的真实更新演示（不需要 Tauri 界面，也不需要 COS）。前置条件只有 PowerShell 7+ 与 Rust stable——本地静态服务由 `delta-update serve` 提供，不再依赖 Python：

```powershell
pwsh -ExecutionPolicy Bypass -File .\real-test\run.ps1
```

脚本依次完成：生成测试密钥 → 编译演示程序 → 构造 v1/v2 两个安装目录 → 用真实 `delta-update release` 生成补丁、清单与签名 → 启动本地 HTTP 服务 → 安装 v1 并执行更新 → 健康确认。

看到以下输出即表示整条链路可用：

- `resources/payload.bin` 的策略是 `qbsdiff+zstd`，补丁远小于 2 MiB；
- 更新后 `demo-app version` 打印 `Demo App v2.0.0`；
- 确认后 `real-test/work/state/journal` 中的 Journal/PREPARED 被清理。

产物保留在 `real-test/work/`，清单、签名、补丁与全量对象都可以直接打开对照。完整说明见 [real-test/README.md](../../real-test/README.md)。


## 2. 前置条件

### 2.1 环境要求

| 项       | 要求                                                                 |
| -------- | -------------------------------------------------------------------- |
| Tauri    | 2.x                                                                  |
| Rust     | stable（与 Tauri 2 要求一致）                                        |
| 平台     | **仅 Windows 10+ x64 桌面**（per-user / per-machine / 便携版）        |
| 打包格式 | Windows：NSIS / MSI / 绿色版                                         |

**不支持平台/形态会在注册期直接禁用更新并返回 `E_UNSUPPORTED_PLATFORM`，不会下载任何字节**：macOS、Linux、移动端、deb/rpm/Flatpak/Snap 均不在本插件范围内。原因：文件级替换会破坏 macOS 代码签名、只读 AppImage 与包管理器所有权。其他平台请使用官方安装器/包管理器。

### 2.2 你需要准备的三样东西

**① 一个 COS 桶**（或任何支持 HTTPS 的对象存储）

插件只直接请求**清单 URL**——你的 `latest.json`，签名固定为同目录的 `latest.json.minisig`，其余对象键由清单内部指定。COS 桶的组织方式由构建工具管理，你不需要手工维护目录结构。如果使用私有桶，需要一个能生成预签名 URL 的签名服务（协议见协议文档 §9.5）。

**② 一对 minisign 密钥（至少一把，建议两把）**

```bash
# 生成第一把（release-1，日常发布用）
minisign -G -p release-1.pub -s release-1.key

# 生成第二把（release-2，轮换备用，务必离线保管）
minisign -G -p release-2.pub -s release-2.key
```

两把公钥在首次发布时就要写进客户端配置。`tauri.conf.json` 在编译期被嵌入二进制，运行期不会从磁盘重新读取，因此它等价于“编译期内嵌的信任锚”。**不要等到需要轮换时才引入第二把**——旧客户端不会信任它，轮换将无法通过更新通道完成。

取公钥不必手工复制：`delta-update init --signing-key release-1.key --signing-key release-2.key` 会从私钥派生公钥并填入 `signing_public_keys`。私钥必须带密码（`minisign -G` 默认如此），CLI 从 `MINISIGN_PASSWORD` 读取密码；用 `minisign -W` 生成的无密码密钥无法被读取，请重新生成。

**③ 一个构建侧 CLI**

`delta-update-cli` 在你的 CI 中运行，负责扫描产物、生成清单、生成补丁、签名、上传。没有它，增量更新无法自动化。


## 3. 四步接入

### 3.1 第一步与第二步：依赖/插件注册 + worker 入口

**Cargo.toml**

```toml
[dependencies]
tauri-plugin-delta-update = "1.2.0"
```

**src-tauri/src/main.rs（或 lib.rs）**

```rust
use tauri_plugin_delta_update::BuilderExt;

fn main() {
    // run_with_delta_update 先处理替换/恢复 worker（不加载 WebView），再启动 GUI。
    tauri::Builder::default()
        .plugin(tauri_plugin_delta_update::init())
        .run_with_delta_update(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

`BuilderExt::run_with_delta_update()` 在进程以 `--delta-update-apply` 或 `--delta-update-recover` 启动时先执行 worker 并退出，因此不会漏掉入口顺序。需要自定义启动流程时，仍可在 `Builder::run` 之前直接调用 `run_applier_from_context()`：它在 worker 模式返回 `Some`，从 `tauri::generate_context!()` 的内嵌配置读取公钥，攻击者无法通过改写 `apply.json`、journal 或工作目录里的 key 文件绕过。旧 API `run_applier_from_args()` 仍存在，但**没有公钥时会失败关闭**，迁移请使用 `run_applier_from_context` 或 `run_applier_with_keys`。缺少入口时，插件 setup 会返回 `E_CONFIG_INVALID` 并提示 “worker mode was not handled”，而不是静默开第二个实例。

### 3.2 第三步：写配置

**tauri.conf.json**

```json
{
  "plugins": {
    "delta-update": {
      "config_version": 1,
      "endpoint": "https://your-bucket.cos.ap-guangzhou.myqcloud.com/manifests/latest.json",
      "signing_public_keys": [
        "RWQ...release-1-public-key...",
        "RWQ...release-2-public-key..."
      ],
      "auto_check": true,
      "auto_apply": false,
      "auto_confirm": false,
      "channel": "stable"
    }
  }
}
```

| 字段                  | 必填 | 说明                                                         |
| --------------------- | ---- | ------------------------------------------------------------ |
| `config_version`      | 是   | 固定 `1`                                                     |
| `endpoint`            | 是   | 清单 URL，**必须 HTTPS**                                     |
| `signing_public_keys` | 是   | 至少 1 把，建议 2 把。内容为 minisign 公钥文件的 base64 字符串 |
| `auto_check`          | 否   | `true`（默认）：setup 后自动后台检查/准备；`false`：仅暴露命令 |
| `auto_apply`          | 否   | `false`（默认）：退出时静默替换；显式设为 `true` 才启用 |
| `auto_confirm`        | 否   | `false`（默认）：不自动健康确认；显式设为 `true` 才启用 |
| `channel`             | 否   | 默认 `stable`，与清单的 `channel` 比对                       |

**获取公钥字符串**：打开 `release-1.pub`，去掉注释行，取 base64 主体部分（通常以 `RWQ` 开头的一整行）。

也可以让 CLI 生成可直接合并的配置片段：

```bash
delta-update init \
  --endpoint https://your-bucket.cos.ap-guangzhou.myqcloud.com/manifests/latest.json \
  --public-key RWQ...release-1... \
  --public-key RWQ...release-2...
```

它会打印 `plugins.delta-update` 配置、`capabilities/default.json` 权限片段与接入检查清单。`--dir` 默认当前目录：如果存在 `src-tauri/tauri.conf.json`，会优先复用已有的 `plugins.delta-update` 配置；从官方 updater 迁移时，也会把 `plugins.updater.pubkey` 作为默认公钥（endpoint 仍需显式提供）。加 `--write` 会在 `src-tauri/capabilities/delta-update.json` 生成一个独立 capability（identifier 为 `delta-update`，不覆盖已有文件），但**不会改写 `tauri.conf.json`**——插件配置仍需按上面的片段合并。

### 3.3 第四步：授予默认权限

**src-tauri/capabilities/default.json**

```json
{
  "identifier": "default",
  "windows": ["main"],
  "permissions": [
    "core:default",
    "delta-update:manage-confirm"
  ]
}
```

`delta-update:manage-confirm` 是 `init` 默认写入的集合（`manage` + `confirm_healthy`），前端可以直接调用 `confirm_healthy()`。兼容集 `delta-update:default` 与之当前等价，包含全部 11 个命令：`check`、`prepare`、`cancel_prepare`、`status`、`confirm_healthy`、`restart_to_apply`、`export_diagnostics`、`open_diagnostics_folder`、`get_update_history`、`clear_skipped_version`、`skip_version`。

需要调整授权面时，直接换预定义权限集：

- `delta-update:manage-confirm`（**`init` 默认写入**）：`manage` + `confirm_healthy`，前端可以直接调用 `confirm_healthy()`；
- `delta-update:monitor`：仅 `check`、`status`、`get_update_history`，前端只能查看更新状态与历史；
- `delta-update:manage`：与 `manage-confirm` 相同但**不含** `confirm_healthy`，健康确认由插件（`auto_confirm`）或 Rust 侧负责；
- `delta-update:default`：兼容旧接入口，当前与 `manage-confirm` 等价。

`delta-update init` **默认生成 `manage-confirm`**，所以“按文档在前端调用 `confirm_healthy()`”开箱即用，不会被 ACL 拒绝。

> **只有一种情况需要额外选择**：显式使用 `--permission-set manage` 时，前端不能调用 `confirm_healthy()`——此时设置 `auto_confirm: true` 由插件兜底，或在 Rust 侧调用 `tauri_plugin_delta_update::confirm_healthy(app.handle())`，或改用默认的 `manage-confirm`。两者都不做时不会立刻出错，但回滚保护实际处于关闭状态。

**零前端代码**：`delta-update init --auto ...` 会写入 `auto_check/auto_apply/auto_confirm = true`，应用启动后自动检查并准备、退出时替换、下次启动自动确认，无需任何前端代码；需要进度条或“稍后提醒”时再接入 SDK。

**完成。** 默认 `auto_check=true`、`auto_apply=false`、`auto_confirm=false`：后台只检查/准备，是否在退出时静默替换、是否自动健康确认必须由应用显式开启。这样可以避免“一个可选更新功能把主应用启动搞坏”或“坏更新被过早确认”。需要全自动体验时，把 `auto_apply` 与 `auto_confirm` 设为 `true`，并确保健康确认时机符合业务实际。下面章节解释如何自定义 UI、监听事件和排查问题。

### 3.4 本地开发注意

`install_dir` 默认取**当前可执行文件所在目录**。`tauri dev` / `cargo run` 下它是 `target/debug`；如果配置指向真实发布通道且远端版本更高，`auto_check` + `auto_apply` 会在退出时用发布版替换开发二进制。

本地开发建议：

- 把 `auto_apply` 设为 `false`（或 `auto_check: false`），只调试命令与事件；
- 需要验证完整“下载 → 退出替换 → 启动新版本”链路时，显式设置 `install_dir` 为一个临时测试目录，并把旧版本复制进去；
- 用本地目录发布并托管：`delta-update release --bucket ./channel ...` 后运行 `delta-update serve --dir ./channel --port 8123`，把 endpoint 指向 `http://127.0.0.1:8123/manifests/latest.json`（只放行 loopback，配置需 `allow_insecure_http: true`）；
- 插件在 debug 构建且 `auto_apply: true` 时会输出一条 `tracing` 警告；宿主未安装 subscriber 时看不到，见[故障排查手册](../operations/troubleshooting.md)。


## 4. 两种工作模式

### 4.1 `auto_check: true`（推荐默认）

```
应用启动
   │
   ▼
插件 setup 最早期：recover_at_startup()
   │  （检测崩溃循环、清理旧备份、复用已准备的缓存；
   │    per-machine 安装需要写安装根时先经 UAC 提权恢复）
   ▼
插件 setup 稍后：后台启动 check_and_prepare()
   │  （不阻塞主线程，用户正常使用软件）
   ▼
准备完成 → 发出 delta-update://state 事件（Prepared）
   │
   ▼
用户退出应用（或调用 restart_to_apply）
   │
   ▼
替换进程在应用退出后完成替换 → 下次启动即新版本
```

**开发者需要写前端代码吗？** 不需要。插件会在后台完成全部工作。但为了用户体验，**建议监听 `delta-update://state` 事件并在 `state === 'Prepared'` 时提示“更新已准备好，下次启动生效”**——否则用户永远不知道有更新。

**关于替换进程**：应用退出后，会用**同一个可执行文件**的 `--delta-update-apply` 模式短暂启动一个无窗口进程完成替换（per-machine 安装可能先弹出 UAC）。它不是常驻服务，完成或失败后立即退出。若它被拦截或被杀，更新保持 `Prepared`，下次启动由 `recover_and_cleanup` 重放或复用；杀毒软件可能把“同一 exe 再次启动”计为行为，代码签名能显著降低误报。

**`auto_check` 只在启动时执行一次**。如果应用会长期驻留，配置 `check_interval_secs`（最小 60 秒）让插件在空闲或失败状态下周期执行 check+prepare；它会自动跳过正在准备、已准备或正在提交的状态。

### 4.2 `auto_check: false`（自定义控制）

```
应用启动
   │
   ▼
插件 setup 最早期：recover_at_startup()  ← 仍然执行
   │
   ▼
插件不再自动检查。由你的前端决定何时调用 check() 和 prepare()。
```

`auto_apply` 仍然生效：如果已有 `Prepared` 更新，应用退出时仍会自动应用。若希望连应用也完全由前端控制，把 `auto_apply` 也设为 `false`，并显式调用 `restart_to_apply`。

适用于：更新检查需要用户登录后才有意义、需要根据用户设置决定检查频率、需要自定义完整更新流程的场景。

**注意**：无论 `auto_check` 为何值，启动恢复都在 setup 最早期执行。**崩溃循环回滚和旧备份清理是引擎的引导层行为，不受配置控制**。per-machine 安装只有在恢复确实需要写安装根时才会在启动阶段弹出一次 UAC；正常的“已提交待确认”启动不会提权。


## 5. 前端集成

### 5.1 监听状态事件

**推荐先看 `createUpdateController`**。它把“启动快照 + 事件订阅 + 取消订阅 + 七个命令”收进一个框架无关的小控制器，React/Vue/Svelte 都无需自己维护监听生命周期：

```typescript
import { createUpdateController, describeUpdate } from 'tauri-plugin-delta-update-api';

const updater = createUpdateController();
const unsubscribe = updater.subscribe((state) => {
  const view = describeUpdate(state);
  if (view.isReady) showUpdateReadyBanner(state?.version ?? '');
  if (view.isRolledBack) showToast(`已回滚到 ${state?.version}`);
});

await updater.start();          // 读取快照并开始监听
// 命令：check / prepare / cancel / restart / skip（“稍后提醒”）/ clearSkip（回到更新）
// confirm 仅用于重启后的健康确认（默认 auto_confirm 已处理）
// 卸载时：unsubscribe(); updater.stop();
```

按钮的可用性也来自同一处：`view.canCheck` / `view.canPrepare` / `view.canRestart` / `view.canCancel` / `view.canSkip` / `view.isBusy`。React hook 返回的就是这些字段（同一份实现），Vue/Svelte 直接对 `describeUpdate(state)` 的结果做条件渲染，语义不会分叉。

React 18+ 可以直接用子路径 hook（`react` 是可选 peer 依赖）：

```tsx
import { useUpdater } from 'tauri-plugin-delta-update-api/react';

export function UpdateBanner() {
  const { state, isReady, restart } = useUpdater();
  if (!isReady) return null;
  return (
    <button onClick={() => void restart()}>立即重启并更新到 v{state?.version}</button>
  );
}
```

`confirm()` 只在重启后（`Committed`）调用，表示“应用已可用”；默认 `auto_confirm` 已自动处理，通常无需手动调用。多个组件共享同一个 controller 时，`start()`/`stop()` 按引用计数管理监听，最后一个组件卸载时才释放。

下面的原始事件接口仍然保留，适合需要完全自定义状态管理的场景。插件通过 Tauri 标准事件系统广播状态变化，事件名固定为 `delta-update://state`。

```typescript
import { listen } from '@tauri-apps/api/event';

interface UpdateState {
  v: number;
  state: 'Idle' | 'Checking' | 'Downloading' | 'Preparing'
       | 'Prepared' | 'Cancelled' | 'Committing' | 'Committed'
       | 'RollingBack' | 'RolledBack' | 'Failed';
  version: string | null;
  progress: { total: number; completed: number } | null;
  error: {
    code: string;
    category: string;
    message: string;
    retryable: boolean;
    action: string;
    user_action: string;
    detail: string;
  } | null;
  timestamp: string;
}

const unlisten = await listen<UpdateState>('delta-update://state', (event) => {
  const s = event.payload;

  switch (s.state) {
    case 'Prepared':
      // 关键时机：更新已就绪，下次启动生效
      showUpdateReadyBanner(s.version);
      break;

    case 'Failed':
      // 失败分类，按 category 决定是否提示用户
      if (s.error?.code === 'E_NETWORK_TIMEOUT') {
        // 静默失败即可，下次再试
      } else if (s.error?.code === 'E_PERMISSION_DENIED') {
        showErrorWithAction(s.error.user_action);
      }
      break;

    case 'RolledBack':
      // 新版本崩溃已自动回滚
      showToast(`已回滚到 ${s.version}，请稍后重试更新`);
      break;
  }
});
```

**启动快照**：`setup` 阶段（包括恢复出 `Prepared` 或 `Committed`）的事件可能早于页面加载完成。挂载 UI 时先调用一次 `status` 补齐当前状态，再依赖事件增量更新。`createUpdateController()` 已经按“先订阅、再取快照”的顺序处理，不会用旧快照覆盖刚到达的事件：

```typescript
import { invoke } from '@tauri-apps/api/core';

const snapshot = await invoke<UpdateState>('plugin:delta-update|status');
applyState(snapshot);
```

下载与准备期间 `version` 是本次的目标版本，`progress` 为累计进度，可直接显示“正在更新到 v{version}”。

### 5.2 调用健康确认（重要）

**`confirm_healthy()` 是回滚机制的核心开关**。它的语义是：“应用已经进入可用状态”。

```typescript
import { invoke } from '@tauri-apps/api/core';

// 在 UI 就绪后调用。推荐时机：主界面完成首屏渲染、核心数据加载完成之后。
async function confirmHealthy() {
  try {
    await invoke('plugin:delta-update|confirm_healthy');
  } catch (e) {
    console.warn('健康确认失败', e);
  }
}

// 在框架的 mount 完成、且核心初始化（登录/数据库/关键插件）之后调用
onMounted(async () => {
  await nextTick();
  await appInitialized();       // 你的应用自己的就绪信号
  await confirmHealthy();
});
```

**不要用固定延时**（例如 `setTimeout(confirmHealthy, 3000)`）代替健康判断：慢机器上会在初始化完成前确认，坏更新因此被误判为健康，崩溃循环保护失效。

**为什么不调用会出问题**：默认阈值 `3` 的判定是 `attempts > 3`——连续三次启动都未确认健康，**第四次启动时判定新版本失败并自动回滚**。**这是自愈机制，但对正常应用会造成误回滚**。正常关闭会折扣一次计数（`exit_grace_period_secs`，默认 10 秒），所以“打开看一眼就关掉”不会累计；只有崩溃或极短会话才计入。启动较慢的应用应提高 `crash_loop_threshold` 或调大 `confirm_delay_ms`。

**默认 `auto_confirm=false`**：插件不会替应用判断“已经健康”。你必须在 UI 就绪、核心数据加载完成后调用 `confirm_healthy()`；否则达到崩溃阈值会回滚。只有显式设置 `auto_confirm: true` 时，插件才会在主窗口页面加载后延迟 `confirm_delay_ms` 自动确认一次作为兜底。对启动较慢的应用，应提高 `crash_loop_threshold` 或调大 `confirm_delay_ms`。

### 5.3 手动检查与准备

```typescript
import { invoke } from '@tauri-apps/api/core';

interface CheckResult {
  available: boolean;
  version: string | null;
  current_version: string;
  files_changed: number;
  files_staged: number;
  download_bytes: number;
  total_bytes: number;
  notes: string | null;
  mandatory: boolean;
}

// 检查更新（不下载补丁）
async function checkUpdate(): Promise<CheckResult> {
  return await invoke('plugin:delta-update|check');
}

// 执行准备（下载 + 差分 + 暂存）。目标版本由最近一次 check 决定。
async function prepareUpdate() {
  return await invoke('plugin:delta-update|prepare');
}

// 取消准备（保留断点，下次可续传）
async function cancelPrepare() {
  return await invoke('plugin:delta-update|cancel_prepare');
}

// 立即重启安装（仅当状态为 Prepared）
async function restartToApply() {
  return await invoke('plugin:delta-update|restart_to_apply');
}

// 查询当前状态
async function getStatus() {
  return await invoke('plugin:delta-update|status');
}
```

`prepare()` 是耗时操作，应避免在主线程同步等待。推荐在后台调用并依赖 `state` 事件获取进度：

```typescript
async function startUpdate() {
  const result = await checkUpdate();
  if (!result.available) return;

  showProgressUI();
  await prepareUpdate();
  // prepare 返回成功时，状态已经是 Prepared
  // 用户下次退出应用时自动生效；也可调用 restartToApply() 立即重启
}
```

### 5.4 推荐的 UI 提示文案

| 状态               | 建议文案                                                    |
| ------------------ | ----------------------------------------------------------- |
| `Prepared`         | “新版本 v{version} 已准备好，下次启动时自动生效。”          |
| `Failed`（网络类） | 静默，不打扰用户。下次检查时自动重试。                      |
| `Failed`（权限类） | “更新需要一次管理员授权才能完成，请允许后重试。”            |
| `Failed`（磁盘类） | “磁盘空间不足，更新已暂停。清理空间后重试。”                |
| `RolledBack`       | “新版本启动异常，已自动回滚到 v{version}。我们会尽快修复。” |

**不要**：在 `Downloading` / `Preparing` 期间弹出阻塞式模态框。更新是后台行为，用户可以继续使用软件。


