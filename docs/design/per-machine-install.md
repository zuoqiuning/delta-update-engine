# 机器级安装：提权与目录边界

本页记录两个相邻问题的结论：启动恢复时的提权提示（已实现的最小版本），以及 `install_root` / `work_dir` 是否应该分离（RFC，未实现）。

## 1. 现状

- `install_root` 在 release 构建中**必须是 exe 所在目录**，`install_dir` 指向父目录会让启动失败（见 `lifecycle.rs`）。
- staging 与 journal 默认在 `app_local_data_dir()/delta-update`，可用 `staging_dir` / `journal_dir` 改到 `%ProgramData%`；这两个目录本来就是分离的。
- `Program Files` 默认 ACL 只允许管理员写入，因此向 `install_root` 落文件的每一步都需要管理员令牌：提交时的 worker、以及提交中断后的启动恢复。

两种安装形态因此有不同代价：

| 形态 | 提交时的提权 | 启动恢复的提权 | 谁写入 journal |
| --- | --- | --- | --- |
| 用户级（`%LocalAppData%`） | 不需要 | 不需要 | 当前用户 |
| 机器级（`Program Files`） | 需要（用户点“重启更新”时） | 需要（启动时无人预期） | 提权 worker |

## 2. 启动恢复的提权提示（B1，已实现）

场景：提交后应用崩溃，下一次启动必须先重放 journal 才能进入正常状态；当前用户令牌无法写 `install_root`，于是 `recover_at_startup` 走 `needs_elevation`（真实写探针）→ `run_elevated_recovery` → `ShellExecuteExW("runas")`。

这一路径的难点不是提权本身，而是**用户没有预期**：应用刚打开就弹出系统 UAC，标题里只有 exe 名字。因此在 UAC 之前加一次说明：

- 弹窗在 `setup` 阶段、窗口创建之前，用 `MessageBoxW(null, text, "更新未完成", MB_YESNO | MB_ICONWARNING | MB_SETFOREGROUND)`。
- **为什么是 `MessageBoxW`**：单次调用、无额外依赖，`windows-sys` 的 `Win32_UI_WindowsAndMessaging` 已经在依赖里；`TaskDialogIndirect` 需要 comctl32 v6 manifest 与回调结构体，而这里只需要“是/否”。也不使用 Tauri 窗口：`setup` 时窗口还不存在，而恢复必须先于 GUI。
- **阻塞语义**：`setup` 在主线程，`MessageBoxW` 是模态阻塞，正好满足“先修复、再显示界面”的顺序；不会出现界面已经可见、文件随后被替换的窗口。
- **“否”的语义**：返回 `EngineError::UacCancelled`（`E_PERMISSION_UAC_CANCELLED`，`action = grant_permission`），状态为 `Failed`，`user_action` 提示“下次启动将重试”。不返回 `Idle`——否则用户会以为一切正常，下次启动又弹一次，形成循环困惑。
- **与 UAC 取消一致**：用户关掉系统 UAC 时 `ShellExecuteExW` 返回 `ERROR_CANCELLED`，同样映射到 `UacCancelled`。两条路径产生的状态与错误码完全相同，客户端只需要处理一个分支。
- **不自动改 ACL**：提权只是执行一次恢复，不改变目录权限。手动修复（以管理员身份运行一次、或由部署者修正 ACL）留给 1.x 的工具化，不在 rc 阶段扩大写权限。

无法自愈时（用户连续拒绝、或提权 worker 超时），journal 保持原样，`inspect` 会显示未完成的 ops，下一次启动继续尝试。

## 3. `install_root` / `work_dir` 分离（B3，RFC）

**需求**：企业部署希望 journal 与 staging 在 `%ProgramData%`（可审计、可备份、随机器清理），而不是每个用户的 `%LocalAppData%`。

**结论先行**：这个需求**不需要改 `install_root`**。`staging_dir` / `journal_dir` 已经可以把工作目录放到 `%ProgramData%`。真正待定的是另一个问题：**是否允许 `install_root` 是 exe 的父目录**（例如 `Program Files\Vendor` 下放多个子应用）。

如果允许，提权 worker 的写范围会从“一个应用目录”扩大到“父目录整棵树”，一次签名清单就能覆盖父目录下任意文件。这不是格式问题，是信任边界问题。

| 选项 | 做法 | 代价 | 判断 |
| --- | --- | --- | --- |
| A（推荐） | 保持 `install_root == exe 目录` | 多应用需要各自目录 | 写范围最小，worker 无法越界 |
| B | 允许 exe 的祖先目录，并加**清单之外的写白名单**（只允许 manifest 声明的路径、`.old/`、`.trash/`、`update.lock`） | 需要新的校验层与拒绝路径的测试；worker 的提权范围仍然变大 | 1.x 可考虑，但必须先有白名单，不能只放宽配置 |
| C | 允许 `install_dir` 为任意目录（旧行为） | 提权 worker 变成任意目录写入器 | 拒绝 |

**若做 B，前置条件**：

1. manifest 里的每个路径都必须是 install_root 的相对路径，且解析后仍在 install_root 内（已有校验，需要在新的白名单层复用）。
2. worker 只能写入清单文件、`.old/<commit_id>/`、`.trash/`、`update.lock` 与 journal 目录；其他路径一律拒绝并记录。
3. 启动恢复同样受限：重放 journal 时按白名单校验，而不是信任 journal 里的路径。
4. 需要一条测试：构造含 `..`／绝对路径／跨盘符的清单与 journal，确认都被拒绝。

**暂不实现的原因**：1.0 的承诺是“worker 只改它自己所在的那个目录”，这条承诺让提权风险可被人肉审计；放宽它属于架构决策，应在 1.x 有真实企业需求时连同白名单一起设计。

## 4. 部署者的责任

- 把 `journal_dir` / `staging_dir` 指到 `%ProgramData%` 时，需要给普通用户写权限；否则 `prepare` 会因权限失败，或者每一步都要提权。
- 机器级安装下，用户拒绝 UAC 是正常操作；应用应把它当作“更新未完成”而不是崩溃，并允许稍后重试。
- 不要通过 `socat` / 端口转发把 `serve` 暴露到公网，也不要把签名私钥放在安装目录。

## 5. 最小配置示例

### 5.1 NSIS per-machine

```json
{
  "bundle": {
    "windows": {
      "nsis": {
        "installMode": "perMachine"
      }
    }
  }
}
```

安装后 `install_root` 是 `%ProgramFiles%\<ProductName>`，普通用户只读。用户在“重启更新”时点击一次 UAC，插件用 `ShellExecuteExW(..., "runas", ...)` 启动同一个 exe 的 `--delta-update-apply`；提交中断后的启动恢复走 §2 的同一条路径。`install_dir` 不要指向父目录：release 构建要求它等于 exe 所在目录，否则 `E_CONFIG_INVALID`。

`both` 形态下 NSIS 会按用户选择落两个目录，而插件只更新**运行中 exe 所在**的那一份，所以 `doctor --check-config` 会对 `perMachine`/`both` 搭配 `auto_apply: true` 给出告警：这会在用户没有预期时触发 UAC。

### 5.2 计划任务（未实现）

把提权 worker 改成一次性计划任务（`schtasks /create ... /rl highest`）可以避免启动时 UAC，但它把“一次点击授权”变成常驻的高权限入口，还需要额外的创建、清理与防篡改设计。1.0 不做，原因与 §3 的信任边界相同。
