# Delta Update Engine 构建与发布操作手册

> 本文件是[发布手册](release.md)的流程部分：签名注入、`release` 参数、原子发布顺序、`.deltaignore`、dry-run 与回滚发布。


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

从私钥导出公钥与 key ID（不需要安装 minisign）：

```bash
delta-update key-info --key /tmp/release.key
# public key: RWQ...
# key id: 1F2E...
```


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
  --min-supported <SEMVER> 发布者仍支持的最旧版本（清单 min_supported_version）。
                          必须 <= --version；低于它的客户端在 check 中收到
                          update_required: true，由应用决定多严格。
  --additional-signing-key <PATH>
                          额外的 minisign 私钥，可重复。每把 key 会写一份
                          manifests/<version>.json.minisig.<key_id> 与 latest 对应文件，
                          并把 key id 写入清单 signing_key_ids；旧客户端仍用主签名。
                          轮换时先发内嵌新公钥的客户端，再用这里加签，最后切主签名。
  --region <REGION>       COS / S3 区域，如 ap-guangzhou、us-east-1、auto（R2）。
  --prefix <PREFIX>       对象键前缀，默认空（根前缀）。
  --dry-run               生成补丁并在内存中签名清单，打印摘要；不落盘、不上传。
  --require-delta         本应差分的文件未能产出低于阈值的补丁时，构建失败。
  --delta-max-ratio <F>   补丁体积超过全量的此比例时，降级为全量。默认 0.60。
  --patch-max-file-size <BYTES>
                          超过此大小的文件不做差分、直接全量。默认 64 MiB。
                          qbsdiff 内存约为输入的 17 倍，需提高时同时降低 --jobs。
  --verify                上传完成后自动执行 verify。
  --self-test             在 --from 的临时副本上做 apply + rollback 演练；需要 --from。
  --jobs <N>              哈希扫描与补丁生成的并行度，默认 min(核数/2, 4)；一个值同时覆盖两个阶段。
                          启动时会打印 “patch jobs: N (M files, largest ..., estimated peak ~P)”；P 是最大的 N 个输入之和 × 6，而不是 N × 单文件上限。
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

`--min-supported` 用于安全场景：它把“最低可接受版本”写进签名清单，客户端 `check` 返回 `update_required: true`，插件进入 `UpdateRequired` 状态。引擎不会阻止应用启动——紧急医疗、离线等场景仍可继续使用；应用可以据此显示不可关闭的更新提示或禁用联网功能。

默认输出在 `released ...` 之后还有一行 **`delta coverage: N/M modified files patched (...)`**，直接回答“增量是否真的生效”：未生效的数量会按 `too large` / `ratio` / `compressed` 拆开，便于调整 `--patch-max-file-size` 或 `--delta-max-ratio`。

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

### 3.4 一体化发布 `--pipeline`

`--pipeline` 把“生成 → 自测 → 上传 → 审计”串成一次运行：

```bash
delta-update release --from build/1.9.0 --to build/2.0.0 \
  --version 2.0.0 --from-version 1.9.0 \
  --bucket cos://your-bucket --key release.key \
  --pipeline --report release-report.json
```

1. 生成补丁、清单、签名（同普通 `release`）。
2. 在 `--from` 的临时副本上执行 `--self-test` 的 apply + rollback。
3. 按原子顺序上传对象、签名、清单与 SBOM。
4. 用 `inspect --from-release` 的同一套逻辑验签并 `HEAD` 校验每个对象。
5. 写 `release-report.json`（`v`/`tool`/覆盖率/自测/审计/观察窗口）。

`--watch 600 --interval 60` 会在审计后继续按间隔重跑审计，直到时间窗口结束；任何一轮发现对象缺失或大小不符都会让整次发布失败，适合 CDN 传播与对象存储一致性延迟的场景。

任一步失败即停止，不会写报告，退出码按 [CLI 契约](../cli-contract.md) 透传。`--initial` 会跳过自测并在报告中记录 `"ran": false`。CI 可以把报告直接作为 artifact 归档；GitHub Actions 与 GitLab CI 的模板见 [`examples/ci/`](../../examples/ci/)。

把 `--report` 指向目录（或没有 `.json` 后缀的路径）时，报告按 `<version>.json` 归档，例如 `--report .reports/`。之后用趋势表检查增量策略是否退化：

```bash
delta-update report --history .reports/
```

```text
VERSION       CHANGED  COVERAGE      DELTA       FULL  PATCH/FALLBACK
2.0.0              12     75.0%     1.2 MB     4.8 MB            9/3
2.1.0              15     40.0%     3.1 MB     7.8 MB            6/9
warning: 2.1.0 coverage 40.0% is 35 points below 2.0.0; new fallbacks: bin/large.dll (too_large)
```

覆盖率相比上一版本下降超过 20 个百分点时，警告会列出该版本新增的回退文件与原因——通常意味着某个文件意外膨胀超过 `--patch-max-file-size`。

需要做 CI 门禁时加 `--json`（schema 见 [CLI 契约 2.6](../cli-contract.md#26-report---history---json)）：

```bash
delta-update report --history .reports/ --json > coverage.json
```

### 3.5 CLI 内部行为

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
10. 生成并上传 `manifests/<version>.sbom.json`（只含清单已有的路径、哈希、大小、策略与对象键，供合规审计；客户端不读取，也不参与更新）。
11. `--verify` 时重新下载清单、验签、抽样应用补丁并校验目标哈希。**注意**：`--verify` 用私钥派生的公钥自检，只能证明上传内容与签名自洽；要验证“客户端内嵌公钥是否信任这把密钥”，请单独执行 `delta-update verify --public-key <客户端公钥>`。


## 4. 原子发布顺序与对象存储操作

### 4.1 顺序即安全

发布顺序不是风格偏好，而是正确性要求：

```
错误顺序：清单先上传 → 客户端拉取清单 → 去拉补丁 → 补丁 404 → 更新失败
正确顺序：补丁先上传 → 确认存在 → 签名先上传 → 清单最后上传
```

清单与其签名之间也存在同样的窗口：如果清单先到而 `.minisig` 未到，客户端会报 `E_SIGNATURE_SIG_MISSING`。因此**签名对象永远先于对应清单上传**。

`release` 还会在最后写一次 `manifests/latest.envelope.json`：它把清单与主签名放进同一个对象，新客户端优先读它，因此不再有清单/签名不匹配的窗口；旧客户端仍读 `latest.json` + `.minisig`，由客户端重试兜底。`rollback` 同样会更新信封。

CLI 的 `release` 命令强制执行此顺序。但如果你手工操作 COS 或使用自定义发布脚本，必须保持同样的顺序。

### 4.2 对象存储目录结构

```
/your-bucket/
  ├── manifests/
  │   ├── latest.json                    # 旧客户端读取的清单
  │   ├── latest.json.minisig
  │   ├── latest.envelope.json           # 清单 + 主签名，新客户端优先读
  │   ├── latest.json.minisig.<key_id>   # 轮换期的附加签名（可选）
  │   ├── 2.0.0.json                     # 历史版本，供回滚/降级素材
  │   ├── 2.0.0.json.minisig
  │   └── 2.0.0.sbom.json                # 派生库存清单（审计用，客户端不读取）
  ├── artifacts/
  │   └── 2.0.0-to-2.1.0/
  │       ├── App.exe.tdp
  │       └── bin/core.dll.tdp
  └── blobs/
      └── <sha256>              # 内容寻址的全量回退对象（所有文件，含未变化）
```

**`manifests/` 前缀**只存放清单和签名。**`artifacts/` 前缀**存放差分补丁，**`blobs/` 前缀**存放按 SHA-256 内容寻址的全量对象。`latest.json` 是唯一被客户端直接请求的 URL（通过插件的 `endpoint` 配置），其余对象键由清单内部指定。

配置 `--prefix` 时，上述所有键（包括 `manifests/`、`artifacts/`、`blobs/` 以及清单内部的对象键）都会加上该前缀；客户端的 `endpoint` 必须指向 `<prefix>/manifests/latest.json`。

### 4.3 COS 生命周期与版本保留

COS 生命周期规则可以自动沉降或删除旧对象以控制存储成本。**但回滚素材不能被提前清理**——否则新版本出问题时，客户端没有旧版本的清单和补丁可回退。

推荐的生命周期策略：

| 对象类别                    | 保留策略                   | 理由                     |
| --------------------------- | -------------------------- | ------------------------ |
| `manifests/latest.json*`    | **永不自动删除**           | 客户端始终需要最新清单   |
| `manifests/<version>.json*` | 保留 **至少 3 个**最近版本 | 回滚和降级素材           |
| `artifacts/<from>-to-<to>/` | 保留 **至少 2 个**版本对   | 覆盖最近的更新路径       |
| `blobs/`                                 | 按内容哈希去重；至少保留最近 2–3 个版本的引用 | 差分失败、跨版本与损坏修复的全量来源 |

生命周期规则应基于对象前缀分别配置，**不要对同一组对象配置冲突条件的多条规则**。

### 4.4 预签名 URL 的约束

私有 COS 桶需要通过预签名 URL 分发。插件内置了 URL 过期自动刷新和断点续传，但有两个来自 COS 平台的硬约束必须遵守：

- **预签名 URL 不支持断点续传**：当指定预签名 URL 的有效时间过期后，请求会中断；申请新的签名后，需要**重新执行失败请求**，不支持在同一个 URL 上续传。插件的 Fetcher 在 URL 过期后会刷新 URL 并从当前字节偏移续传，但这依赖于 Fetcher 层重新发起 Range 请求，而非 COS 自身支持。
- **有效期受临时密钥限制**：使用临时密钥生成的预签名 URL，其过期时间由**临时密钥和预签名 URL 两个中较短者**决定。签名服务的临时密钥有效期应大于 URL 有效期。

**建议配置**：预签名 URL 有效期 ≤ 15 分钟，临时密钥有效期 ≥ 30 分钟。最小权限策略只授予 `GetObject`，资源范围限制在 `manifests/`、`artifacts/` 和 `blobs/` 前缀（`blobs/` 是差分回退、跨版本与损坏修复所需的全量对象来源）。


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

在正式上传 COS 之前，用 `--dry-run` 生成补丁并在内存中签名清单、检查体积，**不落盘、不上传**。适用于：

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

## 6.4 发布后审计

`verify --sample` 只抽样少量补丁，不回答“这个版本的增量策略是否真的生效”。`inspect --from-release` 补齐这一环：它验签版本化清单，对**每个** `delta_key`/`full_key` 做 `HEAD` 大小校验（不下载对象），并输出与 `release --verbose` 同构的逐文件表格和覆盖率汇总。

```bash
delta-update inspect --from-release cos://your-bucket \
  --version 2.1.0 \
  --public-key ./release.pub \
  --prefix releases
```

典型输出结尾：

```text
published 2.1.0: 128 files, delta bytes 12.4 MiB vs 210.8 MiB for patched files (saved 94.1%)
```

把它和发布时的输出对比，就能发现“清单已写但对象没传完”“上传后对象被覆盖”这类只靠 `verify --sample` 容易漏掉的问题：

```bash
delta-update release ... --verbose > before.txt
delta-update inspect --from-release ... > after.txt
diff before.txt after.txt
```

任何缺失或大小不符的对象会以警告列出，命令以退出码 `2` 失败，可直接作为 CI 门禁。

CDN 传播与多 mirror 的清单同步有延迟，`release` 刚结束时 `doctor` 可能还看到旧版本。发布后留一个观察窗口：

```bash
delta-update doctor --endpoint https://cdn.example.com/manifests/latest.json \
  --public-key ./release.pub --watch 600 --interval 60
```

任何一轮失败（签名、版本不一致、对象缺失）都会让命令失败；全部通过则输出 `watch finished`。

### 6.5 本地 HTTP 服务

`--bucket` 指向本地目录时，`release` 只写文件，不会起服务。用 `serve` 把发布目录托管给客户端、`doctor` 或 `verify`：

```bash
delta-update serve --dir ./channel --port 8123
# serving D:\work\channel at http://127.0.0.1:8123
```

- 默认只绑定 `127.0.0.1`；`--host` 可以改，但不要在没有 TLS/鉴权的情况下对外暴露。
- 支持 `GET` / `HEAD` 与单段 `Range`，够清单拉取、对象下载、`doctor` 与 `verify` 使用。
- 不提供目录列表、TLS、鉴权或缓存；这是本地演示与排障工具，不是生产 CDN。loopback 不是信任边界，不防御同机恶意进程；不要用 `socat` / `netsh portproxy` 转发到公网。


## 7. 回滚版本的发布

### 7.1 回滚发布的特殊性

正常发布中，客户端拒绝 `version < 本地版本` 的清单（`E_VERSION_ROLLBACK`）；`version == 本地版本` 视为无更新。**回滚发布必须显式覆盖此守卫**，否则客户端不会接受降级。

回滚发布有两种场景：

| 场景                                       | 触发方式                                                     | 客户端行为                       |
| ------------------------------------------ | ------------------------------------------------------------ | -------------------------------- |
| **紧急回滚**（新版本已发布但发现严重问题） | `delta-update rollback --to-version <旧版本>`（复用已有清单）或 `release --rollback`（重新构建） | 客户端接受降级，清除崩溃循环状态 |
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

### 7.3 一键复用已有版本：`delta-update rollback`

如果旧版本的清单和对象都还在桶里，事故响应不需要重新生成任何补丁——只需要让 `latest.json` 指向旧版本：

```bash
delta-update rollback \
  --bucket cos://your-bucket \
  --to-version 1.9.0 \
  --key /tmp/release.key
```

执行内容：

1. 读取 `manifests/1.9.0.json` 与 `.minisig`，用 `--key` 对应的公钥验签，确认是可信的旧清单。
2. 在清单中设 `rollback: true`，`published_at` 更新为当前时间；`version` 与文件条目保持原样。
3. 用 `--key` 重新签名，上传为 `manifests/latest.json` + `.minisig`。
4. **不修改** `manifests/1.9.0.json`，审计历史保持原状；`--dry-run` 只打印将要发布的内容。

与 `release --rollback` 的区别：`release --rollback` 会从 `--from` 到 `--to` 重新生成补丁和清单，语义是“发布一个版本号更低的新版本”；`rollback --to-version` 复用已发布的清单，只改变推荐版本，语义是“改变 latest 指向”。事故响应优先用后者，避免引入新的构建变量。

### 7.4 回滚发布的注意事项

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


