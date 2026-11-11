use delta_update_engine::{
    EngineError, ErrorAction, ErrorCategory, NetworkError, PatchError, SignatureError,
};

#[test]
fn network_variants_map_to_documented_codes() {
    let cases = [
        (NetworkError::Connect("x".into()), "E_NETWORK_CONNECT", true),
        (NetworkError::Timeout("x".into()), "E_NETWORK_TIMEOUT", true),
        (
            NetworkError::Http5xx {
                status: 503,
                detail: "x".into(),
            },
            "E_NETWORK_HTTP_5XX",
            true,
        ),
        (
            NetworkError::Http4xx {
                status: 404,
                detail: "x".into(),
            },
            "E_NETWORK_HTTP_4XX",
            false,
        ),
        (
            NetworkError::PresignExpired,
            "E_NETWORK_PRESIGN_EXPIRED",
            true,
        ),
        (
            NetworkError::RangeUnsupported,
            "E_NETWORK_RANGE_UNSUPPORTED",
            false,
        ),
        (
            NetworkError::SigningService("x".into()),
            "E_NETWORK_SIGNING_SERVICE",
            true,
        ),
        (
            NetworkError::RateLimited {
                retry_after_secs: Some(7),
            },
            "E_NETWORK_HTTP_4XX",
            true,
        ),
    ];

    for (variant, code, retryable) in cases {
        let error = EngineError::from(variant);
        assert_eq!(error.code(), code);
        assert_eq!(error.retryable(), retryable);
        assert_eq!(error.category(), ErrorCategory::Network);
    }
}

#[test]
fn signature_variants_map_to_documented_codes() {
    let cases = [
        (SignatureError::Invalid("x".into()), "E_SIGNATURE_INVALID"),
        (SignatureError::NoTrustedKey, "E_SIGNATURE_NO_TRUSTED_KEY"),
        (SignatureError::Missing, "E_SIGNATURE_SIG_MISSING"),
    ];

    for (variant, code) in cases {
        let error = EngineError::from(variant);
        assert_eq!(error.code(), code);
        assert!(!error.retryable());
        assert_eq!(error.category(), ErrorCategory::Signature);
    }
}

#[test]
fn patch_variants_map_to_documented_codes() {
    let cases = [
        (
            PatchError::SourceMismatch("x".into()),
            "E_PATCH_SOURCE_MISMATCH",
            true,
        ),
        (
            PatchError::TargetHashMismatch("x".into()),
            "E_PATCH_TARGET_HASH_MISMATCH",
            true,
        ),
        (
            PatchError::MemoryBudget {
                file_size: 10,
                budget: 5,
            },
            "E_PATCH_MEMORY_BUDGET",
            false,
        ),
        (
            PatchError::FormatMismatch("x".into()),
            "E_PATCH_FORMAT_MISMATCH",
            true,
        ),
        (
            PatchError::DecompressionLimit("x".into()),
            "E_PATCH_DECOMPRESSION_LIMIT",
            false,
        ),
        (PatchError::TooLarge("x".into()), "E_PATCH_TOO_LARGE", false),
        (
            PatchError::FullTooLarge("x".into()),
            "E_FULL_TOO_LARGE",
            false,
        ),
        (
            PatchError::UnknownStrategy("x".into()),
            "E_UNKNOWN_STRATEGY",
            false,
        ),
    ];

    for (variant, code, retryable) in cases {
        let error = EngineError::from(variant);
        assert_eq!(error.code(), code);
        assert_eq!(error.retryable(), retryable);
        assert_eq!(error.category(), ErrorCategory::Patch);
    }
}

#[test]
fn other_variants_keep_stable_codes() {
    let cases = [
        (
            EngineError::StagingLimit { limit: 1 },
            "E_DISK_STAGING_LIMIT",
        ),
        (EngineError::UacCancelled, "E_PERMISSION_UAC_CANCELLED"),
        (
            EngineError::InvalidTransition {
                from: "Idle".into(),
                to: "Prepared".into(),
            },
            "E_INVALID_TRANSITION",
        ),
        (EngineError::ManifestInvalid("x".into()), "E_MANIFEST_PARSE"),
        (EngineError::UpdateLocked, "E_UPDATE_LOCKED"),
    ];

    for (error, code) in cases {
        assert_eq!(error.code(), code);
    }
}

#[test]
fn manifest_errors_point_at_the_right_fix() {
    let cases = [
        (
            EngineError::ManifestInvalid("x".into()),
            ErrorAction::ContactSupport,
        ),
        (
            EngineError::ManifestTooLarge("x".into()),
            ErrorAction::ContactSupport,
        ),
        (
            EngineError::ManifestSchemaTooNew {
                client: 1,
                required: 2,
            },
            ErrorAction::UpdateClient,
        ),
        (
            EngineError::VersionInvalid("x".into()),
            ErrorAction::ContactSupport,
        ),
        (
            EngineError::VersionRollback {
                remote: "1.0.0".into(),
                local: "2.0.0".into(),
            },
            ErrorAction::CheckUpdate,
        ),
    ];

    for (error, action) in cases {
        assert_eq!(error.to_payload().action, action, "{error}");
    }
}

#[test]
fn rate_limited_carries_a_capped_retry_delay() {
    let error = EngineError::from(NetworkError::RateLimited {
        retry_after_secs: Some(3600),
    });
    assert!(error.retryable());
    assert_eq!(
        error.retry_after(),
        Some(std::time::Duration::from_secs(60))
    );

    let without_header = EngineError::from(NetworkError::RateLimited {
        retry_after_secs: None,
    });
    assert_eq!(without_header.retry_after(), None);
}

#[test]
fn transient_errors_have_actionable_user_text() {
    let locked = EngineError::UpdateLocked.to_payload();
    assert_eq!(locked.action, ErrorAction::Wait);
    assert!(
        locked.user_action.contains("实例"),
        "{}",
        locked.user_action
    );

    let cancelled = EngineError::Cancelled.to_payload();
    assert_eq!(cancelled.action, ErrorAction::Retry);
    assert!(
        cancelled.user_action.contains("取消"),
        "{}",
        cancelled.user_action
    );

    let not_prepared = EngineError::NotPrepared.to_payload();
    assert_eq!(not_prepared.action, ErrorAction::CheckUpdate);
    assert!(
        not_prepared.user_action.contains("检查更新"),
        "{}",
        not_prepared.user_action
    );
}

#[test]
fn payload_carries_code_and_category() {
    let payload = EngineError::from(NetworkError::Timeout("slow".into())).to_payload();
    assert_eq!(payload.code, "E_NETWORK_TIMEOUT");
    assert_eq!(payload.category, ErrorCategory::Network);
    assert!(payload.retryable);
    assert_eq!(payload.action, delta_update_engine::ErrorAction::Retry);
}
