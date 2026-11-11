# Delta Update Engine 构建与发布操作手册

> 本文面向发布工程师与 CI 维护者。构建与发布阶段的每一个操作，都会直接影响客户端能否安全、正确地完成更新。发布事故往往不来自引擎代码，而来自流程：私钥被写进日志、清单先于补丁上传、回滚素材被提前清理。本文的每一项规范，都以“防止这类事故”为唯一目的。


## 1. 发布流程总览

一次完整的发布，从代码构建到客户端可感知的更新，经过以下阶段：

```
┌──────────┐   ┌──────────┐   ┌──────────┐   ┌──────────┐   ┌──────────┐
│ 构建产物  │──►│ 生成清单  │──►│ 生成补丁  │──►│  签名    │──►│  发布    │
│ dist/2.x │   │ latest.  │   │ *.tdp    │   │ .minisig │   │ 原子上传  │
└──────────┘   │ json     │   └──────────┘   └──────────┘   └──────────┘
               └──────────┘
```

**关键顺序约束**：补丁和全量对象**必须先于**清单到达 COS；签名**必须先于**对应清单到达 COS。客户端只有在清单指向某对象时才会去拉取它，顺序颠倒会产生“清单已更新但对象 404”或“清单已到但签名缺失”的窗口。这是原子发布的核心规则，后文 §4 将展开。


## 2. 签名私钥的安全注入

### 2.1 核心原则

签名私钥是信任链的物理根。私钥一旦泄露，攻击者可以签发任意“合法”更新。因此，私钥**绝不落仓库、绝不落日志、绝不落构建产物、绝不作为命令行参数传递**。CI 中的注入遵循“最小暴露、使用即焚”的原则。

### 2.2 GitHub Actions 中的正确做法

**私钥存放**：使用 GitHub Actions 的 **protected environment secret**（而非普通 repository secret）。protected environment 可以配置审批人、限制到受保护的分支或 tag，提供加密存储和访问控制。

```yaml
# .github/workflows/release.yml
jobs:
  release:
    runs-on: ubuntu-latest
    environment: release           # 受保护的环境
    steps:
      - name: Release delta update
        env:
          MINISIGN_SECRET_KEY_BASE64: ${{ secrets.MINISIGN_SECRET_KEY_BASE64 }}
        run: |
          # 将 base64 私钥解码到临时文件，使用后立即删除
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

**不要这样做**：

- ❌ 把私钥直接写在 `run` 的命令行参数里（`--key "actual-secret"`）——会出现在进程列表和日志中。
- ❌ 把私钥写入 `$GITHUB_ENV` 或 `$GITHUB_OUTPUT`——可能被后续步骤或日志捕获。
- ❌ 在 debug 日志或 `echo` 中打印私钥内容。
- ❌ 把私钥提交到仓库的 `.key` 文件里，即使 `.gitignore` 了——历史记录仍然存在。

### 2.3 公开密钥的存放

公钥**不是 secret**，它本来就是公开信息。应当作为 **repository Actions variable** 配置，供 CI 中的预检步骤验证私钥与公钥的配对关系。

```yaml
- name: Verify key pair
  env:
    MINISIGN_SECRET_KEY_BASE64: ${{ secrets.MINISIGN_SECRET_KEY_BASE64 }}
    MINISIGN_PUBLIC_KEYS: ${{ vars.MINISIGN_PUBLIC_KEYS }}
  run: |
    echo "$MINISIGN_SECRET_KEY_BASE64" | base64 -d > /tmp/release.key
    delta-update verify-key --key /tmp/release.key --public "$MINISIGN_PUBLIC_KEYS"
    rm -f /tmp/release.key
```

`MINISIGN_PUBLIC_KEYS` 的格式为逗号分隔的 base64 公钥载荷，轮换期间恰好两个，无空格、无尾随分隔符。


## 3. `delta-update release` 命令参考

### 3.1 完整参数

```
delta-update release [OPTIONS]

必填：
  --to <DIR>              新版本的构建输出目录
  --version <SEMVER>      新版本号，写入清单并由客户端用于版本比较
  --bucket <URI>          cos://bucket、s3://bucket 或本地目录
  --key <PATH>            minisign 私钥文件路径（从临时文件读取，不从参数传入内容）

增量发布：
  --from <DIR>            上一版本的构建输出目录。缺失时所有文件视为新增，
                          全部走全量，等效于 --initial。
  --from-version <SEMVER> 上一版本号，用于补丁对象键；提供 --from 时必填。

可选：
  --initial               首次发布标记。等价于不提供 --from。
  --rollback              生成 rollback: true 的降级清单（版本号低于当前发布版本）。
  --channel <NAME>        更新通道，默认 stable。
  --notes-file <PATH>     发布说明文件，写入清单 notes（UTF-8，最大 64 KiB）。
  --mandatory             标记本次更新为强制更新（清单 mandatory: true）。
  --region <REGION>       COS / S3 区域，如 ap-guangzhou、us-east-1、auto（R2）。
  --prefix <PREFIX>       对象键前缀，默认空（根前缀）。
  --dry-run               生成清单和补丁，但不上传对象存储。
  --require-delta         本应差分的文件未能产出低于阈值的补丁时，构建失败。
  --delta-max-ratio <F>   补丁体积超过全量的此比例时，降级为全量。默认 0.60。
  --patch-max-file-size <BYTES>
                          超过此大小的文件不做差分、直接全量。默认 64 MiB。
                          qbsdiff 内存约为输入的 17 倍，需提高时同时降低 --jobs。
  --verify                上传完成后自动执行 verify。
  --jobs <N>              补丁生成的并行度，默认 CPU 核数。
  --verbose               输出每个文件的策略选择和补丁体积。
```

### 3.2 典型调用

**首次发布**：

```bash
delta-update release \
  --to ./dist \
  --version 1.0.0 \
  --bucket cos://your-bucket \
  --key /tmp/release.key \
  --initial \
  --region ap-guangzhou
```

**增量发布**：

```bash
delta-update release \
  --from ./dist-2.0.0 \
  --from-version 2.0.0 \
  --to   ./dist-2.1.0 \
  --version 2.1.0 \
  --bucket cos://your-bucket \
  --key /tmp/release.key \
  --region ap-guangzhou \
  --require-delta \
  --verbose
```

`--require-delta` 的语义：对每个 `Modified` 且未被标记为“不可压缩格式”的文件，如果最终策略是 `full`，或补丁体积 ≥ `--delta-max-ratio`，发布失败。显式标记为不可压缩（png/jpg/zip 等）的文件不参与判定。这防止 CI 静默退化为“全量发布”而无人察觉。

`--notes-file` 与 `--mandatory` 是产品提示字段：客户端 `check` 会原样返回 `notes` 与 `mandatory`，由应用的更新弹窗决定如何展示、是否禁止跳过。它们不参与签名之外的任何决策，也不会绕过版本守卫。

### 3.3 S3 兼容存储（AWS S3 / Cloudflare R2 / MinIO）

`--bucket s3://your-bucket` 使用 S3 Signature V4（path-style），凭证从环境变量读取：

| 变量 | 必填 | 说明 |
| ---- | ---- | ---- |
| `S3_ACCESS_KEY_ID` | 是（或 `AWS_ACCESS_KEY_ID`） | Access key |
| `S3_SECRET_ACCESS_KEY` | 是（或 `AWS_SECRET_ACCESS_KEY`） | Secret key |
| `S3_REGION` | 否 | 默认 `us-east-1`；R2 用 `auto` |
| `S3_SESSION_TOKEN` | 否 | 临时凭证 token（也可用 `AWS_SESSION_TOKEN`） |
| `S3_ENDPOINT` | 否 | 自定义端点；AWS 默认 `https://s3.<region>.amazonaws.com` |

```bash
# Cloudflare R2
S3_ENDPOINT="https://<account>.r2.cloudflarestorage.com" \
S3_REGION=auto \
delta-update release --to ./dist --version 2.1.0 \
  --bucket s3://your-bucket --key /tmp/release.key --from ./dist-2.0.0 \
  --from-version 2.0.0

# MinIO（本地/内网）
S3_ENDPOINT="http://127.0.0.1:9000" S3_REGION=us-east-1 \
delta-update release ... --bucket s3://updates
```

客户端仍然只读取 HTTPS 清单端点；S3/R2 的公开读 URL 或 CDN 域名填入插件的 `endpoint` 即可。

### 3.4 CLI 内部行为

`delta-update release` 按以下顺序执行，任一步失败即中止：

1. 扫描 `--to` 目录，递归计算每个文件的 SHA-256 和大小。
2. 读取 `--from` 目录（若提供），逐文件比对，分类为 `Unchanged` / `Modified` / `Added`。
3. 对 `Modified` 文件，可压缩的文件走 qbsdiff+zstd，生成 `.tdp` 容器；已压缩格式或超过 `--patch-max-file-size`（默认 64 MiB）的文件直接标记全量。**当前不做文件内分块差分**——超大文件走全量比 OOM 更可靠。
4. 若候选补丁 ≥ 全量的 `--delta-max-ratio`，降级为全量（`strategy: "full"`）。
5. 生成 `latest.json`，包含所有文件条目和版本元数据。
6. 用 `--key` 指定的私钥对**清单原始字节**签名，产出 `.minisig`。
7. **先上传补丁和全量对象**，逐个 `HEAD` 校验存在且字节数正确。
8. 上传 `<version>.json.minisig` 与 `latest.json.minisig`——**签名必须先于对应清单存在**。
9. 上传 `<version>.json`，**最后上传 `latest.json`**。
10. `--verify` 时重新下载清单、验签、抽样应用补丁并校验目标哈希。


## 4. 原子发布顺序与对象存储操作

### 4.1 顺序即安全

发布顺序不是风格偏好，而是正确性要求：

```
错误顺序：清单先上传 → 客户端拉取清单 → 去拉补丁 → 补丁 404 → 更新失败
正确顺序：补丁先上传 → 确认存在 → 签名先上传 → 清单最后上传
```

清单与其签名之间也存在同样的窗口：如果清单先到而 `.minisig` 未到，客户端会报 `E_SIGNATURE_SIG_MISSING`。因此**签名对象永远先于对应清单上传**。

CLI 的 `release` 命令强制执行此顺序。但如果你手工操作 COS 或使用自定义发布脚本，必须保持同样的顺序。

### 4.2 对象存储目录结构

```
/your-bucket/
  ├── manifests/
  │   ├── latest.json                    # 插件唯一直接请求的清单
  │   ├── latest.json.minisig
  │   ├── 2.0.0.json                     # 历史版本，供回滚/降级素材
  │   └── 2.0.0.json.minisig
  └── artifacts/
      ├── 2.0.0-to-2.1.0/
      │   ├── App.exe.tdp
      │   └── bin/core.dll.tdp
      └── full/
          └── 2.1.0/
              ├── App.exe
              └── resources/icon.png
```

**`manifests/` 前缀**只存放清单和签名。**`artifacts/` 前缀**存放补丁和全量对象。`latest.json` 是唯一被客户端直接请求的 URL（通过插件的 `endpoint` 配置），其余对象键由清单内部指定。

配置 `--prefix` 时，上述所有键（包括 `manifests/`、`artifacts/` 以及清单内部的对象键）都会加上该前缀；客户端的 `endpoint` 必须指向 `<prefix>/manifests/latest.json`。

### 4.3 COS 生命周期与版本保留

COS 生命周期规则可以自动沉降或删除旧对象以控制存储成本。**但回滚素材不能被提前清理**——否则新版本出问题时，客户端没有旧版本的清单和补丁可回退。

推荐的生命周期策略：

| 对象类别                    | 保留策略                   | 理由                     |
| --------------------------- | -------------------------- | ------------------------ |
| `manifests/latest.json*`    | **永不自动删除**           | 客户端始终需要最新清单   |
| `manifests/<version>.json*` | 保留 **至少 3 个**最近版本 | 回滚和降级素材           |
| `artifacts/<from>-to-<to>/` | 保留 **至少 2 个**版本对   | 覆盖最近的更新路径       |
| `artifacts/full/<version>/` | 保留 **至少 2 个**最近版本 | 差分失败时的全量回退来源 |

生命周期规则应基于对象前缀分别配置，**不要对同一组对象配置冲突条件的多条规则**。

### 4.4 预签名 URL 的约束

私有 COS 桶需要通过预签名 URL 分发。插件内置了 URL 过期自动刷新和断点续传，但有两个来自 COS 平台的硬约束必须遵守：

- **预签名 URL 不支持断点续传**：当指定预签名 URL 的有效时间过期后，请求会中断；申请新的签名后，需要**重新执行失败请求**，不支持在同一个 URL 上续传。插件的 Fetcher 在 URL 过期后会刷新 URL 并从当前字节偏移续传，但这依赖于 Fetcher 层重新发起 Range 请求，而非 COS 自身支持。
- **有效期受临时密钥限制**：使用临时密钥生成的预签名 URL，其过期时间由**临时密钥和预签名 URL 两个中较短者**决定。签名服务的临时密钥有效期应大于 URL 有效期。

**建议配置**：预签名 URL 有效期 ≤ 15 分钟，临时密钥有效期 ≥ 30 分钟。最小权限策略只授予 `GetObject`，资源范围限制在 `manifests/` 和 `artifacts/` 前缀。


## 5. `.deltaignore` 语法

### 5.1 目的

构建输出目录中可能包含不应进入清单的文件：调试符号、临时文件、本地配置。`.deltaignore` 放在 `--to` 目录根部，控制哪些文件被排除在清单和差分管线之外。

### 5.2 语法

与 `.gitignore` 兼容的子集：

```
# 注释行
*.pdb
*.dSYM/
*.log
/tmp/
!important.pdb      # 取反：不排除此文件
```

- 每行一个模式。
- `*` 匹配任意字符序列（不跨越 `/`）。
- `/` 结尾表示仅匹配目录。
- `!` 开头表示取反（重新包含）。
- 空行和 `#` 开头的行被忽略。

### 5.3 默认排除规则

即使没有 `.deltaignore`，CLI 也默认排除以下模式：

```
*.pdb
*.dSYM/
*.map
*.tmp
*.log
.DS_Store
Thumbs.db
.deltaignore
```

这些文件通常不参与运行时，进入清单只会增加差分开销和发布体积。


## 6. Dry-Run 验证

### 6.1 何时使用

在正式上传 COS 之前，用 `--dry-run` 生成清单和补丁并检查体积，**不上传**。适用于：

- 首次验证差分管线的健康度。
- 评估某次发布的实际流量节省效果。
- 在 CI 中作为“发布预检”步骤。

```bash
delta-update release \
  --from ./dist-2.0.0 \
  --from-version 2.0.0 \
  --to   ./dist-2.1.0 \
  --version 2.1.0 \
  --bucket cos://your-bucket \
  --key  /tmp/release.key \
  --dry-run \
  --verbose
```

### 6.2 Dry-Run 的输出

CLI 输出每个文件的策略选择和体积对比：

```
FILE                          STRATEGY          FULL      DELTA    RATIO
App.exe                       qbsdiff+zstd      10.0 MB   78 KB    0.76%
bin/core.dll                  qbsdiff+zstd      2.0 MB    12 KB    0.59%
resources/icon.png            full              5 KB      —        —
```

**检查要点**：

- 高熵/已压缩格式是否被标记为 `full`。
- 是否有文件的 `RATIO` 接近 `--delta-max-ratio`，考虑是否应调整策略或降级。
- `--require-delta` 模式下，是否有文件未能通过阈值。

### 6.3 发布后验证

正式上传完成后，建议再执行一次**从 COS 拉取的验证**：

```bash
delta-update verify \
  --bucket cos://your-bucket \
  --version 2.1.0 \
  --public-key ./release.pub \
  --sample 3
```

`verify` 会：重新下载清单、验签、抽样下载补丁、应用补丁、校验目标哈希。这确认了“发布到 COS 的内容”与“CI 生成的内容”一致，排除了上传过程中的损坏或覆盖问题。


## 7. 回滚版本的发布

### 7.1 回滚发布的特殊性

正常发布中，客户端拒绝 `version < 本地版本` 的清单（`E_VERSION_ROLLBACK`）；`version == 本地版本` 视为无更新。**回滚发布必须显式覆盖此守卫**，否则客户端不会接受降级。

回滚发布有两种场景：

| 场景                                       | 触发方式                                                     | 客户端行为                       |
| ------------------------------------------ | ------------------------------------------------------------ | -------------------------------- |
| **紧急回滚**（新版本已发布但发现严重问题） | 将 `latest.json` 的 `version` 指向旧版本，并附带 `rollback: true` 标记 | 客户端接受降级，清除崩溃循环状态 |
| **常规降级**（用户手动选择旧版本）         | 通过独立的降级通道                                           | 需要用户显式确认                 |

### 7.2 紧急回滚发布命令

```bash
# --from：当前已发布的版本（作为“旧”版本）
# --to：目标回滚版本（作为“新”版本）
delta-update release \
  --from ./dist-2.1.0 \
  --from-version 2.1.0 \
  --to   ./dist-2.0.0 \
  --version 2.0.0 \
  --bucket cos://your-bucket \
  --key /tmp/release.key \
  --rollback \
  --channel stable
```

`--rollback` 标志的作用：

1. 清单中 `version` 设置为 `2.0.0`，并附带 `"rollback": true`。
2. 生成从 `2.1.0` → `2.0.0` 的差分补丁。
3. 客户端仅在**本地版本高于 2.0.0** 时接受降级；已经在 2.0.0 或更低版本的客户端按“无更新”处理，避免同一份回滚清单把用户反复拉回。
4. 接受降级后清除该版本的崩溃跳过标记，但**不重置启动尝试计数**。

### 7.3 回滚发布的注意事项

- 回滚清单必须由**受信私钥**签名，签名覆盖范围与正常发布一致。
- 回滚后，**不要立即重新发布被回滚的版本**。等待问题修复，以更高的版本号发布。
- 保留被回滚版本的清单和补丁，供诊断和后续修复参考。


## 8. 补丁素材保留与清理策略

### 8.1 必须保留的素材

| 素材                                                | 保留时长                          | 理由                   |
| --------------------------------------------------- | --------------------------------- | ---------------------- |
| `manifests/latest.json` 和 `.minisig`               | **永久保留，不参与自动清理**      | 所有客户端的更新入口   |
| 最近 3 个版本的 `manifests/<version>.json` 和签名   | 至少保留 3 个版本                 | 回滚发布和降级通道依赖 |
| 最近 2 个版本对的差分补丁                            | 至少保留 2 个版本对               | 覆盖主流更新路径       |
| 最近 2 个版本的全量对象                              | 至少保留 2 个版本                 | 差分失败时的回退来源   |

### 8.2 可以清理的素材

| 素材                          | 清理条件                               |
| ----------------------------- | -------------------------------------- |
| 超过 3 个版本之前的清单和补丁 | 确认没有客户端仍在使用该版本作为源版本 |
| 超过 2 个版本对之前的补丁     | 同上                                   |
| 失败发布产生的临时对象        | 上传完成后立即清理                     |

**不要清理**：`manifests/latest.json` 和 `manifests/latest.json.minisig`。它们是所有客户端的入口。


## 9. 发布检查清单

每次发布前逐项确认。**任何一项不通过，阻塞发布**。

### 9.1 产物完整性

- [ ] `--to` 目录是干净的构建输出，不包含 `.pdb`、`.tmp`、`.log` 等非发布文件
- [ ] `.deltaignore` 已配置并验证（或默认排除规则已覆盖）
- [ ] 清单中的 `version` 严格大于上一版本
- [ ] 清单中的每个 `sha256` 与 `--to` 目录中的实际文件一致
- [ ] 所有 PE 文件已签名，且签名发生在扫描 `--to` 目录之前（§12.2）

### 9.2 签名与密钥

- [ ] `--key` 指向的私钥与客户端内嵌的 `signing_public_keys` 中的至少一把配对
- [ ] 私钥只存在于临时文件或 protected environment secret 中，**不在日志、环境变量、命令行参数、仓库文件中**
- [ ] 签名后 `latest.json.minisig` 存在且非空
- [ ] 若执行密钥轮换，新公钥已内嵌进客户端二进制；清单中不含 `key_rotation`

### 9.3 原子发布顺序

- [ ] **所有补丁对象和全量对象已上传**，且 COS 上存在、字节数正确
- [ ] 签名对象**先于**对应清单上传，`latest.json` 最后上传
- [ ] 上传完成后执行 `delta-update verify`，从 COS 拉取并验证抽样补丁

### 9.4 COS 与权限

- [ ] 运行时签名服务使用的临时密钥只有 `GetObject` 权限，范围限制在 `manifests/` 和 `artifacts/` 前缀
- [ ] 发布凭证与运行时凭证是不同身份
- [ ] 预签名 URL 有效期 ≤ 15 分钟，临时密钥有效期 ≥ URL 有效期
- [ ] 防盗链配置已启用，非授权 Referer 被拒绝
- [ ] 生命周期规则**不会**在保留期内删除回滚素材（§4.3）

### 9.5 回滚素材

- [ ] 上一版本的清单和补丁仍然保留在 COS 上
- [ ] 最近 2 个版本对的全量对象仍然保留
- [ ] 若本次为回滚发布，`--rollback` 标志已正确设置

### 9.6 安装器与包发布

- [ ] NSIS perUser 与 perMachine 的 §12.3 手动清单通过
- [ ] `cargo publish -p delta-update-engine --dry-run --registry crates-io` 通过
- [ ] `cargo package --list` 与 `npm pack --dry-run` 的打包内容符合预期（§13.2）
- [ ] `cargo xtask version-sync` 确认 crate、插件 manifest 与 npm 版本一致


## 10. 常见发布事故与排查

| 症状                                     | 可能原因                                       | 处理                                                         |
| ---------------------------------------- | ---------------------------------------------- | ------------------------------------------------------------ |
| 客户端报 `E_SIGNATURE_INVALID`           | 私钥与客户端公钥不配对；签名对象不是原始字节   | 执行 `delta-update verify-key` 确认配对；确认签名流程未对解析后 JSON 签名 |
| 客户端报 `E_NETWORK_HTTP_4XX` 且对象存在 | 预签名 URL 对象键与清单中的 `delta_key` 不一致 | 检查 COS 上的实际对象键是否与清单完全一致（含大小写和前缀）  |
| 客户端拉取补丁 404                       | 清单先于补丁上传                               | 严格执行 §4.1 的顺序；用 `delta-update verify` 在发布后验证  |
| `--require-delta` 构建失败               | 某个文件的差分补丁超过 `--delta-max-ratio`     | 检查该文件的类型和路由策略；可能该文件本应标记为 `full` 而被错误路由到了差分管线 |
| 发布后磁盘占用异常增长                   | 生命周期规则未配置或配置了冲突条件             | 按 §4.3 的前缀分别配置规则，避免同一组对象的多条冲突规则     |


## 11. 发布手册总结

构建与发布阶段的可靠性，取决于三条被严格遵守的规则：**私钥只存在于临时文件或 protected secret 中，使用后立即清除**；**补丁先于清单到达 COS**；**回滚素材在保留期内不被生命周期规则清理**。这三条规则一旦被违反，后果分别是密钥泄露、更新 404、回滚无素材——都是可以在发布后立即被用户感知的严重事故。

建议把 §9 的检查清单作为 CI 的**硬门禁**，而非人工备忘。`delta-update verify` 和 `delta-update verify-key` 应当作为发布流水线的标准步骤，在任何一次 `release` 之后自动执行。发布流程的自动化程度越高，人为绕过规则的概率越低。


## 12. Windows 代码签名与安装器验收

### 12.1 Authenticode 与 minisign 的分工

两者解决不同问题，缺一不可：

| 机制        | 保护对象                       | 信任来源                              | 失败后果                         |
| ----------- | ------------------------------ | ------------------------------------- | -------------------------------- |
| minisign    | 清单与更新对象                 | 客户端二进制内嵌的公钥集合            | 拒绝更新，`E_SIGNATURE_*`        |
| Authenticode | 应用 exe/DLL 与安装器本身的来源 | Windows 证书链与时间戳                | SmartScreen 警告、企业策略拦截   |

**不要用 Authenticode 代替 minisign**：Authenticode 不覆盖 `.tdp` 补丁和清单内容，也不提供“任意一把受信公钥”的轮换语义。反之，minisign 不能阻止用户下载到一个被替换的安装器。

### 12.2 签名流程

1. 构建完成后、上传安装器之前，对**最终会落盘的所有 PE 文件**签名：主 exe、随包 DLL、NSIS 安装器。更新 worker 是主 exe 的受限模式，与主 exe 共用同一份签名，不需要单独签名。
2. 使用仓库脚本签名并立即验证：

```powershell
pwsh scripts/windows/sign-authenticode.ps1 `
  -Path .\target\release\my-app.exe `
  -Thumbprint $env:CODESIGN_THUMBPRINT

pwsh scripts/windows/sign-authenticode.ps1 `
  -Path .\target\release\bundle\nsis\my-app_1.2.3_x64-setup.exe `
  -Thumbprint $env:CODESIGN_THUMBPRINT
```

脚本使用 SHA-256 摘要与 RFC 3161 时间戳，并在签名后执行 `signtool verify /pa`。**时间戳必须启用**，否则证书过期后已分发的安装器会失去有效性。

3. 签名后的二进制哈希会变化，因此**签名必须发生在 `delta-update release` 扫描产物之前**。当前 CLI 的清单哈希来自 `--to` 目录的实际字节；先扫描再签名会让清单与最终文件不一致。

### 12.3 NSIS perUser / perMachine 手动验收清单

真实 UAC、安装器与 `Program Files` 写入无法在无提权 CI 中自动化，每次发布前在 Windows 虚拟机手动执行。**任一项失败阻塞发布**。

**perUser（默认，无 UAC）**

- [ ] 安装到用户目录后，应用正常启动，`Program Files` 下无残留。
- [ ] 完整更新一次：退出应用无 UAC，重启后版本号为新版本。
- [ ] 更新后首次启动不弹 UAC（已提交待确认不触发恢复提权）。
- [ ] `%LOCALAPPDATA%\<bundleIdentifier>\delta-update\journal\` 在健康确认后只剩 `known_files.json` 与空锁文件。
- [ ] 卸载后安装目录、开始菜单项与更新数据目录按预期清理。

**perMachine（`Program Files`，需要 UAC）**

- [ ] 以管理员安装到 `Program Files`，普通用户启动应用不弹 UAC（未发生更新时）。
- [ ] 完整更新一次：应用退出后弹一次 UAC，更新完成后新版本经 Explorer 降权启动，任务管理器中的完整性级别为“中”。
- [ ] 在提交执行到一半时强制结束 worker，再次启动应用：启动阶段弹一次 UAC，授权后 Journal 被重放或回滚，主窗口正常出现。
- [ ] 上述启动提权选择“否”：应用以 `Failed` 启动，`journal/` 与 `.old/` 保留；再次启动仍会请求授权。
- [ ] 连续两次不确认健康后第三次启动：自动回滚到旧版本，`skipped_version` 写入，计数归零。
- [ ] perUser：确认健康后当前 commit 的 `.old` / `.trash` 被清理；perMachine：确认在用户级运行，无权限删除的残留会保留（不影响运行），需在管理员会话中人工清理。
- [ ] 卸载后 `Program Files` 目录、注册表卸载项与用户数据目录按预期清理。

**签名检查（两种模式都执行）**

- [ ] `Get-AuthenticodeSignature` 对安装后的主 exe 返回 `Status = Valid`，且时间戳存在。
- [ ] 安装器属性页显示预期的发布者名称，SmartScreen 不出现“未知发布者”。

### 12.4 应用列表版本的边界

文件级更新只替换安装目录中的文件，**不会改写 NSIS 安装器写入的 `DisplayVersion`**。因此“应用和功能”里可能仍显示安装时的版本，而应用自身已经是新版本。需要保持一致时，可在 `confirm_healthy` 之后由应用更新对应卸载键（perUser 为 HKCU、perMachine 为 HKLM），或用新安装包覆盖安装。不要为了同步版本号而让主进程整体提权。

### 12.5 无证书环境的边界

仓库与 CI 不包含代码签名证书。没有证书时**不要伪造签名校验逻辑**：可以构建与测试更新链路，但 `Get-AuthenticodeSignature` 会返回 `NotSigned`，SmartScreen 会告警，企业环境可能直接拦截。证书申请、HSM/密钥保管与 CI 注入属于组织流程，不在本项目代码范围内。


## 13. crates.io 与 npm 发布

### 13.1 发布顺序

1. `delta-update-engine`
2. `tauri-plugin-delta-update`（依赖 engine 的已发布版本）
3. `delta-update-cli`（依赖 engine）
4. `tauri-plugin-delta-update-api`（npm，独立于 crate 发布顺序）

`delta-update-testkit` 与 `e2e`、`xtask` 标记 `publish = false`。testkit 只作为**开发依赖**使用且不写版本号，Cargo 打包时会将它从发布清单中剔除；**不要给 testkit 加 `version`**，否则会要求先发布一个测试夹具 crate。

### 13.2 发布前校验

```bash
# 清单与编译校验（不真正上传）
cargo publish -p delta-update-engine --dry-run --registry crates-io

# 依赖 engine 的 crate 需要 engine 已发布后才能 dry-run；
# 首次发布前用 package 列表检查打包内容
cargo package -p tauri-plugin-delta-update --list --allow-dirty
cargo package -p delta-update-cli --list --allow-dirty

# npm 包内容（dist、README、package.json）
cd packages/plugin-api && npm pack --dry-run
```

发布前确认：`readme`、`keywords`、`categories`、`license` 已填写；`repository` 字段在绑定公开仓库后补充（当前仓库未配置远端，不应伪造 URL）。`cargo xtask version-sync` 必须保证 workspace、插件 manifest 与 npm 包版本一致。

### 13.3 密钥轮换的当前边界

`key_rotation` **不是清单字段，也不在运行期生效**：清单出现该字段时按未知字段忽略，插件配置出现时只输出一条警告。信任集只来自客户端二进制内嵌的 `signing_public_keys`，因此轮换只能通过“旧密钥签名的新版本内嵌新公钥”完成，不能通过清单或配置在线扩展信任。

这意味着两件事：

- 正常轮换必须**提前一个版本**把新公钥内嵌进客户端；如果旧客户端尚未更新，就不能只用新密钥签名。
- 私钥在轮换生效前泄露时，系统没有带内吊销能力；不要试图用 `key_rotation` 之类的字段“热修复”信任集。