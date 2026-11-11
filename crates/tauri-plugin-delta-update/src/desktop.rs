use delta_update_engine::EngineError;

/// Desktop platforms that can replace files in a user-writable install root.
pub fn ensure_supported() -> Result<(), EngineError> {
    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    {
        Ok(())
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        Err(EngineError::UnsupportedPlatform(
            "file-level replacement is only supported on desktop platforms".into(),
        ))
    }
}
