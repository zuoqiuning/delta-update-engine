//! Rust ↔ TypeScript contract for the wire types.
//!
//! The SDK's `types.ts` is hand-written next to `StatePayload` and
//! `ErrorPayload`. This test reads both sides and fails when a state name,
//! interface field or event name drifts, which is the failure mode a
//! hand-maintained second copy always has.

use std::collections::BTreeSet;

use delta_update_engine::{
    EngineError, EngineState, ErrorAction, ErrorCategory, ErrorPayload, ProgressPayload,
    ProgressUnit, StatePayload, UpdateRecord, UpdateResult,
};

fn types_source() -> String {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../packages/plugin-api/src/types.ts"
    );
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("read {path}: {error}"))
}

/// Text between `start` (exclusive) and the next `end`.
fn block<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let from = source
        .find(start)
        .unwrap_or_else(|| panic!("missing {start}"))
        + start.len();
    let rest = &source[from..];
    let to = rest
        .find(end)
        .unwrap_or_else(|| panic!("missing {end} after {start}"));
    &rest[..to]
}

/// Field names of an exported interface, ignoring comments and methods.
fn interface_fields(source: &str, name: &str) -> BTreeSet<String> {
    let start = format!("export interface {name} {{");
    block(source, &start, "}")
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with('/') || line.starts_with('*') || !line.ends_with(';') {
                return None;
            }
            line.split_once(':')
                .map(|(field, _)| field.trim().trim_end_matches('?').to_string())
        })
        .collect()
}

fn serialized_fields<T: serde::Serialize>(value: &T) -> BTreeSet<String> {
    serde_json::to_value(value)
        .expect("serialize")
        .as_object()
        .expect("object")
        .keys()
        .cloned()
        .collect()
}

#[test]
fn state_names_match_the_typescript_sdk() {
    let source = types_source();
    let names = block(&source, "export const STATE_NAMES = [", "] as const;");
    let ts: Vec<&str> = names.split('\'').skip(1).step_by(2).collect();

    let rust = [
        EngineState::Idle,
        EngineState::UpdateRequired {
            version: "2.0.0".into(),
            min_supported_version: "1.5.0".into(),
        },
        EngineState::Checking,
        EngineState::Downloading {
            total: 1,
            completed: 0,
        },
        EngineState::Preparing {
            verified: 0,
            total: 1,
        },
        EngineState::Prepared {
            version: "2.0.0".into(),
        },
        EngineState::Cancelled {
            version: "2.0.0".into(),
        },
        EngineState::Committing {
            processed: 0,
            total: 0,
        },
        EngineState::Committed {
            version: "2.0.0".into(),
        },
        EngineState::RollingBack {
            to_version: "1.0.0".into(),
            reason: "crash loop".into(),
        },
        EngineState::RolledBack {
            to_version: "1.0.0".into(),
            reason: "crash loop".into(),
        },
        EngineState::Failed {
            category: "internal".into(),
            error: EngineError::Cancelled.to_payload(),
        },
    ];
    let rust: Vec<&str> = rust.iter().map(EngineState::name).collect();
    assert_eq!(
        ts, rust,
        "STATE_NAMES must list every engine state, in order"
    );
}

#[test]
fn event_name_matches_the_typescript_sdk() {
    let source = types_source();
    let line = block(&source, "export const STATE_EVENT = ", ";\n");
    assert_eq!(
        line.trim(),
        format!("'{}'", tauri_plugin_delta_update::STATE_EVENT)
    );
}

#[test]
fn error_payload_fields_match_the_typescript_sdk() {
    let source = types_source();
    let ts = interface_fields(&source, "UpdateError");
    let payload = EngineError::Network(delta_update_engine::NetworkError::Timeout("slow".into()))
        .to_payload();
    assert_eq!(ts, serialized_fields(&payload), "UpdateError fields");
}

#[test]
fn state_payload_fields_match_the_typescript_sdk() {
    let source = types_source();
    let ts = interface_fields(&source, "UpdateStatePayload");
    let mut payload = StatePayload::from_state(&EngineState::Downloading {
        total: 10,
        completed: 2,
    });
    payload.progress = Some(ProgressPayload {
        total: 10,
        completed: 2,
        file: Some("App.exe".into()),
        unit: Some(ProgressUnit::Bytes),
    });
    payload.target_version = Some("2.0.0".into());
    payload.skipped_version = Some("1.9.0".into());
    let rust = serialized_fields(&payload);

    // Every emitted field must exist in the SDK, and every required SDK field
    // must be emitted. Optional SDK fields may legitimately be absent.
    for field in &rust {
        assert!(ts.contains(field), "UpdateStatePayload is missing {field}");
    }
    for required in ["v", "state", "version", "progress", "error", "timestamp"] {
        assert!(
            rust.contains(required),
            "payload must always emit {required}"
        );
    }
}

#[test]
fn progress_payload_fields_match_the_typescript_sdk() {
    let source = types_source();
    let ts = interface_fields(&source, "UpdateProgress");
    let progress = ProgressPayload {
        total: 10,
        completed: 2,
        file: Some("App.exe".into()),
        unit: Some(ProgressUnit::Ops),
    };
    assert_eq!(ts, serialized_fields(&progress), "UpdateProgress fields");
}

#[test]
fn check_result_fields_match_the_typescript_sdk() {
    let source = types_source();
    let ts = interface_fields(&source, "CheckResult");
    let result = tauri_plugin_delta_update::commands::CheckResult {
        available: false,
        version: None,
        current_version: "1.0.0".into(),
        files_changed: 0,
        files_staged: 0,
        download_bytes: 0,
        total_bytes: 0,
        notes: None,
        mandatory: false,
        update_required: false,
        min_supported_version: None,
        cloud_placeholders_skipped: 0,
        skipped_version: None,
    };
    assert_eq!(ts, serialized_fields(&result), "CheckResult fields");
}

#[test]
fn update_record_fields_match_the_typescript_sdk() {
    let source = types_source();
    let ts = interface_fields(&source, "UpdateRecord");
    // Every optional field is set so `skip_serializing_if` cannot hide one.
    let record = UpdateRecord {
        v: 1,
        version: "2.0.0".into(),
        from_version: Some("1.9.0".into()),
        started_at_unix: Some(1),
        finished_at_unix: 2,
        result: UpdateResult::Completed,
        duration_ms: Some(1_000),
        reason: Some("crash loop".into()),
        error_code: Some("E_INTERNAL".into()),
    };
    assert_eq!(ts, serialized_fields(&record), "UpdateRecord fields");
    for result in [
        UpdateResult::Completed,
        UpdateResult::RolledBack,
        UpdateResult::Failed,
    ] {
        let wire = serde_json::to_value(result).unwrap();
        let text = wire.as_str().expect("result serializes as a string");
        assert!(
            source.contains(&format!("'{text}'")),
            "UpdateRecord result union is missing {text}"
        );
    }
}

/// The error categories/actions on the wire are lowercase strings; the SDK
/// types them as unions, so every value must appear there verbatim.
#[test]
fn error_categories_and_actions_match_the_sdk_unions() {
    let source = types_source();
    for category in [
        ErrorCategory::Network,
        ErrorCategory::Signature,
        ErrorCategory::Manifest,
        ErrorCategory::Security,
        ErrorCategory::Disk,
        ErrorCategory::Permission,
        ErrorCategory::Patch,
        ErrorCategory::Journal,
        ErrorCategory::Rollback,
        ErrorCategory::Config,
        ErrorCategory::Platform,
        ErrorCategory::Apply,
        ErrorCategory::Internal,
    ] {
        let wire = serde_json::to_value(category).unwrap();
        let text = wire.as_str().expect("category serializes as a string");
        assert_eq!(text, text.to_ascii_lowercase(), "{text} must be lowercase");
        assert!(
            source.contains(&format!("'{text}'")),
            "ErrorCategory union is missing {text}"
        );
    }
    for action in [
        ErrorAction::Retry,
        ErrorAction::Wait,
        ErrorAction::CheckUpdate,
        ErrorAction::ContactSupport,
        ErrorAction::GrantPermission,
        ErrorAction::FreeSpace,
        ErrorAction::UpdateClient,
        ErrorAction::ReportSecurity,
        ErrorAction::RecoverBackup,
        ErrorAction::FixConfig,
        ErrorAction::Unsupported,
        ErrorAction::Restart,
    ] {
        let wire = serde_json::to_value(action).unwrap();
        let text = wire.as_str().expect("action serializes as a string");
        assert!(
            source.contains(&format!("'{text}'")),
            "ErrorAction union is missing {text}"
        );
    }
    let sample: ErrorPayload = EngineError::Cancelled.to_payload();
    assert_eq!(
        serde_json::to_value(sample.action).unwrap(),
        serde_json::Value::String("retry".into())
    );
}
