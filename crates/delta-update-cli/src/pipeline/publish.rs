use std::path::PathBuf;

use anyhow::{bail, Context};

use crate::pipeline::manifest::{latest_key, signature_key, version_key};
use crate::publisher::Publisher;

#[derive(Debug, Clone)]
pub enum ObjectBody {
    Bytes(Vec<u8>),
    File(PathBuf),
}

#[derive(Debug, Clone)]
pub struct ObjectUpload {
    pub key: String,
    pub body: ObjectBody,
    pub size: u64,
}

pub struct ManifestSet<'a> {
    pub prefix: &'a str,
    pub version: &'a str,
    pub manifest: &'a [u8],
    pub signature: &'a [u8],
}

/// Upload objects and confirm every one exists with the expected size. This
/// runs before any manifest is published.
pub fn upload_objects(publisher: &dyn Publisher, objects: &[ObjectUpload]) -> anyhow::Result<()> {
    for object in objects {
        match &object.body {
            ObjectBody::Bytes(bytes) => publisher.put(&object.key, bytes)?,
            ObjectBody::File(path) => publisher.put_file(&object.key, path)?,
        }
    }
    verify_objects(publisher, objects)
}

pub fn verify_objects(publisher: &dyn Publisher, objects: &[ObjectUpload]) -> anyhow::Result<()> {
    for object in objects {
        match publisher.head(&object.key)? {
            Some(size) if size == object.size => {}
            Some(size) => bail!(
                "object {} has size {size}, expected {}",
                object.key,
                object.size
            ),
            None => bail!("object {} is missing after upload", object.key),
        }
    }
    Ok(())
}

/// Publish in the only safe order: signatures, versioned manifest, latest
/// manifest last.
pub fn publish_manifests(publisher: &dyn Publisher, set: &ManifestSet<'_>) -> anyhow::Result<()> {
    let version_key = version_key(set.prefix, set.version);
    let version_signature_key = signature_key(&version_key);
    let latest_key = latest_key(set.prefix);
    let latest_signature_key = signature_key(&latest_key);

    publisher.put(&version_signature_key, set.signature)?;
    publisher.put(&latest_signature_key, set.signature)?;
    publisher.put(&version_key, set.manifest)?;
    publisher.put(&latest_key, set.manifest)?;

    for (key, expected) in [
        (version_signature_key.as_str(), set.signature.len() as u64),
        (latest_signature_key.as_str(), set.signature.len() as u64),
        (version_key.as_str(), set.manifest.len() as u64),
        (latest_key.as_str(), set.manifest.len() as u64),
    ] {
        match publisher.head(key)? {
            Some(size) if size == expected => {}
            Some(size) => bail!("{key} has size {size}, expected {expected}"),
            None => bail!("{key} is missing after upload"),
        }
    }
    Ok(())
}

/// Read the published manifest and signature for `version`.
pub fn fetch_manifest(
    publisher: &dyn Publisher,
    prefix: &str,
    version: &str,
) -> anyhow::Result<(Vec<u8>, String)> {
    let key = version_key(prefix, version);
    let manifest = publisher
        .get(&key)
        .with_context(|| format!("download {key}"))?;
    let signature = publisher
        .get(&signature_key(&key))
        .with_context(|| format!("download {}.minisig", key))?;
    let signature = String::from_utf8(signature).context("signature is not valid UTF-8")?;
    Ok((manifest, signature))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publisher::fs::FsPublisher;

    #[test]
    fn manifests_are_published_after_objects() {
        let dir = tempfile::tempdir().unwrap();
        let publisher = FsPublisher::new(dir.path());
        let objects = vec![ObjectUpload {
            key: "artifacts/full/1.0.0/a.txt".into(),
            body: ObjectBody::Bytes(b"a".to_vec()),
            size: 1,
        }];
        upload_objects(&publisher, &objects).unwrap();
        publish_manifests(
            &publisher,
            &ManifestSet {
                prefix: "",
                version: "1.0.0",
                manifest: b"{}",
                signature: b"sig",
            },
        )
        .unwrap();
        assert_eq!(
            publisher.get("manifests/latest.json").unwrap(),
            b"{}".to_vec()
        );
        assert_eq!(
            publisher.get("manifests/1.0.0.json.minisig").unwrap(),
            b"sig".to_vec()
        );
    }

    #[test]
    fn missing_objects_fail_before_manifest_publish() {
        let dir = tempfile::tempdir().unwrap();
        let publisher = FsPublisher::new(dir.path());
        let objects = vec![ObjectUpload {
            key: "artifacts/full/1.0.0/a.txt".into(),
            body: ObjectBody::Bytes(b"a".to_vec()),
            size: 2,
        }];
        assert!(upload_objects(&publisher, &objects).is_err());
    }
}
