use delta_update_engine::manifest::model::Strategy;
use delta_update_engine::path::validate_rel_path;
use delta_update_engine::{EngineError, Strategy as ReexportedStrategy};

#[test]
fn rejects_traversal_and_absolute_paths() {
    assert!(matches!(
        validate_rel_path("../evil.dll"),
        Err(EngineError::PathEscape(_))
    ));
    assert!(matches!(
        validate_rel_path("/etc/passwd"),
        Err(EngineError::PathInvalid(_))
    ));
    assert!(matches!(
        validate_rel_path("C:\\evil.dll"),
        Err(EngineError::PathInvalid(_))
    ));
}

#[test]
fn rejects_reserved_windows_names() {
    for path in ["CON", "NUL.txt", "bin/COM1", "lpt9.log"] {
        assert!(
            matches!(validate_rel_path(path), Err(EngineError::PathInvalid(_))),
            "{path} should be rejected"
        );
    }
}

#[test]
fn rejects_windows_drive_and_illegal_characters() {
    for path in ["C:/evil.dll", "bin/a?b.dll", "bin/a:b.dll", "bin/a*b.dll"] {
        assert!(
            matches!(validate_rel_path(path), Err(EngineError::PathInvalid(_))),
            "{path} should be rejected"
        );
    }
}

#[test]
fn manifest_rejects_uppercase_hashes() {
    let body = serde_json::json!({
        "schema": 1,
        "version": "2.1.0",
        "min_client_schema": 1,
        "files": {
            "App.exe": {
                "sha256": "A".repeat(64),
                "size": 1,
                "strategy": "full",
                "full_key": "artifacts/full/2.1.0/App.exe"
            }
        }
    });
    let error = delta_update_engine::manifest::parse_manifest(
        body.to_string().as_bytes(),
        &delta_update_engine::Limits::default(),
    )
    .unwrap_err();
    assert_eq!(error.code(), "E_MANIFEST_PARSE");
}

#[test]
fn accepts_normal_relative_paths() {
    assert!(validate_rel_path("bin/core.dll").is_ok());
    assert!(validate_rel_path("resources/icon.png").is_ok());
}

#[test]
fn unknown_strategy_is_forward_compatible() {
    let strategy: Strategy = serde_json::from_str("\"future+codec\"").unwrap();
    assert!(matches!(strategy, Strategy::Unknown(ref value) if value == "future+codec"));
    assert_eq!(
        serde_json::to_string(&strategy).unwrap(),
        "\"future+codec\""
    );
    let _: ReexportedStrategy = Strategy::Full;
}
