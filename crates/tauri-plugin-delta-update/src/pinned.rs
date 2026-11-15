//! Compile-time pinned trust root.
//!
//! In a per-user install the configuration file is writable by the same user
//! who runs the app, so `signing_public_keys` alone cannot stop a modified
//! config from trusting an attacker's key. Building with the `pinned-keys`
//! feature narrows the trust root at compile time:
//!
//! * pinned keys come from `DELTA_UPDATE_PINNED_PUBLIC_KEYS` (comma separated);
//! * the effective keys are `config ∩ pinned`;
//! * an empty intersection is a startup error, never a silent fallback.
//!
//! Without the feature this module is a pass-through, so behaviour is
//! unchanged for hosts that do not opt in.

use delta_update_engine::EngineError;

/// Keys trusted at build time.
#[cfg(feature = "pinned-keys")]
fn pinned_keys() -> Vec<String> {
    option_env!("DELTA_UPDATE_PINNED_PUBLIC_KEYS")
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string)
        .collect()
}

/// Verification only reads the first non-comment line, so the intersection
/// compares the same payload even when a configured key is a whole `.pub` file.
#[cfg(feature = "pinned-keys")]
fn payload_line(key: &str) -> &str {
    key.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("untrusted comment:"))
        .unwrap_or("")
}

/// Narrow configured keys to the compile-time pinned set.
#[cfg(feature = "pinned-keys")]
pub fn effective_keys(configured: &[String]) -> Result<Vec<String>, EngineError> {
    let pinned = pinned_keys();
    if pinned.is_empty() {
        return Err(EngineError::ConfigInvalid(
            "pinned-keys is enabled but DELTA_UPDATE_PINNED_PUBLIC_KEYS was empty at build time; rebuild with the comma-separated key list".into(),
        ));
    }
    let trusted: Vec<String> = configured
        .iter()
        .filter(|key| {
            let payload = payload_line(key);
            pinned.iter().any(|pin| pin == payload)
        })
        .cloned()
        .collect();
    if trusted.is_empty() {
        return Err(EngineError::ConfigInvalid(
            "no configured signing key is pinned (signing_public_keys ∩ DELTA_UPDATE_PINNED_PUBLIC_KEYS is empty); point the config at a pinned key or rebuild with the new key".into(),
        ));
    }
    Ok(trusted)
}

/// Feature disabled: the runtime config stays the only trust root.
#[cfg(not(feature = "pinned-keys"))]
pub fn effective_keys(configured: &[String]) -> Result<Vec<String>, EngineError> {
    Ok(configured.to_vec())
}
