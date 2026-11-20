# Minimal Delta App

最小接入示例：注册插件、填写三项配置、授予默认权限。

在**本目录内**执行：

```bash
pnpm install
pnpm tauri dev
```

`src-tauri/tauri.conf.json` 里的 `endpoint` 与 `signing_public_keys` 是占位值，启动后检查会失败（状态 `Failed`），不影响界面与命令演示。接入真实发布通道时替换为真实值。

本地开发请保持 `auto_apply: false`，或把 `install_dir` 指向临时测试目录；否则已准备的更新会在退出时替换 `target/debug` 下的开发二进制（见[接入指南](../../docs/guides/getting-started.md) §3.4）。示例只带一个最小图标，不包含代码签名配置。

## 本地跑通完整链路

1. `minisign -G -p release.pub -s release.key`
2. 准备两版产物目录（如 `dist/v1`、`dist/v2`，各含应用可执行文件）
3. `delta-update release --to dist/v2 --from dist/v1 --version 0.2.0 --from-version 0.1.0 --bucket ./channel --key release.key`
4. `delta-update serve --dir channel --port 8000`（无需 Python；启动时会打印可粘贴的 `endpoint`），把 `endpoint` 改为打印出的 `http://127.0.0.1:8000/manifests/latest.json`，并加 `"allow_insecure_http": true`
5. 按接入指南 §3.4 用临时 `install_dir` 放入 v1 产物，退出应用后确认文件已被替换

`allow_insecure_http` 只放行 loopback，仅用于本地调试，不要随真实通道提交。
