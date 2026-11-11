# Minimal Delta App

最小接入示例：注册插件、填写三项配置、授予默认权限。

在**本目录内**执行：

```bash
pnpm install
pnpm tauri dev
```

`src-tauri/tauri.conf.json` 里的 `endpoint` 与 `signing_public_keys` 是占位值，启动后检查会失败（状态 `Failed`），不影响界面与命令演示。接入真实发布通道时替换为真实值。

本地开发请保持 `auto_apply: false`，或把 `install_dir` 指向临时测试目录；否则已准备的更新会在退出时替换 `target/debug` 下的开发二进制（见[接入指南](../../docs/guides/getting-started.md) §3.4）。示例只带一个最小图标，不包含代码签名配置。
