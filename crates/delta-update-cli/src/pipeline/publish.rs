use std::path::PathBuf;

use anyhow::{bail, Context};

use crate::pipeline::manifest::{latest_key, sbom_key, signature_key, version_key};
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
/// runs before any manifest is published. Transient object-store failures are
/// retried; a release can be re-run safely because keys are content-addressed
/// or version-scoped.
pub fn upload_objects(publisher: &dyn Publisher, objects: &[ObjectUpload]) -> anyhow::Result<()> {
    for object in objects {
        put_with_retry(publisher, object)?;
    }
    verify_objects(publisher, objects)
}

fn put_with_retry(publisher: &dyn Publisher, object: &ObjectUpload) -> anyhow::Result<()> {
    let mut last = None;
    for attempt in 0..3 {
        let result = match &object.body {
            ObjectBody::Bytes(bytes) => publisher.put(&object.key, bytes),
            ObjectBody::File(path) => publisher.put_file(&object.key, path),
        };
        match result {
            Ok(()) => return Ok(()),
            Err(error) => {
                last = Some(error);
                if attempt < 2 {
                    std::thread::sleep(std::time::Duration::from_millis(200 << attempt));
                }
            }
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("upload failed without an error")))
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

/// Publish the advisory SBOM after the signed manifest. Clients never read it,
/// so a failure here fails the release for compliance but cannot affect
/// update delivery.
pub fn publish_sbom(
    publisher: &dyn Publisher,
    prefix: &str,
    version: &str,
    sbom: &[u8],
) -> anyhow::Result<String> {
    let key = sbom_key(prefix, version);
    publisher.put(&key, sbom)?;
    match publisher.head(&key)? {
        Some(size) if size == sbom.len() as u64 => Ok(key),
        Some(size) => bail!("{key} has size {size}, expected {}", sbom.len()),
        None => bail!("{key} is missing after upload"),
    }
}

/// Publish only `latest.json` and its signature, leaving the versioned manifest
/// untouched. Used by `rollback --to-version`, which must keep the original
/// release history auditable.
pub fn publish_latest(
    publisher: &dyn Publisher,
    prefix: &str,
    manifest: &[u8],
    signature: &[u8],
) -> anyhow::Result<String> {
    let latest_key = latest_key(prefix);
    let latest_signature_key = signature_key(&latest_key);
    publisher.put(&latest_signature_key, signature)?;
    publisher.put(&latest_key, manifest)?;
    for (key, expected) in [
        (latest_signature_key.as_str(), signature.len() as u64),
        (latest_key.as_str(), manifest.len() as u64),
    ] {
        match publisher.head(key)? {
            Some(size) if size == expected => {}
            Some(size) => bail!("{key} has size {size}, expected {expected}"),
            None => bail!("{key} is missing after upload"),
        }
    }
    Ok(latest_key)
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
