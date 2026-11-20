use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use anyhow::{bail, Context};

use crate::pipeline::manifest::{envelope_key, latest_key, sbom_key, signature_key, version_key};
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
    /// Extra detached signatures keyed by minisign key id (`Vec<u8>` is the
    /// `.minisig` file body). Published before the manifest so a declared
    /// attachment is never missing while the release is visible.
    pub attachments: &'a [(String, Vec<u8>)],
}

/// Objects uploaded at the same time. Every PUT is independent and retried on
/// its own, and a release with hundreds of artifacts is dominated by network
/// round trips. The bound keeps CI runners and enterprise uplinks from opening
/// dozens of connections.
const UPLOAD_CONCURRENCY: usize = 4;

/// Upload objects and confirm every one exists with the expected size. This
/// runs before any manifest is published. Transient object-store failures are
/// retried; a release can be re-run safely because keys are content-addressed
/// or version-scoped.
///
/// The first failure stops the remaining uploads; the verification pass still
/// runs only when every object succeeded.
pub fn upload_objects(publisher: &dyn Publisher, objects: &[ObjectUpload]) -> anyhow::Result<()> {
    let next = AtomicUsize::new(0);
    let failure: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    let workers = UPLOAD_CONCURRENCY.min(objects.len());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                if failure.lock().map(|slot| slot.is_some()).unwrap_or(true) {
                    return;
                }
                let Some(object) = objects.get(next.fetch_add(1, Ordering::Relaxed)) else {
                    return;
                };
                if let Err(error) = put_with_retry(publisher, object) {
                    if let Ok(mut slot) = failure.lock() {
                        slot.get_or_insert(error);
                    }
                    return;
                }
            });
        }
    });
    match failure.into_inner() {
        Ok(Some(error)) => Err(error),
        _ => verify_objects(publisher, objects),
    }
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
///
/// The versioned manifest goes first so a reader that lands mid-publish sees
/// "manifest without signature" (clear) instead of "signature without
/// manifest". `latest` still cannot be atomic across the two objects: the
/// signature is written right before the manifest to keep that window as short
/// as possible, and clients retry a mismatched pair. A single-object envelope
/// removes the window entirely; see RFC 0001.
pub fn publish_manifests(publisher: &dyn Publisher, set: &ManifestSet<'_>) -> anyhow::Result<()> {
    let version_key = version_key(set.prefix, set.version);
    let version_signature_key = signature_key(&version_key);
    let latest_key = latest_key(set.prefix);
    let latest_signature_key = signature_key(&latest_key);

    let mut expected: Vec<(String, u64)> = Vec::new();
    for (id, bytes) in set.attachments {
        for key in [
            format!("{version_signature_key}.{id}"),
            format!("{latest_signature_key}.{id}"),
        ] {
            publisher.put(&key, bytes)?;
            expected.push((key, bytes.len() as u64));
        }
    }

    publisher.put(&version_key, set.manifest)?;
    publisher.put(&version_signature_key, set.signature)?;
    publisher.put(&latest_signature_key, set.signature)?;
    publisher.put(&latest_key, set.manifest)?;

    expected.extend([
        (version_signature_key.clone(), set.signature.len() as u64),
        (latest_signature_key.clone(), set.signature.len() as u64),
        (version_key.clone(), set.manifest.len() as u64),
        (latest_key.clone(), set.manifest.len() as u64),
    ]);

    // Last, so the envelope always describes a fully published release. The
    // manifest is embedded as text: the signature covers those exact bytes.
    let key = envelope_key(set.prefix);
    let envelope = envelope(set.manifest, set.signature)?;
    publisher.put(&key, &envelope)?;
    expected.push((key, envelope.len() as u64));

    for (key, size) in expected {
        match publisher.head(&key)? {
            Some(actual) if actual == size => {}
            Some(actual) => bail!("{key} has size {actual}, expected {size}"),
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

/// `latest.json` and its signature in one object. Both are embedded as text so
/// the signed bytes survive serialization unchanged.
pub fn envelope(manifest: &[u8], signature: &[u8]) -> anyhow::Result<Vec<u8>> {
    let manifest = std::str::from_utf8(manifest).context("manifest is not UTF-8")?;
    let signature = std::str::from_utf8(signature).context("signature is not UTF-8")?;
    serde_json::to_vec(&serde_json::json!({
        "schema": 2,
        "manifest": manifest,
        "signature": signature,
    }))
    .context("serialize envelope")
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
    let key = envelope_key(prefix);
    let envelope = envelope(manifest, signature)?;
    publisher.put(&key, &envelope)?;
    for (key, expected) in [
        (latest_signature_key.as_str(), signature.len() as u64),
        (latest_key.as_str(), manifest.len() as u64),
        (key.as_str(), envelope.len() as u64),
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
                attachments: &[],
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
        let envelope: serde_json::Value = serde_json::from_slice(
            &publisher
                .get("manifests/latest.envelope.json")
                .expect("envelope is published"),
        )
        .unwrap();
        assert_eq!(envelope["schema"], 2);
        assert_eq!(envelope["manifest"], "{}");
        assert_eq!(envelope["signature"], "sig");
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

    /// Counts overlapping PUTs so the test fails if uploads regress to serial.
    struct CountingPublisher {
        active: AtomicUsize,
        peak: AtomicUsize,
        stored: Mutex<Vec<String>>,
    }

    impl Publisher for CountingPublisher {
        fn put(&self, key: &str, _bytes: &[u8]) -> anyhow::Result<()> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(active, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(20));
            self.active.fetch_sub(1, Ordering::SeqCst);
            self.stored.lock().unwrap().push(key.to_string());
            Ok(())
        }

        fn head(&self, key: &str) -> anyhow::Result<Option<u64>> {
            let stored = self.stored.lock().unwrap();
            Ok(stored
                .iter()
                .any(|seen| seen.as_str() == key)
                .then_some(1_u64))
        }

        fn get(&self, key: &str) -> anyhow::Result<Vec<u8>> {
            match self.head(key)? {
                Some(size) => Ok(vec![0; size as usize]),
                None => bail!("missing {key}"),
            }
        }
    }

    #[test]
    fn uploads_run_in_parallel_and_still_verify() {
        let publisher = CountingPublisher {
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            stored: Mutex::new(Vec::new()),
        };
        let objects: Vec<ObjectUpload> = (0..8)
            .map(|index| ObjectUpload {
                key: format!("artifacts/full/1.0.0/{index}.bin"),
                body: ObjectBody::Bytes(vec![0]),
                size: 1,
            })
            .collect();

        upload_objects(&publisher, &objects).unwrap();

        assert_eq!(publisher.stored.lock().unwrap().len(), objects.len());
        assert!(
            publisher.peak.load(Ordering::SeqCst) > 1,
            "uploads must overlap"
        );
    }
}
