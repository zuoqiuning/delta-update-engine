# Delta Update Engine 协议与数据格式规范

> 本文是三端（engine / plugin / cli）实现共同遵守的唯一协议契约。任何字段、格式、语义的变更都必须先更新本文档，再改代码。文档本身按 `schema` 版本号管理，与清单 `schema` 字段同步演进。
>
> 标识约定：仓库与核心 crate 同名 `delta-update-engine`，插件标识固定为 `delta-update`，配置键为 `plugins.delta-update`，默认权限集为 `delta-update:manage-confirm`，事件名为 `delta-update://state`。Tauri 的 identifier 采用短名，与项目名刻意不同，避免权限字符串里出现 `updater:default` 这种容易被误读为官方 updater 的形态。

## 读哪一份

| 你要查的内容 | 文件 |
| --- | --- |
| 通用约定、Manifest、签名、补丁容器、Journal | [protocol-data.md](protocol-data.md) |
| 状态机、事件 payload、错误对象、命令语义 | [protocol-runtime.md](protocol-runtime.md) |
| 插件配置 schema、崩溃循环、更新锁、签名服务、schema 演进、示例 | [protocol-config.md](protocol-config.md) |

协议变更的顺序是固定的：先改本目录下的对应分册，再改三端实现，最后让 `cargo xtask contract-check` 与契约测试通过。只改代码不改协议文档的 PR 不予合并。
