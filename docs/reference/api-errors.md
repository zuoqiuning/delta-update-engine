# 错误与错误码

> 本文件是 [API 参考](api.md) 的错误部分：`EngineError` 枚举、wire payload 与错误码映射。

## 4. 错误类型

### 4.1 引擎错误枚举

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EngineError {
    #[error("network error: {0}")]
    Network(#[from] NetworkError),

    #[error("signature verification failed: {0}")]
    Signature(#[from] SignatureError),

    #[error("manifest schema too new: client supports {client}, manifest requires {required}")]
    ManifestSchemaTooNew { client: u32, required: u32 },

    #[error("version rollback rejected: remote {remote}, local {local}")]
    VersionRollback { remote: String, local: String },

    #[error("invalid version: {0}")]
    VersionInvalid(String),

    #[error("path escape detected: {0}")]
    PathEscape(String),

    #[error("invalid path: {0}")]
    PathInvalid(String),

    #[error("path collision: {0}")]
    PathCollision(String),

    #[error("manifest is invalid: {0}")]
    ManifestInvalid(String),

    #[error("manifest too large: {0}")]
    ManifestTooLarge(String),

    #[error("insufficient disk space: required {required}, available {available}")]
    InsufficientDiskSpace { required: u64, available: u64 },

    #[error("staging area exceeds the configured limit of {limit} bytes")]
    StagingLimit { limit: u64 },

    #[error("permission denied: {0}")]
    PermissionDenied(String),

    #[error("user cancelled the elevation prompt")]
    UacCancelled,

    #[error("patch failed: {0}")]
    PatchFailed(#[from] PatchError),

    #[error("journal corrupt: {0}")]
    JournalCorrupt(String),

    #[error("journal format {actual} is newer than this client supports ({client})")]
    JournalFormatTooNew { client: u16, actual: u16 },

    #[error("journal replay failed: {0}")]
    JournalReplayFailed(String),

    #[error("update locked by another process")]
    UpdateLocked,

    #[error("rollback failed: {0}")]
    RollbackFailed(String),

    #[error("invalid config: {0}")]
    ConfigInvalid(String),

    #[error("config migration required: from {from} to {to}")]
    ConfigMigrationRequired { from: u32, to: u32 },

    #[error("insecure endpoint: {0}")]
    EndpointInsecure(String),

    #[error("unsupported platform: {0}")]
    UnsupportedPlatform(String),

    #[error("unsupported install format: {0}")]
    UnsupportedFormat(String),

    #[error("apply helper failed: {0}")]
    ApplyHelperFailed(String),

    #[error("elevated recovery did not finish within {timeout_ms} ms; the worker may still be running")]
    ElevatedRecoveryTimeout { timeout_ms: u64 },

    #[error("not prepared")]
    NotPrepared,

    #[error("invalid state transition: {from} -> {to}")]
    InvalidTransition { from: String, to: String },

    #[error("cancelled")]
    Cancelled,

    #[error("internal error: {0}")]
    Internal(String),
}
```

遵循 Rust 生态惯例：**库 crate 使用 `thiserror` 定义具体枚举，调用方可匹配变体**。标记 `#[non_exhaustive]` 使新增变体不破坏 SemVer。**不在此枚举中使用 `anyhow`**——`anyhow` 仅用于 CLI 的顶层错误聚合。

三个子枚举持有细分码值，`EngineError` 只负责分组与转发：

```rust
pub enum NetworkError {
    Connect(String), Timeout(String),
    Http5xx { status: u16, detail: String },
    Http4xx { status: u16, detail: String },
    PresignExpired, RangeUnsupported, SigningService(String),
}

pub enum SignatureError { Invalid(String), NoTrustedKey, Missing }

pub enum PatchError {
    SourceMismatch(String), ApplyFailed(String), TargetHashMismatch(String),
    MemoryBudget { file_size: u64, budget: u64 }, FormatMismatch(String),
    DecompressionLimit(String), TooLarge(String), FullTooLarge(String),
    UnknownStrategy(String),
}
```

### 4.2 错误码映射

引擎错误变体与协议文档 §7.3 的字符串码值有一一映射。**`EngineError::code()` 是码值的唯一来源**；`NetworkError`、`SignatureError`、`PatchError` 持有细分码值，`EngineError` 只做转发。插件层把结果复制进 `ErrorPayload`，不维护第二份映射表。

| `EngineError` 变体                            | 协议码值示例                   |
| --------------------------------------------- | ------------------------------ |
| `Network(NetworkError::Timeout)`              | `E_NETWORK_TIMEOUT`            |
| `Signature(SignatureError::NoTrustedKey)`     | `E_SIGNATURE_NO_TRUSTED_KEY`   |
| `VersionRollback { .. }`                      | `E_VERSION_ROLLBACK`           |
| `VersionInvalid(_)`                           | `E_VERSION_INVALID`            |
| `PathEscape(_)`                               | `E_PATH_ESCAPE`                |
| `InsufficientDiskSpace { .. }`                | `E_DISK_INSUFFICIENT`          |
| `StagingLimit { .. }`                         | `E_DISK_STAGING_LIMIT`         |
| `PermissionDenied(_)`                         | `E_PERMISSION_DENIED`          |
| `UacCancelled`                                | `E_PERMISSION_UAC_CANCELLED`   |
| `PatchFailed(PatchError::TargetHashMismatch)` | `E_PATCH_TARGET_HASH_MISMATCH` |
| `JournalCorrupt(_)`                           | `E_JOURNAL_CORRUPT`            |
| `JournalFormatTooNew { .. }`                  | `E_JOURNAL_FORMAT_TOO_NEW`     |
| `UpdateLocked`                                | `E_UPDATE_LOCKED`              |
| `RollbackFailed(_)`                           | `E_ROLLBACK_FAILED`            |
| `PathInvalid(_)`                              | `E_PATH_INVALID`               |
| `PathCollision(_)`                            | `E_PATH_COLLISION`             |
| `ManifestTooLarge(_)`                         | `E_MANIFEST_TOO_LARGE`         |
| `ConfigInvalid(_)`                            | `E_CONFIG_INVALID`             |
| `ConfigMigrationRequired { .. }`              | `E_CONFIG_MIGRATION_REQUIRED`  |
| `EndpointInsecure(_)`                         | `E_ENDPOINT_INSECURE`          |
| `UnsupportedPlatform(_)`                      | `E_UNSUPPORTED_PLATFORM`       |
| `UnsupportedFormat(_)`                        | `E_UNSUPPORTED_FORMAT`         |
| `ApplyHelperFailed(_)`                        | `E_APPLY_HELPER_FAILED`        |
| `ElevatedRecoveryTimeout { .. }`              | `E_APPLY_RECOVERY_TIMEOUT`     |
| `NotPrepared`                                 | `E_NOT_PREPARED`               |
| `InvalidTransition { .. }`                    | `E_INVALID_TRANSITION`         |
| `Cancelled`                                   | `E_CANCELLED`                  |
| `Internal(_)`                                 | `E_INTERNAL`                   |

映射表是**唯一**的码值来源，由 `EngineError::code()` 实现；插件层只负责把 `ErrorPayload` 原样序列化给前端。

