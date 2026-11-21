//! The editor schema (`docs/reference/config.schema.json`) is hand-written next
//! to `PluginConfig`. These tests keep the two from drifting: every field must
//! appear, defaults must match the runtime defaults, and only the three truly
//! required keys may be listed as required.

use std::collections::BTreeSet;

use delta_update_engine::config::PluginConfig;

fn schema() -> serde_json::Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/reference/config.schema.json"
    );
    serde_json::from_str(
        &std::fs::read_to_string(path).unwrap_or_else(|error| panic!("read {path}: {error}")),
    )
    .unwrap_or_else(|error| panic!("parse {path}: {error}"))
}

fn keys(value: &serde_json::Value) -> BTreeSet<String> {
    value.as_object().expect("object").keys().cloned().collect()
}

#[test]
fn schema_covers_every_config_field() {
    let schema = schema();
    let properties = keys(&schema["properties"]);
    let defaults = serde_json::to_value(PluginConfig::default()).expect("serialize defaults");
    assert_eq!(
        properties,
        keys(&defaults),
        "config.schema.json properties must match PluginConfig"
    );

    let required: BTreeSet<String> = schema["required"]
        .as_array()
        .expect("required array")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_string)
        .collect();
    assert_eq!(
        required,
        BTreeSet::from([
            "config_version".to_string(),
            "endpoint".to_string(),
            "signing_public_keys".to_string(),
        ]),
        "only fields without a serde default may be required"
    );
    assert_eq!(
        schema["additionalProperties"],
        serde_json::Value::Bool(true),
        "unknown keys are ignored, and `$schema` must stay allowed"
    );
}

#[test]
fn schema_defaults_match_runtime_defaults() {
    let schema = schema();
    let defaults = serde_json::to_value(PluginConfig::default()).expect("serialize defaults");
    for (field, property) in schema["properties"].as_object().expect("properties") {
        let Some(default) = property.get("default") else {
            continue;
        };
        let runtime = defaults
            .get(field)
            .unwrap_or_else(|| panic!("PluginConfig has no field {field}"));
        assert_eq!(default, runtime, "default for {field} drifted");
    }
}
