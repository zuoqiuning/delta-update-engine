//! The start-up trust root is process-wide on purpose: the replacement worker
//! verifies the journal before the plugin exists, so a key set that only
//! reached the GUI would leave the two halves of one update disagreeing.

use tauri_plugin_delta_update::{pinned, set_trusted_keys, TrustMode};

fn embedded_keys() -> Vec<String> {
    #[cfg(feature = "pinned-keys")]
    {
        option_env!("DELTA_UPDATE_PINNED_PUBLIC_KEYS")
            .expect("build.rs requires DELTA_UPDATE_PINNED_PUBLIC_KEYS")
            .split(',')
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(str::to_string)
            .collect()
    }
    #[cfg(not(feature = "pinned-keys"))]
    {
        vec!["RWQembedded".into(), "RWQrotating".into()]
    }
}

#[test]
fn runtime_keys_narrow_the_embedded_config_for_the_whole_process() {
    let embedded = embedded_keys();
    // The first embedded key is also pinned when the feature is on, so the
    // narrowed set has the same shape in both builds.
    let kept = embedded[0].clone();
    set_trusted_keys(vec![kept.clone()], TrustMode::Intersect).unwrap();
    assert_eq!(pinned::effective_keys(&embedded).unwrap(), vec![kept]);

    // The trust root is frozen by the first resolution: a late call cannot
    // silently leave the GUI and the worker with different key sets.
    let late = set_trusted_keys(vec!["RWQother".into()], TrustMode::PinnedOnly).unwrap_err();
    assert!(
        late.to_string().contains("resolved"),
        "a late call must say the trust root is already resolved: {late}"
    );
}
