//! Contract for the `pinned-keys` feature: the trust root must be the
//! intersection of the runtime config and the keys compiled into the binary.
//!
//! Run the enabled half with a key list:
//!
//! ```text
//! DELTA_UPDATE_PINNED_PUBLIC_KEYS=RWQ... cargo test -p tauri-plugin-delta-update \
//!     --features pinned-keys --test pinned_keys_contract
//! ```

#[cfg(not(feature = "pinned-keys"))]
#[test]
fn without_the_feature_configured_keys_pass_through() {
    let configured = vec![
        "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3".to_string(),
        "RWJ4p2n0R8Xv6tVY3lq3fPw5m8M2H9cK1sT7bD4gE6aQ".to_string(),
    ];
    assert_eq!(
        tauri_plugin_delta_update::pinned::effective_keys(&configured).unwrap(),
        configured
    );
}

#[cfg(feature = "pinned-keys")]
mod enabled {
    use tauri_plugin_delta_update::pinned::effective_keys;

    fn pinned() -> Vec<String> {
        option_env!("DELTA_UPDATE_PINNED_PUBLIC_KEYS")
            .expect("build.rs requires DELTA_UPDATE_PINNED_PUBLIC_KEYS")
            .split(',')
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn keeps_configured_keys_that_are_pinned() {
        let pinned = pinned();
        assert_eq!(effective_keys(&pinned).unwrap(), pinned);
    }

    #[test]
    fn drops_configured_keys_that_are_not_pinned() {
        let pinned = pinned();
        let mut configured = pinned.clone();
        configured.push("RWQnotpinned000000000000000000000000000000000000000000".into());
        assert_eq!(effective_keys(&configured).unwrap(), pinned);
    }

    #[test]
    fn refuses_an_empty_intersection() {
        let error = effective_keys(&["RWQnotpinned00000000000000000000000000000000000000".into()])
            .unwrap_err();
        assert_eq!(error.code(), "E_CONFIG_INVALID");
        assert!(error.to_string().contains("pinned"), "{error}");
    }

    #[test]
    fn matches_whole_pub_files_by_payload_line() {
        let pinned = pinned();
        let as_file = format!("untrusted comment: test key\n{}\n", pinned[0]);
        let effective = effective_keys(&[as_file]).unwrap();
        assert_eq!(effective.len(), 1);
        assert!(effective[0].contains(&pinned[0]));
    }
}
