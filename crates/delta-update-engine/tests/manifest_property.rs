use delta_update_engine::manifest::parse_manifest;
use delta_update_engine::Limits;
use proptest::prelude::*;

proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(raw in prop::collection::vec(any::<u8>(), 0..2048)) {
        let _ = parse_manifest(&raw, &Limits::default());
    }

    #[test]
    fn accepts_well_formed_manifest(version in "(0|[1-9][0-9]{0,2})\\.(0|[1-9][0-9]{0,2})\\.(0|[1-9][0-9]{0,2})", size in 0u64..4096, sha in "[0-9a-f]{64}") {
        let body = serde_json::json!({
            "schema": 1,
            "version": version,
            "min_client_schema": 1,
            "files": {
                "App.exe": {
                    "sha256": sha,
                    "size": size,
                    "strategy": "full",
                    "full_key": format!("artifacts/full/{version}/App.exe")
                }
            }
        });
        let manifest = parse_manifest(body.to_string().as_bytes(), &Limits::default())
            .expect("well-formed manifest should parse");
        prop_assert_eq!(manifest.version, version);
    }
}
