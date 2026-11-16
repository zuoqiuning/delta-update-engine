//! The trust root: embedded config narrowed at build time and at start-up.
//!
//! `signing_public_keys` is embedded in the binary from `tauri.conf.json` at
//! build time, not read from a writable file at runtime. The remaining risk is
//! therefore a build that compiles in keys which do not match the released
//! client: a tampered config in the release pipeline, a stale branch, or a
//! half-finished rotation. Two mechanisms narrow it further, newest last:
//!
//! * building with the `pinned-keys` feature: keys come from
//!   `DELTA_UPDATE_PINNED_PUBLIC_KEYS` (comma separated), injected by the
//!   release environment rather than by the reviewed file, and the effective
//!   set is `config ∩ pinned`; an empty intersection is a startup error.
//! * calling [`set_trusted_keys`] at the top of `main`: the host supplies the
//!   set it actually wants, either intersected with the embedded keys or as
//!   the only source.
//!
//! Rotation ordering matters because a refusal here means the application does
//! not start: ship the client whose pinned set already contains the new key
//! **before** the publisher signs with it (see `docs/reference/versioning.md`
//! §5).

use std::sync::OnceLock;

use delta_update_engine::EngineError;

/// How keys supplied at start-up combine with the embedded config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustMode {
    /// Trust only keys present in both the embedded config and the supplied
    /// list. The usual choice for a managed allowlist.
    Intersect,
    /// Trust exactly the supplied list and ignore the embedded
    /// `signing_public_keys`. For deployments that own the key set outside the
    /// application; it widens the trust root, so it is never the default.
    PinnedOnly,
}

static RUNTIME_KEYS: OnceLock<(Vec<String>, TrustMode)> = OnceLock::new();
static RESOLVED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Restrict this process to `keys`.
///
/// Call at the top of `main`, before the builder runs: the replacement worker
/// verifies the journal before the plugin exists, so a trust root set later
/// would apply to the GUI only. A worker narrower than the GUI would then fail
/// after the user had already restarted, so a late call is refused instead of
/// silently leaving two trust roots.
///
/// Returns an error for an empty key list, a second call, and any call after
/// the trust root has already been resolved.
pub fn set_trusted_keys(keys: Vec<String>, mode: TrustMode) -> Result<(), EngineError> {
    if keys.iter().all(|key| payload_line(key).is_empty()) {
        return Err(EngineError::ConfigInvalid(
            "set_trusted_keys was called without a usable key; an empty set would trust nothing"
                .into(),
        ));
    }
    if RESOLVED.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(EngineError::ConfigInvalid(
            "set_trusted_keys ran after the trust root was already resolved; call it at the top of main, before the builder starts".into(),
        ));
    }
    RUNTIME_KEYS.set((keys, mode)).map_err(|_| {
        EngineError::ConfigInvalid(
            "set_trusted_keys was called twice; the trust root is resolved once at start-up".into(),
        )
    })
}

/// Resolve the keys this process verifies against.
pub fn effective_keys(configured: &[String]) -> Result<Vec<String>, EngineError> {
    let base = compile_time_keys(configured)?;
    RESOLVED.store(true, std::sync::atomic::Ordering::Relaxed);
    match RUNTIME_KEYS.get() {
        Some((keys, mode)) => narrow(base, keys, *mode),
        None => Ok(base),
    }
}

fn narrow(base: Vec<String>, keys: &[String], mode: TrustMode) -> Result<Vec<String>, EngineError> {
    match mode {
        TrustMode::PinnedOnly => Ok(keys.to_vec()),
        TrustMode::Intersect => {
            let trusted: Vec<String> = base
                .into_iter()
                .filter(|key| {
                    let payload = payload_line(key);
                    keys.iter()
                        .any(|supplied| payload_line(supplied) == payload)
                })
                .collect();
            if trusted.is_empty() {
                return Err(EngineError::ConfigInvalid(
                    "no embedded signing key is in the keys passed to set_trusted_keys (the intersection is empty)".into(),
                ));
            }
            Ok(trusted)
        }
    }
}

/// Verification only reads the first non-comment line, so keys are compared by
/// that payload even when one side is a whole `.pub` file.
fn payload_line(key: &str) -> &str {
    key.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("untrusted comment:"))
        .unwrap_or("")
}

/// Narrow configured keys to the compile-time pinned set.
#[cfg(feature = "pinned-keys")]
fn compile_time_keys(configured: &[String]) -> Result<Vec<String>, EngineError> {
    let pinned: Vec<String> = option_env!("DELTA_UPDATE_PINNED_PUBLIC_KEYS")
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string)
        .collect();
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

/// Feature disabled: the embedded config is the only compile-time source.
#[cfg(not(feature = "pinned-keys"))]
fn compile_time_keys(configured: &[String]) -> Result<Vec<String>, EngineError> {
    Ok(configured.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn intersect_keeps_only_shared_keys() {
        let base = keys(&["A", "B"]);
        assert_eq!(
            narrow(base, &keys(&["B", "C"]), TrustMode::Intersect).unwrap(),
            keys(&["B"])
        );
    }

    #[test]
    fn intersect_compares_the_payload_line() {
        let base = keys(&["RWQ1"]);
        let supplied = keys(&["untrusted comment: deploy\nRWQ1\n"]);
        assert_eq!(
            narrow(base, &supplied, TrustMode::Intersect).unwrap(),
            keys(&["RWQ1"])
        );
    }

    #[test]
    fn intersect_refuses_an_empty_result() {
        let error = narrow(keys(&["A"]), &keys(&["B"]), TrustMode::Intersect).unwrap_err();
        assert_eq!(error.code(), "E_CONFIG_INVALID");
    }

    #[test]
    fn pinned_only_ignores_the_embedded_keys() {
        assert_eq!(
            narrow(keys(&["A"]), &keys(&["B"]), TrustMode::PinnedOnly).unwrap(),
            keys(&["B"])
        );
    }

    #[test]
    fn blank_keys_are_rejected() {
        assert!(set_trusted_keys(keys(&[""]), TrustMode::Intersect).is_err());
    }
}
