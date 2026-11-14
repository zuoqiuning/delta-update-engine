use delta_update_engine::EngineError;

/// The file-level replacement plugin is intentionally Windows-only.
///
/// macOS signed bundles and Linux package-manager/AppImage installs have
/// different integrity models (code signing, read-only mounts, package
/// ownership). Pretending to update them with a generic file replacement would
/// be worse than refusing the update.
pub fn ensure_supported() -> Result<(), EngineError> {
    #[cfg(target_os = "windows")]
    {
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err(EngineError::UnsupportedPlatform(
            "delta-update supports Windows desktop only; use the platform installer/package manager elsewhere"
                .into(),
        ))
    }
}
