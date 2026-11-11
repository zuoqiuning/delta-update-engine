use std::path::Path;

/// Current byte length of an interrupted download, or zero.
pub async fn resume_offset(path: &Path) -> u64 {
    tokio::fs::metadata(path)
        .await
        .map(|meta| meta.len())
        .unwrap_or(0)
}
