use semver::Version;

use crate::error::EngineError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum VersionDecision {
    Update,
    NoUpdate,
    Rollback,
}

/// Decide whether a manifest version may be applied.
///
/// Equal versions are "no update", not an error. Lower versions are only
/// accepted when the signed manifest explicitly carries `rollback: true`.
pub fn decide(
    remote: &str,
    local: &str,
    rollback_allowed: bool,
) -> Result<VersionDecision, EngineError> {
    let remote =
        Version::parse(remote).map_err(|_| EngineError::VersionInvalid(remote.to_string()))?;
    let local =
        Version::parse(local).map_err(|_| EngineError::VersionInvalid(local.to_string()))?;

    match remote.cmp(&local) {
        std::cmp::Ordering::Greater => Ok(VersionDecision::Update),
        std::cmp::Ordering::Equal => Ok(VersionDecision::NoUpdate),
        std::cmp::Ordering::Less if rollback_allowed => Ok(VersionDecision::Rollback),
        std::cmp::Ordering::Less => Err(EngineError::VersionRollback {
            remote: remote.to_string(),
            local: local.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_version_is_no_update() {
        assert_eq!(
            decide("2.1.0", "2.1.0", false).unwrap(),
            VersionDecision::NoUpdate
        );
    }

    #[test]
    fn lower_version_requires_rollback_flag() {
        assert!(matches!(
            decide("2.0.0", "2.1.0", false),
            Err(EngineError::VersionRollback { .. })
        ));
        assert_eq!(
            decide("2.0.0", "2.1.0", true).unwrap(),
            VersionDecision::Rollback
        );
    }

    #[test]
    fn prerelease_is_lower_than_release() {
        assert_eq!(
            decide("2.0.0", "2.0.0-rc.1", true).unwrap(),
            VersionDecision::Update
        );
    }
}
