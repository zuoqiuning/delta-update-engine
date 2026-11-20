# Delta Update Engine 协议与数据格式规范

> 本文是三端（engine / plugin / cli）实现共同遵守的唯一协议契约。任何字段、格式、语义的变更都必须先更新本文档，再改代码。文档本身按 `schema` 版本号管理，与清单 `schema` 字段同步演进。
>
> 标识约定：仓库与核心 crate 同名 `delta-update-engine`，插件标识固定为 `delta-update`，配置键为 `plugins.delta-update`，默认权限集为 `delta-update:manage-confirm`，事件名为 `delta-update://state`。Tauri 的 identifier 采用短名，与项目名刻意不同，避免权限字符串里出现 `updater:default` 这种容易被误读为官方 updater 的形态。


## 1. 通用约定

### 1.1 版本化原则

| 对象            | 版本字段         | 当前版本 | 演进规则                                                     |
| --------------- | ---------------- | -------- | ------------------------------------------------------------ |
| Manifest        | `schema`         | `1`      | 新增可选字段不升版本；删除/改义必升版本                      |
| Journal         | `format`         | `1`      | 任何格式变更必须升版本，旧版本 journal 只读不可重放          |
| 补丁容器        | `format`         | `1`      | 任何格式变更必须升版本，未知格式回退全量                     |
| 事件 payload    | `v`              | `1`      | 新增字段不升版本；删除/改义必升版本                          |
| 错误码          | 码值本身         | —        | 已发布码值永不重用、永不改义                                 |
| 插件配置        | `config_version` | `1`      | 新增可选字段不升版本；必填字段变更升版本                     |

**版本号比较**：清单 `version` 与 CLI 的版本参数一律按 **SemVer 2.0.0** 比较——先比较主/次/修订号，预发布版本低于同号正式版（`2.0.0-rc.1 < 2.0.0`），构建元数据（`+build.1`）不参与比较。**禁止用字符串字典序比较版本**。`schema` / `format` / `config_version` 是单调递增的整数，按数值比较。

**客户端行为**：遇到高于自身支持版本的 `schema` / `format` / `config_version`，**拒绝并返回明确错误**，不尝试“尽力解析”。

### 1.2 编码与序列化

- 所有结构化数据使用 **UTF-8 JSON**，禁止 BOM。
- 键名使用 `snake_case`。
- 数值不使用科学计数法；大小字段为无符号整数，单位字节。
- 哈希统一为**小写十六进制字符串**，不带 `0x` 前缀。
- 时间统一为 **RFC 3339 UTC**（如 `2026-11-10T08:00:00Z`），仅用于诊断，不参与任何决策逻辑。

### 1.3 路径规范

- 清单中的文件路径为**相对路径**，使用 `/` 分隔，**不以 `/` 开头**。
- 路径大小写敏感性与文件系统一致；Windows 下比对时**规范化到小写**，但实际文件操作保留原始大小写。
- 禁止出现 `..`、`.`、空段、绝对路径、盘符、UNC 前缀、结尾 `/`。
- 单个路径总长度 ≤ **1024 字节**、段数 ≤ **64**；超过返回 `E_PATH_INVALID`。
- 禁止段尾的空格与句点（Windows 会静默去掉，导致清单与实际不一致）。
- 禁止 Windows 非法字符 `<`、`>`、`:`、`"`、`|`、`?`、`*` 与控制字符；盘符（如 `C:/x`）因此也会被拒绝。
- 禁止 Windows 保留设备名及其带扩展名形式：`CON`、`PRN`、`AUX`、`NUL`、`COM1`–`COM9`、`LPT1`–`LPT9`。
- **大小写冲突**：同一清单中两个路径按 ASCII 小写规范化后相同（如 `App.exe` 与 `app.exe`）时，整份清单拒绝，返回 `E_PATH_COLLISION`。该检查不区分目标平台，保证同一清单可跨平台复用。
- **保留目录**：首段为 `.old` 或 `.trash`（大小写不敏感）的安装路径一律拒绝，避免覆盖替换引擎的备份/回收目录。
- 违反路径规范的清单一律拒绝：结构违规返回 `E_PATH_INVALID`，规范化后逃逸根目录返回 `E_PATH_ESCAPE`。

### 1.4 对象键（COS Key）规范

```
manifests/latest.json
manifests/latest.json.minisig
manifests/<version>.json
manifests/<version>.json.minisig
blobs/<sha256>
artifacts/<from>-to-<to>/<relative_path>.tdp
```

`blobs/<sha256>` 是内容寻址的全量回退对象：每个文件条目都必须提供 `full_key`，包括 `Unchanged` 条目。这样任意本地文件损坏、跳过版本升级或回滚都能按目标哈希取回完整内容；相同内容只存一份。

`<from>`、`<to>`、`<version>` 使用语义化版本字符串，构建元数据（`+` 之后）在对象键中省略。

**签名对象定位**：签名对象与清单**同目录、固定后缀** `sig_key = manifest_key + ".minisig"`。客户端在解析清单之前就能确定签名 URL：

1. 配置了 `signing_endpoint`：向签名服务请求该 key 的下载 URL。
2. 未配置：以 `endpoint` 的**源站**（scheme + host）为基准拼接。例如 `endpoint = https://h/manifests/latest.json`，签名 URL 为 `https://h/manifests/latest.json.minisig`。

**为什么不在清单里放 `signature_ref`**：那会造成“先解析清单才知道签名在哪、但解析前必须先验签”的循环依赖。固定后缀让客户端**在解析任何未受信字节之前**就能拿到签名并完成验签。

**预签名 endpoint 的特殊规则**：如果 `endpoint` 本身带查询参数（预签名 URL），客户端无法从它推导出可下载的签名 URL。此时必须配置 `signing_endpoint`；否则在启动校验阶段返回 `E_CONFIG_INVALID`，而不是等到下载时才失败。



### 1.5 资源上限

所有上限都是**签名清单之外的安全边界**，用于抵御恶意或被劫持的服务端。超限一律拒绝，不做“尽力处理”。

| 对象                     | 上限                                    | 超限错误                       |
| ------------------------ | --------------------------------------- | ------------------------------ |
| 清单原始字节             | 8 MiB                                   | `E_MANIFEST_TOO_LARGE`         |
| 清单文件条目数           | 20 000                                  | `E_MANIFEST_TOO_LARGE`         |
| 清单发布说明             | 64 KiB                                  | `E_MANIFEST_PARSE`             |
| 签名文件                 | 64 KiB                                  | `E_SIGNATURE_INVALID`          |
| 单个补丁对象             | 1 GiB，且不得超过 `delta_size`          | `E_PATCH_TOO_LARGE`            |
| 单个全量对象             | 4 GiB，且不得超过清单 `size`            | `E_FULL_TOO_LARGE`             |
| 补丁解压输出             | 清单 `size`，超出 1 字节即中止          | `E_PATCH_DECOMPRESSION_LIMIT`  |
| Journal 文件             | 32 MiB / 100 000 个操作                 | `E_JOURNAL_CORRUPT`            |
| 单文件路径               | 1024 字节 / 64 段                       | `E_PATH_INVALID`               |

**为什么把上限放在协议层**：清单虽然经过签名，但签名只证明“发布者写了这些字段”，不证明“发布者不会误写一个 200 万条目的清单”。上限保护的是客户端自身的内存与磁盘，不依赖发布端的正确性。


## 2. Manifest 协议（`latest.json`）

### 2.1 顶层结构

```json
{
  "schema": 1,
  "version": "2.1.0",
  "channel": "stable",
  "published_at": "2026-11-10T08:00:00Z",
  "min_client_schema": 1,
  "notes": "修复了若干问题",
  "mandatory": true,
  "min_supported_version": "1.9.0",
  "signing_key_ids": ["a1b2c3d4e5f60718"],
  "files": { }
}
```

| 字段                    | 类型   | 必填 | 语义                                                    |
| ----------------------- | ------ | ---- | ------------------------------------------------------- |
| `schema`                | u32    | 是   | 清单格式版本。当前为 `1`。                              |
| `version`               | string | 是   | 语义化版本（SemVer 2.0.0）。与本地版本的关系见 §3.4。   |
| `channel`               | string | 否   | 更新通道，默认 `stable`。客户端按配置的通道过滤。       |
| `published_at`          | string | 否   | 发布时间，仅诊断用。                                    |
| `min_client_schema`     | u32    | 是   | 客户端最低支持 schema。低于此值的客户端拒绝使用本清单。 |
| `notes`                 | string | 否   | 发布说明，UTF-8，最大 64 KiB；仅用于展示。               |
| `mandatory`             | bool   | 否   | 强制更新标记，默认 `false`；是否阻止跳过由应用 UI 决定。 |
| `min_supported_version` | string | 否   | 发布者仍支持的最旧版本（SemVer，且 `<= version`）。低于它的客户端在 `check` 中收到 `update_required: true`，由应用决定是否阻止使用。 |
| `files`                 | object | 是   | 文件条目映射，键为相对路径。                            |
| `rollback`              | bool   | 否   | 显式降级标记，默认 `false`。语义见 §3.4。               |
| `signing_key_ids`       | array  | 否   | 附加签名的 key id 列表（每个 16 位小写十六进制），最多 8 个；文件为 `<清单>.minisig.<key_id>`。旧客户端忽略该字段。见 §3.1。 |

**清单中不存在的字段**：客户端一律忽略。**清单中不允许出现 `key_rotation` 一类运行期扩展信任集的字段**——信任锚只能随二进制更新，理由见 §3.3。

### 2.2 文件条目（`files` 的值）

```json
{
  "sha256": "a3f1…",
  "size": 10240,
  "strategy": "qbsdiff+zstd",
  "delta_key": "artifacts/2.0.0-to-2.1.0/App.exe.tdp",
  "delta_size": 1832,
  "full_key": "blobs/<sha256>",
  "executable": true
}
```

| 字段         | 类型       | 必填 | 语义                                                         |
| ------------ | ---------- | ---- | ------------------------------------------------------------ |
| `sha256`     | string(64) | 是   | 目标文件完整 SHA-256。补丁应用后必须校验到此值。             |
| `size`       | u64        | 是   | 目标文件字节数。用于磁盘空间预检与下载校验。                 |
| `strategy`   | string     | 是   | 补丁策略标识。取值见 2.3。无补丁时为 `"full"`。              |
| `delta_key`  | string     | 否   | 补丁对象键。`strategy == "full"` 时必须缺省。                |
| `delta_size` | u64        | 条件 | 补丁对象字节数。**存在 `delta_key` 时必填**，用于大小上限校验、进度与并发调度。 |
| `full_key`   | string     | 条件 | 全量对象键。**`Added`/`Modified` 必填；发布 CLI 对所有条目（含 `Unchanged`）都生成 `blobs/<sha256>`，用于跨版本与损坏修复。** |
| `executable` | bool       | 否   | 该文件是否为 PE/ELF 可执行体，供审计与诊断用。默认为 `false`。 |

**为什么 `full_key` 是条件必填**：它是所有变化文件与本地损坏文件的最终修复来源。**发布 CLI 会为每个条目（包括 `Unchanged`）生成 `full_key`，指向内容寻址的 `blobs/<sha256>`**。客户端对 `Added`/`Modified` 或本地哈希不匹配的条目要求 `full_key`；缺失时返回 `E_MANIFEST_PARSE`。旧版发布端如果只为变化文件生成 full_key，只能支持相邻版本且本地文件树完全健康；不建议继续这样做。

### 2.3 策略标识（`strategy`）取值

| 值               | 含义                                      | 运行侧行为                         |
| ---------------- | ----------------------------------------- | ---------------------------------- |
| `full`           | 不生成差分，直接下载 `full_key`           | 直接下载全量                       |
| `qbsdiff+zstd`   | bsdiff 差分 + zstd 压缩（默认管线）       | 解压后调用 bsdiff apply            |
| `qbsdiff+lzma2`  | bsdiff 差分 + lzma2 压缩                  | 解压后调用 bsdiff apply            |
| `hdiff+zstd`     | HDiffPatch 差分 + zstd 压缩               | 解压后调用 HDiffPatch apply        |
| `hdiff+lzma2`    | HDiffPatch 差分 + lzma2 压缩              | 解压后调用 HDiffPatch apply        |
| `zucchini+zstd`  | Zucchini 归一化 + bsdiff/HDiff + zstd     | 解压后调用 Zucchini apply 流程     |
| `zstd-dict`      | 以旧文件为字典的 zstd 压缩                | 调用 zstd 字典解码                 |

**当前客户端实现**：只支持 `full` 与 `qbsdiff+zstd`。`qbsdiff+lzma2`、`hdiff+*`、`zucchini+zstd`、`zstd-dict` 是保留标识，客户端不会 apply；发布端不得在正式清单中使用它们。未知策略仍可解析（不因未知值让整份清单失败），但运行时回退全量。

**客户端契约**：`strategy` 在协议层是**字符串**，不是封闭枚举。客户端可以把它解析为带 `Unknown(String)` 变体的内部类型，但**不得**因未知值导致整份清单解析失败。

`strategy` 取值**大小写敏感**，清单必须使用 §2.3 表中的小写形式（例如 `qbsdiff+zstd`）；`QBSDIFF+ZSTD`、`qbsdiff-zstd` 等变体会被当作未知策略处理并回退全量，客户端会记录警告但不会猜测发布端意图。

- 未知 `strategy` + 提供 `full_key` → 回退全量，记录诊断事件。
- 未知 `strategy` + 无 `full_key` → 该条目在差异计算时是 `Modified`/`Added`，客户端返回 `E_MANIFEST_PARSE`（缺少全量回退），不会静默跳过。
- **不尝试猜测策略**，也不按字符串前缀匹配。

### 2.4 差异分类规则（协议层定义，非实现细节）

| 本地状态       | 远程条目                         | 分类        | 行为                              |
| -------------- | -------------------------------- | ----------- | --------------------------------- |
| 存在，哈希一致 | 存在                             | `Unchanged` | 不下载、不替换；`strategy`/`delta_key` 被忽略 |
| 存在，哈希不同 | 存在                             | `Modified`  | 有 `delta_key` 走差分，否则走全量 |
| 不存在         | 存在                             | `Added`     | **直接走全量**，不进入差分管线    |
| 存在           | 不存在，且曾出现在任一历史清单   | `Removed`   | 移入 `.trash/`，不物理删除        |
| 存在           | 不存在，且从未出现在任何历史清单 | `Untracked` | **不处理**（保护用户数据）        |

`Untracked` 的判定依赖客户端维护的“历史官方文件集合”，该集合持久化在应用数据目录，首次迁移时为空。


## 3. 签名协议（`.minisig`）

### 3.1 签名对象

签名对象为 **minisign detached signature**，覆盖的是**原始清单字节**，而非解析后的 JSON。

每份清单有一个主签名 `<清单>.minisig`，以及零到多个附加签名 `<清单>.minisig.<key_id>`。附加签名在清单的 `signing_key_ids` 中声明；旧客户端只读主签名，因此主签名必须始终由当前所有受支持客户端都信任的密钥签发。多签名的动机与兼容矩阵见 [RFC 0001](../rfcs/0001-multi-signature-rotation.md)。

### 3.2 验签输入定义

```
signed_payload = raw_bytes_of(latest.json)
```

客户端必须：
1. 先下载 `.minisig` 与 `latest.json` 原始字节。
2. 用内嵌受信公钥集合中的**任意一把**验证主签名。
3. 主签名失败时，按 `signing_key_ids` 依次拉取 `.minisig.<key_id>`，**任一通过即接受**。
4. **验证通过后**才解析 JSON 用于更新决策。

`signing_key_ids` 在验签通过前来自不可信字节，只用于构造同目录的兄弟签名 URL：客户端只接受 16 位小写十六进制 id、最多 8 个，文件缺失或无效时忽略该条。清单从未通过验签时，绝不把它的其他字段用于更新决策。

**顺序不可颠倒**：先验签后解析，防止解析器差异引入的攻击面。

### 3.2.1 单对象信封（推荐路径）

`manifests/latest.envelope.json` 把清单与主签名放进同一个对象：

```json
{ "schema": 2, "manifest": "<原始清单 JSON 文本>", "signature": "<minisig 文本>" }
```

- 单次 PUT 生效，读取方不会看到“清单与签名来自不同版本”的中间态。
- `manifest` 是**字符串**而不是嵌套对象：签名覆盖的字节必须能精确定位，重新序列化对象可能改变键顺序。
- 客户端优先读信封：存在且验签通过则直接使用；存在但验签失败**直接拒绝**（不回退旧路径，否则等于允许把攻击降级到有窗口的通道）；不存在（404）才回退 `latest.json` + `.minisig`。
- 旧客户端不认识这个文件，继续读旧键，行为不变。

### 3.3 受信公钥集合与轮换

- 客户端二进制内嵌 **一组** minisign 公钥（至少 1 把，建议 2 把）。
- 验签条件：**任意一把受信公钥验证成功**即为通过。
- **信任集不可在运行期扩展**：客户端没有持久化信任集，也不接受清单中的新公钥。`key_rotation` 不是合法字段，出现时按“未知字段”忽略。

**为什么不做持久化信任集**：信任锚必须与二进制同生命周期。可写的信任集意味着本地用户只要能写应用数据目录，就能加入自己的公钥，再用自签清单替换 machine-wide 安装目录——这把“能写用户数据”提升为“能向受保护目录写入任意代码”。而这个能力换来的收益是零：插件随应用二进制分发，新增公钥本来就可以通过一次正常的二进制更新完成。

**轮换流程（无信任缺口）**：

```
v1 内嵌 {K1, K2}，用 K1 签名
v2 内嵌 {K2, K3}，由 K1 签名 → 旧客户端信任 K1，正常更新
v3 及之后    由 K2 或 K3 签名 → v2 起的客户端都信任
K1 在确认 v2 覆盖率达标后离线归档
```

轮换只依赖“新版本内嵌了新公钥”，不依赖客户端持久化任何状态，也不依赖清单携带信任指令。

### 3.4 版本与通道守卫

即使签名有效，客户端仍必须执行以下判定：

| 条件                                             | 行为                                                         |
| ------------------------------------------------ | ------------------------------------------------------------ |
| `version` > 本地版本                             | 正常更新                                                     |
| `version` == 本地版本                            | 无更新（`available: false`），不报错                         |
| `version` < 本地版本，`rollback != true`         | 拒绝，`E_VERSION_ROLLBACK`                                   |
| `version` < 本地版本，`rollback == true`         | 接受，记录降级来源；清除该版本的崩溃跳过标记                 |
| `channel` 与本地配置不一致                       | 无更新（`available: false`），不报错                         |
| `min_client_schema` > 客户端支持                 | 拒绝，`E_MANIFEST_SCHEMA_TOO_NEW`                            |
| 版本号不是合法 SemVer                            | 拒绝，`E_VERSION_INVALID`                                    |

**`rollback: true` 的完整语义**：

- 它是**签名保护的**字段，只有持发布私钥的一方能授权降级；MITM 无法伪造。
- 仅对“本地版本高于清单版本”的客户端生效。本地版本等于或低于清单版本时，按普通“无更新”处理，避免同一份回滚清单把已回滚的用户再拉回去。
- 接受降级后，客户端把“已安装版本”视为清单版本；后续正常发布必须以高于该版本的版本号发布。
- 降级不触发崩溃循环跳过逻辑，也不重置启动尝试计数。


## 4. 补丁对象协议

### 4.1 容器格式

所有差分补丁统一包在自有容器中。容器的作用是让运行侧在**解压之前**就能完成来源与目标的身份校验，而不依赖各差分算法自己的元数据。

```
┌────────────┬────────────┬───────────────┬─────────┬──────────┬───────────────┬───────────────┬──────────────┬─────────┐
│ magic (4B) │ format u16 │ strategy_id u16│ codec u8│ flags u8 │ target_size u64│ source_sha256 │ target_sha256│ payload │
│ "TDP1"     │ 0x0001     │ 见 4.2        │ 1/2/0   │ 保留 0   │ 目标字节数     │ (32B)         │ (32B)        │ 见 4.3  │
└────────────┴────────────┴───────────────┴─────────┴──────────┴───────────────┴───────────────┴──────────────┴─────────┘
```

- `codec`：`0` = 不压缩，`1` = zstd，`2` = lzma2。
- 所有整数字段为小端序。
- 清单中的 `sha256` 必须与容器头 `target_sha256` 一致；不一致时拒绝该补丁（`E_PATCH_FORMAT_MISMATCH`），回退全量。
- 清单 `strategy` 与容器 `strategy_id` 的映射由发布端保证一致；客户端以**容器头为准**执行，但若两者不一致，记录诊断事件并回退全量——不猜测哪一个是对的。

### 4.2 策略编号

| `strategy_id` | 清单 `strategy`  | 含义                                      |
| ------------- | ---------------- | ----------------------------------------- |
| `0x0001`      | `qbsdiff+zstd`   | bsdiff 差分，zstd 压缩（默认管线）        |
| `0x0002`      | `qbsdiff+lzma2`  | bsdiff 差分，lzma2 压缩                   |
| `0x0003`      | `hdiff+zstd`     | HDiffPatch 差分，zstd 压缩                |
| `0x0004`      | `hdiff+lzma2`    | HDiffPatch 差分，lzma2 压缩               |
| `0x0005`      | `zucchini+zstd`  | Zucchini 归一化 + bsdiff/HDiff，zstd 压缩 |
| `0x0006`      | `zstd-dict`      | 以旧文件为字典的 zstd 压缩                |
| `0xFFFF`      | 保留             | 未知策略，运行侧不得 apply                |

新增策略必须分配新编号并同步更新本表；清单 `strategy` 是字符串，客户端遇到未知字符串仍按 §2.3 处理。

### 4.3 载荷与压缩

`payload` 是差分字节经过 `codec` 压缩后的结果。若 `codec = 0`，`payload` 即差分字节本身。

**解压上限**：`payload` 解压后是差分算法的中间数据，必须有界读取。通用差分算法（qbsdiff 等）的中间数据上限为 `max_patch_file_size`（默认 256 MiB），超出立即中止并返回 `E_PATCH_DECOMPRESSION_LIMIT`；`zstd-dict` 的解压输出即目标文件，必须严格等于 `target_size`。**不能**无界解压到内存。应用差分后的目标文件在任何策略下都必须严格等于 `target_size`，否则该候选被丢弃并回退全量。

### 4.4 应用前置条件

客户端在应用补丁前必须验证：

1. 补丁对象下载完整：实际字节数与清单 `delta_size` 一致，且不超过 1 GiB。
2. 容器 magic/format 合法，`strategy_id` 在客户端支持范围内；未知编号直接回退全量。
3. 本地源文件 SHA-256 与容器头 `source_sha256` 一致；不一致返回 `E_PATCH_SOURCE_MISMATCH` 并回退全量。
4. 清单 `size` 与容器头 `target_size` 一致。
5. 暂存区有足够空间。

任一条件不满足，**不进入 apply**，直接回退到 `full_key`。

### 4.5 应用后置条件

应用完成后，目标文件必须：

1. 字节数与清单 `size` 一致。
2. SHA-256 与清单 `sha256` 一致。

**任一不一致 → 候选文件被丢弃 → 回退到全量 → 若全量仍失败则 prepare 整体失败**。

**为什么补丁不单独设哈希字段**：容器头已经把源哈希、目标哈希和目标大小绑定进补丁本身；目标哈希校验同时覆盖了“补丁字节完整”和“应用语义正确”。再维护一个补丁对象哈希只会增加清单体积和发布端出错面，不增加安全强度。


## 5. Journal 格式

### 5.1 设计目标

Journal 是 **write-ahead log**，在触碰根目录任何文件**之前**写入并 flush。**flush 成功即提交点**。提交点之后崩溃，恢复时向前重放；提交点之前崩溃，根目录未被触碰。

提交点之前还必须先落盘 `commit.meta.json` 与 `verified_manifest.json`/`.minisig`：worker 用内嵌公钥验签清单，并校验 journal 的 `Place` 目标哈希来自该清单。checksum 只发现损坏，不承担认证。验证通过后，worker 执行的是内存中已解码的同一份 Journal，不会按路径再次读取；`Backup.dst` 必须精确为 `.old/<commit_id>/<src>`，`Trash.trash_dst` 必须等于 `src`。

### 5.2 二进制格式

```
┌─────────────┬────────────┬───────────────┬──────────────┬───────────────┬─────────────────────────┬──────────────┐
│ magic (4B)  │ format u16 │ op_count u32  │ payload_len  │ commit_id(16B)│ payload                 │ checksum     │
│ "TDJ1"      │ 0x0001     │ N             │ u32          │ UUIDv4        │ version + op[0..N-1]    │ SHA-256(32B) │
└─────────────┴────────────┴───────────────┴──────────────┴───────────────┴─────────────────────────┴──────────────┘
```

- **Checksum 是尾部字段**，覆盖范围是**从 `magic` 到 `payload` 最后一个字节**（不含 checksum 自身）。校验失败 → `E_JOURNAL_CORRUPT`，**不尝试重放**。
- `payload` 内先写 `version_len u16 + version UTF-8`，再写 `from_version_len u16 + from_version UTF-8`，最后顺序写各 `op`。`from_version` 是提交前的运行版本，回滚时用于恢复到正确版本。
- 字符串字段编码为 `len u16 + UTF-8 bytes`；长度超过 1024 视为损坏。
- 整个文件不得超过 32 MiB / 100 000 个操作。上限与默认 `max_files = 20000` 对齐：即使一个安装的全部文件都被替换并伴随大量删除，也不会在提交点因容量不足失败。
- `commit_id` 用于把日志、诊断事件与替换进程的退出码关联起来；它不是安全边界。

**Checksum 不是认证**：本地攻击者可以重算 checksum。Journal 的安全性来自 **每个 `Place`/`Restore` 操作都携带目标哈希与大小**，重放时逐字节校验；伪造日志无法把未通过哈希校验的字节写进安装目录。

### 5.3 操作类型（`op`）

每个操作自包含，不依赖其他操作的结果。共五类：

| 类型          | 码值   | 字段                              | 语义                                                         |
| ------------- | ------ | --------------------------------- | ------------------------------------------------------------ |
| `Backup`      | `0x01` | `src`, `dst`                      | 将根目录中的 `src` 重命名为 `dst`（备份，仅当 `src` 存在）   |
| `Place`       | `0x02` | `staging_src`, `dst`, `sha256`, `size` | 将暂存区文件移入根目录 `dst`，落地后校验哈希与大小      |
| `Trash`       | `0x03` | `src`, `trash_dst`                | 将根目录旧官方文件移入 `.trash/`                              |
| `Restore`     | `0x04` | `backup_src`, `dst`, `sha256`, `size` | 从 `.old` 备份或 `.trash/` 恢复文件，落地后校验哈希与大小 |
| `RemoveTrash` | `0x05` | `trash_path`                      | 物理删除 `.trash/` 中的文件（只在 `confirm_healthy` 之后执行） |

**路径字段**：全部为相对路径，规则同 §1.3。

| 字段           | 解析根                                                        |
| -------------- | ------------------------------------------------------------- |
| `staging_src`  | 暂存区根 `staging/<version>/`                                  |
| `backup_src`   | 本次提交的备份根 `R/.old/<commit_id>/`，或 `.trash` 根 `R/.trash/<commit_id>/`（以 `old/` 或 `trash/` 前缀区分） |
| `dst` / `src`  | 安装根 R                                                      |
| `trash_dst`    | `R/.trash/<commit_id>/`                                       |

**每次提交使用唯一 `commit_id` 子目录**，因此 `.old` / `.trash` 不会与上一次残留或用户自建的同名文件冲突，也不需要时间戳后缀。启动清理只删除已经 `confirm_healthy` 的 `commit_id` 目录。三类根由 Journal 打开时从配置解析，**不从日志内容读取**。

**为什么 `Place` 必须携带哈希**：没有它，重放只能判断“目标是否存在”。本地攻击者改写暂存区文件并重算 checksum 后，日志会被当作合法意图执行，任意字节进入安装目录。携带哈希后，重放与正常提交走同一条“先校验来源、再落地、再校验目标”的路径，日志篡改无法绕过内容校验。

### 5.4 重放语义（幂等向前）

`recover()` 的行为：

```
for each op in journal.ops:
    if op 已经落地（目标状态成立且哈希匹配）:
        continue          # 幂等跳过
    else:
        执行 op（执行前后均校验哈希）
```

**“已经落地”的判定**：

- `Backup`：`dst` 存在，或 `src` 不存在（后续 `Place` 可能已重新填充 `src`）。
- `Place`：`dst` 存在、大小等于 `size`、SHA-256 等于 `sha256`。
- `Trash`：`trash_dst` 存在，或 `src` 不存在（`src` 可能在回滚/后续流程中重新出现）。
- `Restore`：`dst` 存在、大小与 SHA-256 匹配。
- `RemoveTrash`：`trash_path` 不存在。

**未落地且来源不可用**：`Place` 的暂存源不存在但 `dst` 哈希不匹配，或 `Restore` 的备份源不存在且 `dst` 不匹配 → 返回 `E_JOURNAL_REPLAY_FAILED`，停止重放，交由回滚流程处理。**不得跳过**——跳过会让目录停留在混合状态。

**执行前后校验**：

1. 执行 `Place`/`Restore` 前，计算来源文件哈希；不匹配 → `E_JOURNAL_REPLAY_FAILED`，不写入。
2. 执行后，计算目标文件哈希；不匹配 → 删除目标、保留来源（若仍存在），返回 `E_JOURNAL_REPLAY_FAILED`。

**并发**：提交与重放必须持有 `update.lock` 独占锁（见 §9.3）。拿不到锁的进程返回 `E_UPDATE_LOCKED`，不等待、不轮询，避免两个进程同时改同一批文件。

恢复过程本身可以被中断并再次恢复。**幂等性是恢复可靠性的核心性质**。

### 5.5 Journal 生命周期

```
prepare 完成，状态 Prepared
    │ 用户退出应用 / 调用 restart_to_apply
    ▼
主进程 spawn 独立替换进程（应用退出后运行）
    │
    ├── begin(root, ops) → 写入 + flush   ← 提交点
    │
    ├── 逐 op 执行（每次执行前后校验哈希）
    │
    ├── 全部完成 → mark_completed() → 写 commit.status=completed
    │                （Journal 与 .old/.trash 保留到 confirm_healthy，供回滚）
    │
    └── 中途崩溃 → 下次启动 recover() → 重放 → mark_completed()
```

`mark_completed()` 本身通过“写临时文件 + 原子 rename”完成，保证“要么完整存在，要么完全不存在”。

`commit.status=failed`（worker 执行失败）不会立刻回滚：下次启动先幂等重放一次，临时性原因（磁盘瞬时满、文件短暂被占用）可直接续完；仍失败则生成逆日志回滚到旧版本并记录 `skipped_version`，避免安装停留在混合状态。

**职责边界**：主进程只负责 `begin()` 之前的决策与 `begin()` 的写入；它是提交点的唯一作者。替换进程只消费已经固化的日志，不决定替换哪些文件。启动时的 `recover()` 只做恢复，不承担正常提交——正常提交发生在应用退出之后、下次启动之前。

### 5.6 回滚日志

回滚不是“把 `.old` 改回来”，而是**生成并执行另一份向前重放的日志**。这样回滚本身也能被中断并再次恢复。

回滚日志的生成规则（对提交日志逐条求逆）：

| 提交操作                       | 回滚操作                                                         |
| ------------------------------ | ---------------------------------------------------------------- |
| `Place{staging → dst}`         | 若旧版本存在该文件：`Restore{backup → dst}`；否则 `Trash{dst → .trash/<commit_id>/…}` |
| `Backup{src → .old/…}`         | `Restore{.old/… → src}`（与上一条成对出现，二选一，避免重复）     |
| `Trash{src → .trash/…}`        | `Restore{.trash/… → src}`                                        |
| `RemoveTrash{…}`               | 不可逆；回滚前不得执行 `RemoveTrash`                             |

**硬规则**：`RemoveTrash` 只在 `confirm_healthy()` 之后执行。健康确认之前的物理删除会让回滚失去素材，因此提交日志在健康确认前不包含 `RemoveTrash`。

**部分提交**：如果崩溃发生在某个 `Backup`/`Trash` 之前，对应备份素材不存在；求逆时跳过该条（旧文件仍在原位或尚未创建），不视为回滚失败。只有素材存在但恢复失败才是 `E_ROLLBACK_FAILED`。

**受信旧哈希**：`begin_commit` 在写 journal 前把将被替换/删除文件的 sha256 + size 写入 `old_hashes.json`。求逆生成的 `Restore` 使用这份快照校验备份；备份损坏时拒绝回滚，而不是用备份当前字节现算哈希后“自证正确”。


