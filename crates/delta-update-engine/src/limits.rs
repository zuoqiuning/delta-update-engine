/// Hard limits that protect the client from a malicious or broken server.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Limits {
    pub manifest_max_bytes: u64,
    pub max_files: usize,
    pub max_patch_bytes: u64,
    pub max_full_bytes: u64,
    pub max_patch_file_size: u64,
    pub patch_memory_factor: f32,
    pub staging_size_limit: u64,
    pub max_concurrent_downloads: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            manifest_max_bytes: 8 * 1024 * 1024,
            max_files: 20_000,
            max_patch_bytes: 1024 * 1024 * 1024,
            max_full_bytes: 4 * 1024 * 1024 * 1024,
            max_patch_file_size: 256 * 1024 * 1024,
            patch_memory_factor: 2.5,
            staging_size_limit: 2 * 1024 * 1024 * 1024,
            max_concurrent_downloads: 4,
        }
    }
}
