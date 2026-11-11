# Delta Update Engine 开发者接入指南

> 本文面向使用 Delta Update Engine（`tauri-plugin-delta-update`）的 Tauri 应用开发者。目标：从零到跑通完整更新链路，理解两种工作模式，掌握前端集成方式，并能在出现问题时快速定位。本文不解释引擎内部原理——需要理解设计时请参阅架构文档和功能机制文档。


## 1. 你将获得什么

接入完成后，你的 Tauri 应用将具备以下行为：

- **启动时静默检查**更新，发现新版本后在后台下载并生成补丁。
- **全程不打断用户**：下载、差分、暂存都在后台完成，软件照常使用。
- **下次启动即为新版本**：无需运行安装器，无需用户点击“安装”。
- **更新失败时自动回退**：如果新版本启动即崩溃，自动回滚到上一个可用版本。
- **细粒度差分**：只下载变化的文件，已压缩/未变化的文件零传输。

你需要做的只有三步：注册插件、写三项配置、授予默认权限。


## 2. 前置条件

### 2.1 环境要求

| 项       | 要求                                                                 |
| -------- | -------------------------------------------------------------------- |
| Tauri    | 2.x                                                                  |
| Rust     | stable（与 Tauri 2 要求一致）                                        |
| 平台     | Windows 10+ x64；Linux AppImage/tar；macOS 仅未签名开发包             |
| 打包格式 | Windows：NSIS / MSI / 绿色版；Linux：AppImage；macOS：不签名的 .app |

**不支持的形态会在注册期或下载前直接拒绝**：Linux `deb`/`rpm`、Flatpak/Snap、macOS 签名/公证 `.app` 返回 `E_UNSUPPORTED_FORMAT`，移动端返回 `E_UNSUPPORTED_PLATFORM`。原因见 API 文档平台矩阵——签名 `.app` 内部文件被替换后代码签名会失效，`deb`/`rpm` 的目录属于包管理器。

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

**③ 一个构建侧 CLI**

`delta-update-cli` 在你的 CI 中运行，负责扫描产物、生成清单、生成补丁、签名、上传。没有它，增量更新无法自动化。


## 3. 三步接入

### 3.1 第一步：添加依赖并注册插件

**Cargo.toml**

```toml
[dependencies]
tauri-plugin-delta-update = "0.1"
```

**src-tauri/src/main.rs（或 lib.rs）**

```rust
fn main() {
    // 替换进程入口：必须在 Tauri 启动之前，worker 模式不加载 WebView。
    if let Some(code) = tauri_plugin_delta_update::run_applier_from_args() {
        std::process::exit(code);
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_delta_update::init())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

`run_applier_from_args()` 在进程以 `--delta-update-apply` 或 `--delta-update-recover` 启动时返回 `Some`：提交 worker 会等待主进程退出、读取 Journal 并完成文件替换；启动期恢复 worker 会直接重放/回滚并退出。缺少这段入口时，插件 setup 会直接返回 `E_CONFIG_INVALID` 并提示“worker mode was not handled”，而不是静默开第二个实例。除此之外与普通 Tauri 插件一致，**没有额外的 builder 链，没有必须实现的自定义 trait**。

### 3.2 第二步：写配置

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
      "auto_apply": true,
      "auto_confirm": true,
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
| `auto_check`          | 否   | `true`（默认）：setup 后自动后台准备；`false`：仅暴露命令    |
| `channel`             | 否   | 默认 `stable`，与清单的 `channel` 比对                       |

**获取公钥字符串**：打开 `release-1.pub`，去掉注释行，取 base64 主体部分（通常以 `RWQ` 开头的一整行）。

也可以让 CLI 生成可直接合并的配置片段：

```bash
delta-update init \
  --endpoint https://your-bucket.cos.ap-guangzhou.myqcloud.com/manifests/latest.json \
  --public-key RWQ...release-1... \
  --public-key RWQ...release-2...
```

它会打印 `plugins.delta-update` 配置、`capabilities/default.json` 权限片段与接入检查清单。`--dir` 默认当前目录：如果存在 `src-tauri/tauri.conf.json`，会优先复用已有的 `plugins.delta-update` 配置；从官方 updater 迁移时，也会把 `plugins.updater.pubkey` 作为默认公钥（endpoint 仍需显式提供）。

### 3.3 第三步：授予默认权限

**src-tauri/capabilities/default.json**

```json
{
  "identifier": "default",
  "windows": ["main"],
  "permissions": [
    "core:default",
    "delta-update:default"
  ]
}
```

`delta-update:default` 包含六个命令的允许权限：`check`、`prepare`、`cancel_prepare`、`status`、`confirm_healthy`、`restart_to_apply`。

**完成。** 如果 `auto_check` 与 `auto_apply` 都为 `true` 且你没有额外的 UI 需求，此时已经获得完整的“后台自动准备、退出时自动应用、下次启动即新版本”体验。下面章节解释如何自定义 UI、监听事件和排查问题。

### 3.4 本地开发注意

`install_dir` 默认取**当前可执行文件所在目录**。`tauri dev` / `cargo run` 下它是 `target/debug`；如果配置指向真实发布通道且远端版本更高，`auto_check` + `auto_apply` 会在退出时用发布版替换开发二进制。

本地开发建议：

- 把 `auto_apply` 设为 `false`（或 `auto_check: false`），只调试命令与事件；
- 需要验证完整“下载 → 退出替换 → 启动新版本”链路时，显式设置 `install_dir` 为一个临时测试目录，并把旧版本复制进去；
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

**推荐先看 `createUpdateController`**。它把“启动快照 + 事件订阅 + 取消订阅 + 六个命令”收进一个框架无关的小控制器，React/Vue/Svelte 都无需自己维护监听生命周期：

```typescript
import { createUpdateController } from 'tauri-plugin-delta-update-api';

const updater = createUpdateController();
const unsubscribe = updater.subscribe((state) => {
  if (state?.state === 'Prepared') showUpdateReadyBanner(state.version);
  if (state?.state === 'RolledBack') showToast(`已回滚到 ${state.version}`);
});

await updater.start();          // 读取快照并开始监听
// 命令：check / prepare / cancel / restart
// confirm 仅用于重启后的健康确认（默认 auto_confirm 已处理）
// 卸载时：unsubscribe(); updater.stop();
```

React 18+ 可以直接用子路径 hook（`react` 是可选 peer 依赖）：

```tsx
import { useUpdater } from 'tauri-plugin-delta-update-api/react';

export function UpdateBanner() {
  const { state, restart } = useUpdater();
  if (state?.state === 'Prepared') {
    return (
      <button onClick={() => void restart()}>立即重启并更新到 v{state.version}</button>
    );
  }
  return null;
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

// 方式一：在框架的 mount 完成后调用
onMounted(async () => {
  await nextTick();
  await confirmHealthy();
});

// 方式二：延迟调用，给应用留出“稳定运行”的观察窗口
setTimeout(confirmHealthy, 3000);
```

**为什么不调用会出问题**：默认阈值 `2` 的判定是 `attempts > 2`——连续两次启动都未确认健康，**第三次启动时判定新版本失败并自动回滚**。**这是自愈机制，但对正常应用会造成误回滚**。启动较慢的应用应提高 `crash_loop_threshold` 或调大 `confirm_delay_ms`。

**如果使用 `auto_confirm: true`（默认）且不写任何前端代码**，插件层会在主窗口页面加载后延迟 `confirm_delay_ms` 自动调用一次 `confirm_healthy` 作为兜底。但如果你希望“应用真正可用”才确认，**请显式调用并覆盖默认行为**。

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
│   │   └── default.json          # 授予 delta-update:default
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
| 权限未授予              | 检查 `capabilities/default.json` 是否包含 `delta-update:default` |
| 看不到任何插件日志      | 插件只发出 `tracing` 事件，宿主未安装 subscriber 时不会落盘；见[故障排查 §1.1](../operations/troubleshooting.md) |

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
| 安装目录定位错误            | 检查注册表 `InstallLocation` 是否指向正确路径                |
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


## 9. 进阶配置（通常不需要）

以下配置有安全默认值，只在特殊场景下需要显式设置：

```json
{
  "plugins": {
    "delta-update": {
      "crash_loop_threshold": 2,
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
| `crash_loop_threshold`     | `2`                                          | 应用启动较慢或初始化复杂时，可提高到 3 |
| `confirm_delay_ms`         | `3000`                                       | 启动慢、首屏晚时调大；0 表示不自动确认 |
| `staging_dir`              | `app_local_data_dir()/delta-update/staging/` | 磁盘空间紧张且想放到其他盘（可跨卷）   |
| `journal_dir`              | `app_local_data_dir()/delta-update/journal/` | 同上                                   |
| `install_dir`              | 自动解析                                     | 默认为当前可执行文件所在目录（开发时是 `target/debug`），便携版等解析失败时显式指定 |
| `max_concurrent_downloads` | `4`                                          | 网络环境差时降低，强网可提高           |
| `staging_size_limit`       | `2 GiB`                                      | 磁盘空间严格受限时降低                 |
| `max_patch_file_size`      | `256 MiB`                                    | 有超大 sidecar 且希望差分时提高        |
| `patch_memory_factor`      | `2.5`                                        | 内存紧张时降低，但过低保补丁失败率上升 |
| `manifest_max_bytes`       | `8 MiB`                                      | 文件数极多的大型应用可适当提高         |
| `max_files`                | `20000`                                      | 同上                                   |
| `signing_endpoint`         | 空                                           | 私有 COS 桶需要预签名服务时设置        |
| `allowed_hosts`            | 空                                           | 需要限制内容下载主机时设置             |

**注意**：`staging_dir` 和 `journal_dir` 必须在用户有写权限的位置，且**不能在安装根目录之内**。可以放到其他盘；跨卷时替换进程会自动改用“复制到目标目录临时文件 + 原子 rename”，不会破坏原子性，但会多一次复制开销。


## 10. 接入验收清单

接入完成后，逐项确认：

- [ ] `tauri.conf.json` 中 `endpoint` 可访问，返回合法清单 JSON
- [ ] `signing_public_keys` 至少一把，且与签名私钥配对
- [ ] `capabilities/default.json` 包含 `delta-update:default`
- [ ] 前端在 UI 就绪后调用了 `confirm_healthy()`（或接受插件默认行为）
- [ ] 监听 `delta-update://state` 事件并展示 `Prepared` 状态提示
- [ ] CI 中 `delta-update release` 能完整执行并上传到 COS
- [ ] 测试机安装旧版本 → 发布新版本 → 观察 `state = Prepared` → 退出再启动 → 版本更新
- [ ] 测试机新版本启动崩溃 → 连续两次未确认 → 第三次启动自动回滚
- [ ] 安装在 `Program Files` 时，更新流程能正确处理 UAC
- [ ] 安装在 `Program Files` 且提交被中断后，下次启动能提权恢复（取消则保留 Journal）
- [ ] 断网后恢复网络，更新能续传并完成


## 11. 版本兼容性承诺

| 插件版本 | 支持的 Tauri 版本 | 文件级替换支持的形态                              | 协议 schema |
| -------- | ----------------- | ------------------------------------------------- | ----------- |
| 0.1.x    | 2.x               | Windows 10+ x64、Linux AppImage/tar、macOS 未签名包 | 1           |

**插件版本语义化**：

- **Patch**（0.1.0 → 0.1.1）：Bug 修复，命令和配置不变。
- **Minor**（0.1.x → 0.2.0）：新增命令或配置字段，向后兼容。旧配置无需修改。
- **Major**（0.x → 1.0）：可能包含命令或配置的破坏性变更，会提供迁移指南。

**协议 schema 独立于插件版本**。清单 schema 演进规则见协议与数据格式规范。插件会拒绝 `min_client_schema` 高于自身支持值的清单，并给出明确错误。


## 12. 接入总结

接入体验的核心判断是：**开发者不需要理解增量更新引擎的内部机制，只需要做三个正确的决定**——配置正确的 `endpoint` 和公钥、授予正确的权限、在合适的时机调用 `confirm_healthy()`。其余全部由插件在后台完成。

如果接入过程中遇到本文未覆盖的问题，请优先收集 §8.7 列出的诊断信息，再对照错误码表定位类别。错误码是稳定的——它们不会在不同版本间改变含义，可以安全地作为程序分支的依据。