//! Compile-time pinned trust root.
//!
//! `signing_public_keys` is embedded in the binary from `tauri.conf.json` at
//! build time, not read from a writable file at runtime. The remaining risk is
//! therefore a build that compiles in keys which do not match the released
//! client: a tampered config in the release pipeline, a stale branch, or a
//! half-finished rotation. Building with the `pinned-keys` feature narrows the
//! trust root at compile time:
//!
//! * pinned keys come from `DELTA_UPDATE_PINNED_PUBLIC_KEYS` (comma separated),
//!   injected by the release environment rather than by the reviewed file;
//! * the effective keys are `config ∩ pinned`;
//! * an empty intersection is a startup error, never a silent fallback.
//!
//! Rotation ordering matters because a refusal here means the application does
//! not start: ship the client whose pinned set already contains the new key
//! **before** the publisher signs with it (see `docs/reference/versioning.md`
//! §5). Without the feature this module is a pass-through.

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
            "no configured signing key is pinned (signing_public_keys ∩ DELTA_UPDATE_PINNED_PUBLIC_KEYS is empty). Rotate in order: first ship a client build whose pinned set contains the new key, then switch the publisher to that key; the application refuses to start until both sides agree".into(),
        ));
    }
    Ok(trusted)
}

/// Feature disabled: the embedded config stays the only trust root.
#[cfg(not(feature = "pinned-keys"))]
pub fn effective_keys(configured: &[String]) -> Result<Vec<String>, EngineError> {
    Ok(configured.to_vec())
}
