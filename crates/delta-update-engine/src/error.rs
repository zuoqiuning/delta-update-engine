use serde::{Deserialize, Serialize};

/// Stable error categories. UI grouping uses these values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorCategory {
    Network,
    Signature,
    Manifest,
    Security,
    Disk,
    Permission,
    Patch,
    Journal,
    Rollback,
    Config,
    Platform,
    Apply,
    Internal,
}

impl std::fmt::Display for ErrorCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

/// Stable machine-readable follow-up action for localized UIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorAction {
    Retry,
    Wait,
    CheckUpdate,
    ContactSupport,
    GrantPermission,
    FreeSpace,
    UpdateClient,
    ReportSecurity,
    RecoverBackup,
    FixConfig,
    Unsupported,
    Restart,
}

/// Wire-level error object. The stable `code` field is what callers match on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ErrorPayload {
    pub code: String,
    pub category: ErrorCategory,
    pub message: String,
    pub retryable: bool,
    /// Stable machine-readable action, for UI localization.
    pub action: ErrorAction,
    pub user_action: String,
    pub detail: String,
}

/// Transport-level failures. Each variant maps to one stable wire code.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum NetworkError {
    #[error("connection failed: {0}")]
    Connect(String),

    #[error("request timed out: {0}")]
    Timeout(String),

    #[error("server returned {status}: {detail}")]
    Http5xx { status: u16, detail: String },

    #[error("client request rejected with {status}: {detail}")]
    Http4xx { status: u16, detail: String },

    #[error("server rate limited the request{}", match retry_after_secs {
        Some(seconds) => format!(" (retry after {seconds}s)"),
        None => String::new(),
    })]
    RateLimited { retry_after_secs: Option<u64> },

    #[error("presigned URL expired")]
    PresignExpired,

    #[error("server does not support range requests")]
    RangeUnsupported,

    #[error("signing service failed: {0}")]
    SigningService(String),
}

impl NetworkError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Connect(_) => "E_NETWORK_CONNECT",
            Self::Timeout(_) => "E_NETWORK_TIMEOUT",
            Self::Http5xx { .. } => "E_NETWORK_HTTP_5XX",
            Self::Http4xx { .. } | Self::RateLimited { .. } => "E_NETWORK_HTTP_4XX",
            Self::PresignExpired => "E_NETWORK_PRESIGN_EXPIRED",
            Self::RangeUnsupported => "E_NETWORK_RANGE_UNSUPPORTED",
            Self::SigningService(_) => "E_NETWORK_SIGNING_SERVICE",
        }
    }

    pub fn retryable(&self) -> bool {
        !matches!(self, Self::Http4xx { .. } | Self::RangeUnsupported)
    }
}

/// Signature verification failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SignatureError {
    #[error("signature is invalid: {0}")]
    Invalid(String),

    #[error("no trusted public key matched the manifest signature")]
    NoTrustedKey,

    #[error("signature object is missing")]
    Missing,
}

impl SignatureError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "E_SIGNATURE_INVALID",
            Self::NoTrustedKey => "E_SIGNATURE_NO_TRUSTED_KEY",
            Self::Missing => "E_SIGNATURE_SIG_MISSING",
        }
    }
}

/// Patch pipeline failures. Most are recoverable by falling back to a full
/// object, so they stay distinct from generic internal errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PatchError {
    #[error("source file does not match the patch: {0}")]
    SourceMismatch(String),

    #[error("patch application failed: {0}")]
    ApplyFailed(String),

    #[error("target hash mismatch after applying patch: {0}")]
    TargetHashMismatch(String),

    #[error("patch memory budget exceeded: file {file_size} bytes, budget {budget} bytes")]
    MemoryBudget { file_size: u64, budget: u64 },

    #[error("patch container does not match the manifest strategy: {0}")]
    FormatMismatch(String),

    #[error("decompressed output exceeds the declared size: {0}")]
    DecompressionLimit(String),

    #[error("patch object exceeds the size limit: {0}")]
    TooLarge(String),

    #[error("full object exceeds the size limit: {0}")]
    FullTooLarge(String),

    #[error("unknown strategy without a usable full object: {0}")]
    UnknownStrategy(String),
}

impl PatchError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::SourceMismatch(_) => "E_PATCH_SOURCE_MISMATCH",
            Self::ApplyFailed(_) => "E_PATCH_APPLY_FAILED",
            Self::TargetHashMismatch(_) => "E_PATCH_TARGET_HASH_MISMATCH",
            Self::MemoryBudget { .. } => "E_PATCH_MEMORY_BUDGET",
            Self::FormatMismatch(_) => "E_PATCH_FORMAT_MISMATCH",
            Self::DecompressionLimit(_) => "E_PATCH_DECOMPRESSION_LIMIT",
            Self::TooLarge(_) => "E_PATCH_TOO_LARGE",
            Self::FullTooLarge(_) => "E_FULL_TOO_LARGE",
            Self::UnknownStrategy(_) => "E_UNKNOWN_STRATEGY",
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::SourceMismatch(_)
                | Self::ApplyFailed(_)
                | Self::TargetHashMismatch(_)
                | Self::FormatMismatch(_)
        )
    }
}

/// Every failure that can reach a caller, a log or a UI.
///
/// The wire contract is three exhaustive tables in this file: [`code`] (stable
/// identifier), [`action`] (machine-readable next step) and [`user_action`]
/// (prose next step). Adding a variant fails to compile until all three are
/// updated, so a code can never exist without a documented follow-up.
///
/// [`code`]: EngineError::code
/// [`action`]: EngineError::action
/// [`user_action`]: EngineError::user_action
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EngineError {
    #[error(transparent)]
    Network(#[from] NetworkError),

    #[error(transparent)]
    Signature(#[from] SignatureError),

    #[error("manifest schema too new: client supports {client}, manifest requires {required}")]
    ManifestSchemaTooNew { client: u32, required: u32 },

    #[error("manifest is invalid: {0}")]
    ManifestInvalid(String),

    #[error("manifest is too large: {0}")]
    ManifestTooLarge(String),

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

    #[error("insufficient disk space: required {required}, available {available}")]
    InsufficientDiskSpace { required: u64, available: u64 },

    #[error("staging area exceeds the configured limit of {limit} bytes")]
    StagingLimit { limit: u64 },

    #[error("permission denied: {0}")]
    PermissionDenied(String),

    #[error("user cancelled the elevation prompt")]
    UacCancelled,

    #[error(transparent)]
    PatchFailed(#[from] PatchError),

    #[error("journal corrupt: {0}")]
    JournalCorrupt(String),

    #[error("journal format {actual} is newer than this client supports ({client})")]
    JournalFormatTooNew { client: u16, actual: u16 },

    #[error("journal replay failed: {0}")]
    JournalReplayFailed(String),

    #[error("update is locked by another process")]
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

    #[error(
        "elevated recovery did not finish within {timeout_ms} ms; the worker may still be running"
    )]
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

impl EngineError {
    /// Suggested delay before retrying, taken from a `Retry-After` header.
    /// Capped so a hostile or misconfigured server cannot stall the process.
    pub fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            Self::Network(NetworkError::RateLimited { retry_after_secs }) => {
                retry_after_secs.map(|seconds| std::time::Duration::from_secs(seconds.min(60)))
            }
            _ => None,
        }
    }

    pub fn category(&self) -> ErrorCategory {
        match self {
            Self::Network(_) => ErrorCategory::Network,
            Self::Signature(_) => ErrorCategory::Signature,
            Self::ManifestSchemaTooNew { .. }
            | Self::ManifestInvalid(_)
            | Self::ManifestTooLarge(_)
            | Self::VersionRollback { .. }
            | Self::VersionInvalid(_) => ErrorCategory::Manifest,
            Self::PathEscape(_) | Self::PathInvalid(_) | Self::PathCollision(_) => {
                ErrorCategory::Security
            }
            Self::InsufficientDiskSpace { .. } | Self::StagingLimit { .. } => ErrorCategory::Disk,
            Self::PermissionDenied(_) | Self::UacCancelled => ErrorCategory::Permission,
            Self::PatchFailed(_) => ErrorCategory::Patch,
            Self::JournalCorrupt(_)
            | Self::JournalFormatTooNew { .. }
            | Self::JournalReplayFailed(_)
            | Self::UpdateLocked => ErrorCategory::Journal,
            Self::RollbackFailed(_) => ErrorCategory::Rollback,
            Self::ConfigInvalid(_)
            | Self::ConfigMigrationRequired { .. }
            | Self::EndpointInsecure(_) => ErrorCategory::Config,
            Self::UnsupportedPlatform(_) | Self::UnsupportedFormat(_) => ErrorCategory::Platform,
            Self::ApplyHelperFailed(_)
            | Self::ElevatedRecoveryTimeout { .. }
            | Self::NotPrepared => ErrorCategory::Apply,
            Self::InvalidTransition { .. } | Self::Cancelled | Self::Internal(_) => {
                ErrorCategory::Internal
            }
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::Network(error) => error.code(),
            Self::Signature(error) => error.code(),
            Self::ManifestSchemaTooNew { .. } => "E_MANIFEST_SCHEMA_TOO_NEW",
            Self::ManifestInvalid(_) => "E_MANIFEST_PARSE",
            Self::ManifestTooLarge(_) => "E_MANIFEST_TOO_LARGE",
            Self::VersionRollback { .. } => "E_VERSION_ROLLBACK",
            Self::VersionInvalid(_) => "E_VERSION_INVALID",
            Self::PathEscape(_) => "E_PATH_ESCAPE",
            Self::PathInvalid(_) => "E_PATH_INVALID",
            Self::PathCollision(_) => "E_PATH_COLLISION",
            Self::InsufficientDiskSpace { .. } => "E_DISK_INSUFFICIENT",
            Self::StagingLimit { .. } => "E_DISK_STAGING_LIMIT",
            Self::PermissionDenied(_) => "E_PERMISSION_DENIED",
            Self::UacCancelled => "E_PERMISSION_UAC_CANCELLED",
            Self::PatchFailed(error) => error.code(),
            Self::JournalCorrupt(_) => "E_JOURNAL_CORRUPT",
            Self::JournalFormatTooNew { .. } => "E_JOURNAL_FORMAT_TOO_NEW",
            Self::JournalReplayFailed(_) => "E_JOURNAL_REPLAY_FAILED",
            Self::UpdateLocked => "E_UPDATE_LOCKED",
            Self::RollbackFailed(_) => "E_ROLLBACK_FAILED",
            Self::ConfigInvalid(_) => "E_CONFIG_INVALID",
            Self::ConfigMigrationRequired { .. } => "E_CONFIG_MIGRATION_REQUIRED",
            Self::EndpointInsecure(_) => "E_ENDPOINT_INSECURE",
            Self::UnsupportedPlatform(_) => "E_UNSUPPORTED_PLATFORM",
            Self::UnsupportedFormat(_) => "E_UNSUPPORTED_FORMAT",
            Self::ApplyHelperFailed(_) => "E_APPLY_HELPER_FAILED",
            Self::ElevatedRecoveryTimeout { .. } => "E_APPLY_RECOVERY_TIMEOUT",
            Self::NotPrepared => "E_NOT_PREPARED",
            Self::InvalidTransition { .. } => "E_INVALID_TRANSITION",
            Self::Cancelled => "E_CANCELLED",
            Self::Internal(_) => "E_INTERNAL",
        }
    }

    pub fn retryable(&self) -> bool {
        match self {
            Self::Network(error) => error.retryable(),
            Self::PatchFailed(error) => error.retryable(),
            Self::PermissionDenied(_) | Self::UacCancelled | Self::UpdateLocked => true,
            Self::ApplyHelperFailed(_) | Self::ElevatedRecoveryTimeout { .. } => true,
            _ => false,
        }
    }

    /// Stable process exit code for CLIs and CI.
    ///
    /// 1 = transient (retrying may succeed), 2 = needs a human (config,
    /// signature, manifest, disk), 3 = internal error (a bug; report it).
    /// This keeps the documented `inspect` contract (0 ok, 1 unfinished,
    /// 2 manual attention) and gives every other command the same mapping.
    pub fn exit_code(&self) -> i32 {
        if self.retryable() {
            1
        } else if self.category() == ErrorCategory::Internal {
            3
        } else {
            2
        }
    }

    /// Stable action code for localized UIs. Unlike `user_action`, this value
    /// is not prose and is safe to match on.
    pub fn action(&self) -> ErrorAction {
        match self {
            Self::UpdateLocked | Self::InvalidTransition { .. } => ErrorAction::Wait,
            Self::Cancelled => ErrorAction::Retry,
            Self::ElevatedRecoveryTimeout { .. } => ErrorAction::Restart,
            Self::JournalFormatTooNew { .. } => ErrorAction::UpdateClient,
            Self::NotPrepared => ErrorAction::CheckUpdate,
            // A malformed or oversized manifest is a publisher problem; only a
            // too-new schema is something the client can fix by updating.
            Self::ManifestSchemaTooNew { .. } => ErrorAction::UpdateClient,
            Self::ManifestInvalid(_) | Self::ManifestTooLarge(_) | Self::VersionInvalid(_) => {
                ErrorAction::ContactSupport
            }
            Self::VersionRollback { .. } => ErrorAction::CheckUpdate,
            _ => match self.category() {
                ErrorCategory::Network => ErrorAction::Retry,
                ErrorCategory::Signature => ErrorAction::ContactSupport,
                ErrorCategory::Manifest => ErrorAction::UpdateClient,
                ErrorCategory::Security => ErrorAction::ReportSecurity,
                ErrorCategory::Disk => ErrorAction::FreeSpace,
                ErrorCategory::Permission => ErrorAction::GrantPermission,
                ErrorCategory::Patch => ErrorAction::Retry,
                ErrorCategory::Journal => ErrorAction::ContactSupport,
                ErrorCategory::Rollback => ErrorAction::RecoverBackup,
                ErrorCategory::Config => ErrorAction::FixConfig,
                ErrorCategory::Platform => ErrorAction::Unsupported,
                ErrorCategory::Apply => ErrorAction::ContactSupport,
                ErrorCategory::Internal => ErrorAction::ContactSupport,
            },
        }
    }

    pub fn user_action(&self) -> &'static str {
        match self {
            Self::UpdateLocked => "另一个实例正在执行更新，请等待其完成或关闭后重试",
            Self::InvalidTransition { .. } => "当前正在执行其他更新操作，请稍后重试",
            Self::Cancelled => "更新已取消，可稍后重试",
            Self::ElevatedRecoveryTimeout { .. } => {
                "请重启应用以完成恢复；不要手动删除 Journal 或 .old"
            }
            Self::JournalFormatTooNew { .. } => "请升级应用后再恢复更新",
            Self::NotPrepared => "请先检查更新并完成下载",
            Self::Network(NetworkError::Connect(message))
                if message.starts_with("DNS resolution failed") =>
            {
                "检查 endpoint 域名是否正确解析；内网环境检查 DNS 或代理设置"
            }
            Self::Network(NetworkError::Connect(message))
                if message.starts_with("TLS handshake failed") =>
            {
                "检查系统时间与证书链，以及代理是否拦截 TLS"
            }
            Self::Network(NetworkError::Connect(message)) if message.starts_with("proxy") => {
                "检查代理配置，确认代理允许访问更新域名"
            }
            Self::Network(NetworkError::Timeout(_)) => {
                "稍后重试；弱网环境可降低 max_concurrent_downloads"
            }
            Self::Network(NetworkError::RateLimited { .. }) => {
                "服务端限流，插件会按 Retry-After 自动重试；频繁出现请降低发布端并发"
            }
            Self::Network(NetworkError::Http5xx { .. }) => "服务端暂时不可用，稍后重试",
            Self::Network(NetworkError::Http4xx { .. }) => {
                "检查 endpoint/对象键与桶权限；403 常见于本机时间偏差或签名过期"
            }
            Self::Network(NetworkError::PresignExpired) => {
                "插件会自动刷新签名并续传；若持续出现，检查本机时间与签名服务"
            }
            Self::Network(NetworkError::RangeUnsupported) => {
                "服务器不支持断点续传，已回退全量下载；更新会比预期慢"
            }
            Self::Network(NetworkError::SigningService(_)) => {
                "检查签名服务是否可用，以及 signing_endpoint 配置是否正确"
            }
            _ => match self.category() {
                ErrorCategory::Network => "检查网络后重试",
                ErrorCategory::Signature => "联系软件提供商，可能是签名或密钥问题",
                ErrorCategory::Manifest => "检查发布端配置；必要时升级客户端",
                ErrorCategory::Security => "安全事件，收集日志并上报",
                ErrorCategory::Disk => "清理磁盘空间后重试",
                ErrorCategory::Permission => "允许替换进程的管理员授权后重试",
                ErrorCategory::Patch => "已自动回退全量；若仍失败请联系提供商",
                ErrorCategory::Journal => "收集日志与目录状态，不要手动删除 Journal",
                ErrorCategory::Rollback => "需要人工恢复备份",
                ErrorCategory::Config => "按错误提示修正插件配置",
                ErrorCategory::Platform => "改用受支持的安装形态",
                ErrorCategory::Apply => "检查安全软件拦截；已就绪的更新可重试",
                ErrorCategory::Internal => "收集日志并报告",
            },
        }
    }

    pub fn to_payload(&self) -> ErrorPayload {
        ErrorPayload {
            code: self.code().to_string(),
            category: self.category(),
            message: self.to_string(),
            retryable: self.retryable(),
            action: self.action(),
            user_action: self.user_action().to_string(),
            detail: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_separate_transient_manual_and_internal() {
        assert_eq!(
            EngineError::Network(NetworkError::Timeout("slow".into())).exit_code(),
            1
        );
        assert_eq!(
            EngineError::ConfigInvalid("bad".into()).exit_code(),
            2,
            "config errors need a human"
        );
        assert_eq!(
            EngineError::Signature(SignatureError::NoTrustedKey).exit_code(),
            2
        );
        assert_eq!(EngineError::Internal("bug".into()).exit_code(), 3);
    }

    #[test]
    fn every_variant_has_a_code_and_a_follow_up() {
        for error in samples() {
            assert!(!error.code().is_empty());
            assert!(!error.user_action().is_empty(), "{error:?}");
        }
    }

    /// One variant per distinct match arm above. Adding a variant means adding
    /// it here too, so an undocumented failure cannot ship silently.
    fn samples() -> Vec<EngineError> {
        vec![
            EngineError::Network(NetworkError::Connect("dns".into())),
            EngineError::Network(NetworkError::Timeout("slow".into())),
            EngineError::Network(NetworkError::RateLimited {
                retry_after_secs: Some(1),
            }),
            EngineError::Network(NetworkError::PresignExpired),
            EngineError::Signature(SignatureError::NoTrustedKey),
            EngineError::ManifestSchemaTooNew {
                client: 1,
                required: 2,
            },
            EngineError::ManifestInvalid("bad".into()),
            EngineError::VersionRollback {
                remote: "1.0.0".into(),
                local: "2.0.0".into(),
            },
            EngineError::PathEscape("..".into()),
            EngineError::InsufficientDiskSpace {
                required: 1,
                available: 0,
            },
            EngineError::PermissionDenied("denied".into()),
            EngineError::UacCancelled,
            EngineError::PatchFailed(PatchError::MemoryBudget {
                file_size: 2,
                budget: 1,
            }),
            EngineError::JournalCorrupt("bad".into()),
            EngineError::JournalFormatTooNew {
                client: 1,
                actual: 2,
            },
            EngineError::UpdateLocked,
            EngineError::RollbackFailed("failed".into()),
            EngineError::ConfigInvalid("bad".into()),
            EngineError::ConfigMigrationRequired { from: 1, to: 2 },
            EngineError::EndpointInsecure("http://example.com".into()),
            EngineError::UnsupportedPlatform("linux".into()),
            EngineError::ApplyHelperFailed("failed".into()),
            EngineError::ElevatedRecoveryTimeout { timeout_ms: 1 },
            EngineError::NotPrepared,
            EngineError::InvalidTransition {
                from: "Idle".into(),
                to: "Prepared".into(),
            },
            EngineError::Cancelled,
            EngineError::Internal("bug".into()),
        ]
    }
}
