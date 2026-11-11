use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::bail;

use crate::publisher::Publisher;

pub struct FsPublisher {
    root: PathBuf,
}

impl FsPublisher {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path(&self, key: &str) -> anyhow::Result<PathBuf> {
        validate_key(key)?;
        Ok(self.root.join(key))
    }
}

impl Publisher for FsPublisher {
    fn put(&self, key: &str, bytes: &[u8]) -> anyhow::Result<()> {
        let path = self.path(key)?;
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("object key has no parent: {key}"))?;
        std::fs::create_dir_all(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(bytes)?;
        temp.as_file().sync_all()?;
        temp.persist(&path)
            .map_err(|error| error.error)
            .map(|_| ())?;
        Ok(())
    }

    fn put_file(&self, key: &str, source: &Path) -> anyhow::Result<()> {
        let path = self.path(key)?;
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("object key has no parent: {key}"))?;
        std::fs::create_dir_all(parent)?;
        let mut input = std::fs::File::open(source)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        std::io::copy(&mut input, &mut temp)?;
        temp.as_file().sync_all()?;
        temp.persist(&path)
            .map_err(|error| error.error)
            .map(|_| ())?;
        Ok(())
    }

    fn head(&self, key: &str) -> anyhow::Result<Option<u64>> {
        match std::fs::metadata(self.path(key)?) {
            Ok(meta) => Ok(Some(meta.len())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    fn get(&self, key: &str) -> anyhow::Result<Vec<u8>> {
        Ok(std::fs::read(self.path(key)?)?)
    }

    fn get_optional(&self, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        match std::fs::read(self.path(key)?) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }
}

fn validate_key(key: &str) -> anyhow::Result<()> {
    if key.is_empty()
        || key.starts_with('/')
        || key.starts_with('\\')
        || key.contains('\\')
        || key
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        bail!("invalid object key: {key}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_and_absolute_keys() {
        let publisher = FsPublisher::new(std::env::temp_dir());
        for key in ["../evil", "/etc/passwd", "a/../../b", "a//b", "a\\b"] {
            assert!(
                publisher.put(key, b"x").is_err(),
                "{key} should be rejected"
            );
        }
    }

    #[test]
    fn accepts_nested_relative_keys() {
        let publisher = FsPublisher::new(std::env::temp_dir());
        assert!(publisher.path("artifacts/full/1.2.3/App.exe").is_ok());
    }
}
