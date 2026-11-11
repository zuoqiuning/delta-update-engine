use tauri_plugin_delta_update::Config;

#[test]
fn config_uses_snake_case_and_defaults() {
    let config: Config = serde_json::from_str(
        r#"{
            "config_version": 1,
            "endpoint": "https://example.com/manifests/latest.json",
            "signing_public_keys": ["RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3"],
            "auto_check": true
        }"#,
    )
    .unwrap();

    assert!(config.auto_apply);
    assert!(config.auto_confirm);
    assert_eq!(config.channel, "stable");
    assert_eq!(config.crash_loop_threshold, 2);
    assert_eq!(config.check_interval_secs, None);
    assert!(config.validate().is_ok());
}

#[test]
fn empty_proxy_is_rejected() {
    let config: Config = serde_json::from_str(
        r#"{
            "config_version": 1,
            "endpoint": "https://example.com/latest.json",
            "signing_public_keys": ["RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3"],
            "proxy": "  "
        }"#,
    )
    .unwrap();

    assert_eq!(config.validate().unwrap_err().code(), "E_CONFIG_INVALID");
}

#[test]
fn check_interval_requires_a_sane_minimum() {
    let parse = |interval: u64| -> Config {
        serde_json::from_str(&format!(
            r#"{{
                "config_version": 1,
                "endpoint": "https://example.com/manifests/latest.json",
                "signing_public_keys": ["RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3"],
                "check_interval_secs": {interval}
            }}"#
        ))
        .unwrap()
    };

    assert!(parse(59).validate().is_err());
    assert!(parse(60).validate().is_ok());
}

#[test]
fn config_rejects_non_https_endpoint() {
    let config: Config = serde_json::from_str(
        r#"{
            "config_version": 1,
            "endpoint": "http://example.com/latest.json",
            "signing_public_keys": ["RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3"]
        }"#,
    )
    .unwrap();

    assert!(config.validate().is_err());
}

#[test]
fn insecure_http_is_limited_to_loopback() {
    let parse = |endpoint: &str| -> Config {
        serde_json::from_str(&format!(
            r#"{{
                "config_version": 1,
                "endpoint": "{endpoint}",
                "signing_public_keys": ["RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3"],
                "allow_insecure_http": true
            }}"#
        ))
        .unwrap()
    };

    assert!(parse("http://127.0.0.1:8080/latest.json")
        .validate()
        .is_ok());
    assert!(parse("http://localhost:8080/latest.json")
        .validate()
        .is_ok());
    assert!(parse("http://example.com/latest.json").validate().is_err());
}

#[test]
fn presigned_endpoint_requires_signing_service() {
    let config: Config = serde_json::from_str(
        r#"{
            "config_version": 1,
            "endpoint": "https://example.com/latest.json?sign=abc",
            "signing_public_keys": ["RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3"]
        }"#,
    )
    .unwrap();

    assert!(config.validate().is_err());
}

#[test]
fn invalid_public_key_is_rejected() {
    let config: Config = serde_json::from_str(
        r#"{
            "config_version": 1,
            "endpoint": "https://example.com/latest.json",
            "signing_public_keys": ["not-a-minisign-key"]
        }"#,
    )
    .unwrap();

    assert_eq!(config.validate().unwrap_err().code(), "E_CONFIG_INVALID");
}

#[test]
fn newer_config_version_is_rejected() {
    let config: Config = serde_json::from_str(
        r#"{
            "config_version": 2,
            "endpoint": "https://example.com/latest.json",
            "signing_public_keys": ["RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3"]
        }"#,
    )
    .unwrap();

    assert_eq!(config.validate().unwrap_err().code(), "E_CONFIG_INVALID");
}

#[test]
fn unknown_fields_are_ignored_without_failing_validation() {
    let config: Config = serde_json::from_str(
        r#"{
            "config_version": 1,
            "endpoint": "https://example.com/latest.json",
            "signing_public_keys": ["RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3"],
            "future_option": { "enabled": true },
            "key_rotation": ["RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3"]
        }"#,
    )
    .unwrap();

    assert!(config.validate().is_ok());
}

/// P-05: the default permission set covers all six commands.
#[test]
fn default_permissions_cover_all_commands() {
    let text = include_str!("../permissions/default.toml");
    for command in [
        "check",
        "prepare",
        "cancel-prepare",
        "status",
        "confirm-healthy",
        "restart-to-apply",
    ] {
        assert!(
            text.contains(&format!("\"allow-{command}\"")),
            "missing allow-{command}"
        );
    }
}
