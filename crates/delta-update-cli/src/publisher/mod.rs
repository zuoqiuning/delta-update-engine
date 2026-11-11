pub mod cos;
pub mod cos_signer;
pub mod fs;
pub mod s3;
pub mod s3_signer;

use std::path::{Path, PathBuf};

use anyhow::Context;

pub trait Publisher: Send + Sync {
    fn put(&self, key: &str, bytes: &[u8]) -> anyhow::Result<()>;
    fn put_file(&self, key: &str, path: &Path) -> anyhow::Result<()> {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        self.put(key, &bytes)
    }
    fn head(&self, key: &str) -> anyhow::Result<Option<u64>>;
    fn get(&self, key: &str) -> anyhow::Result<Vec<u8>>;
    /// Fetch an object, returning `None` when it does not exist.
    fn get_optional(&self, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        match self.get(key) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) => Err(error),
        }
    }
}

/// Open a publisher from `--bucket`.
///
/// `cos://bucket` uses the COS REST API, `s3://bucket` uses S3 Signature V4
/// (AWS S3, Cloudflare R2, MinIO, S3-compatible OSS), and any other value is
/// treated as a local directory (`file://` is accepted) so the pipeline can be
/// exercised offline.
pub fn open(bucket: &str, region: Option<&str>) -> anyhow::Result<Box<dyn Publisher>> {
    if let Some(bucket_name) = bucket.strip_prefix("cos://") {
        let region = region
            .map(str::to_string)
            .or_else(|| std::env::var("COS_REGION").ok())
            .filter(|value| !value.trim().is_empty())
            .context("COS region is required (--region or COS_REGION)")?;
        let secret_id = std::env::var("COS_SECRET_ID")
            .context("COS_SECRET_ID is required for cos:// publishing")?;
        let secret_key = std::env::var("COS_SECRET_KEY")
            .context("COS_SECRET_KEY is required for cos:// publishing")?;
        let token = std::env::var("COS_SESSION_TOKEN").ok();
        return Ok(Box::new(cos::CosPublisher::new(
            bucket_name,
            region,
            secret_id,
            secret_key,
            token,
        )?));
    }
    if let Some(bucket_name) = bucket.strip_prefix("s3://") {
        let region = region
            .map(str::to_string)
            .or_else(|| std::env::var("S3_REGION").ok())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "us-east-1".to_string());
        let access_key = std::env::var("S3_ACCESS_KEY_ID")
            .or_else(|_| std::env::var("AWS_ACCESS_KEY_ID"))
            .context("S3_ACCESS_KEY_ID (or AWS_ACCESS_KEY_ID) is required for s3:// publishing")?;
        let secret_key = std::env::var("S3_SECRET_ACCESS_KEY")
            .or_else(|_| std::env::var("AWS_SECRET_ACCESS_KEY"))
            .context(
                "S3_SECRET_ACCESS_KEY (or AWS_SECRET_ACCESS_KEY) is required for s3:// publishing",
            )?;
        let token = std::env::var("S3_SESSION_TOKEN")
            .or_else(|_| std::env::var("AWS_SESSION_TOKEN"))
            .ok();
        let endpoint = std::env::var("S3_ENDPOINT").ok();
        return Ok(Box::new(s3::S3Publisher::new(
            bucket_name,
            region,
            endpoint.as_deref(),
            access_key,
            secret_key,
            token,
        )?));
    }
    Ok(Box::new(fs::FsPublisher::new(local_path(bucket)?)))
}

fn local_path(bucket: &str) -> anyhow::Result<PathBuf> {
    if bucket.starts_with("file://") {
        let url = url::Url::parse(bucket).context("parse file:// bucket")?;
        url.to_file_path()
            .map_err(|_| anyhow::anyhow!("invalid file:// bucket: {bucket}"))
    } else {
        Ok(PathBuf::from(bucket))
    }
}
