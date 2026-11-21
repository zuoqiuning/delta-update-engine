//! `init --write` embeds an editor schema in the CLI binary. Keep that copy
//! byte-identical to the documented one, so onboarding never hands users a
//! stale schema.

#[test]
fn embedded_schema_matches_the_documented_schema() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let asset = std::fs::read(format!("{manifest_dir}/assets/config.schema.json"))
        .expect("read assets/config.schema.json");
    let docs = std::fs::read(format!(
        "{manifest_dir}/../../docs/reference/config.schema.json"
    ))
    .expect("read docs/reference/config.schema.json");
    assert!(
        asset == docs,
        "assets/config.schema.json drifted from docs/reference/config.schema.json"
    );
}
