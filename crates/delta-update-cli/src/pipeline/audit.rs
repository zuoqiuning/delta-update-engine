//! Read a published release back from the object store: verify the signature,
//! HEAD-check every object, and optionally re-apply a patch sample.

use std::collections::BTreeSet;

use anyhow::{bail, Context};
use delta_update_engine::limits::Limits;
use delta_update_engine::manifest::parse::parse_manifest;
use delta_update_engine::patch::container::PatchHeader;

use crate::output;
use crate::pipeline::keys::parse_delta_key;
use crate::pipeline::manifest::prefixed;
use crate::pipeline::publish;
use crate::publisher::Publisher;

/// One row of the published inventory.
pub(crate) struct AuditRow {
    pub(crate) path: String,
    pub(crate) strategy: String,
    pub(crate) size: u64,
    pub(crate) delta_size: Option<u64>,
    pub(crate) ratio: f64,
}

/// Result of reading a published release back from the object store.
pub(crate) struct ReleaseAudit {
    pub(crate) channel: Option<String>,
    pub(crate) files: usize,
    pub(crate) patched: usize,
    pub(crate) delta_bytes: u64,
    pub(crate) patched_full_bytes: u64,
    pub(crate) missing: Vec<String>,
    pub(crate) rows: Vec<AuditRow>,
}

/// Fetch, verify and HEAD-check a published release. Shared by
/// `inspect --from-release` and `release --pipeline`.
pub(crate) fn audit_release(
    publisher: &dyn Publisher,
    prefix: &str,
    version: &str,
    keys: &[String],
) -> anyhow::Result<ReleaseAudit> {
    let (raw, signature) = publish::fetch_manifest(publisher, prefix, version)?;
    delta_update_engine::crypto::verify_manifest(&raw, &signature, keys)
        .context("verify manifest signature")?;
    let manifest = parse_manifest(&raw, &Limits::default())?;
    if manifest.version != version {
        bail!(
            "manifest under {} reports version {}",
            version,
            manifest.version
        );
    }

    let mut delta_bytes = 0_u64;
    let mut patched_full_bytes = 0_u64;
    let mut patched = 0_usize;
    let mut rows = Vec::new();
    let mut missing = Vec::new();
    let mut files: Vec<_> = manifest.files.iter().collect();
    files.sort_by_key(|(path, _)| path.as_str());
    for (path, entry) in files {
        let delta_size = entry.delta_size.unwrap_or(0);
        let ratio = if entry.size == 0 {
            0.0
        } else {
            delta_size as f64 / entry.size as f64
        };
        if entry.delta_key.is_some() {
            patched += 1;
            delta_bytes = delta_bytes.saturating_add(delta_size);
            patched_full_bytes = patched_full_bytes.saturating_add(entry.size);
        }
        for (kind, key, expected) in [
            ("delta", entry.delta_key.as_deref(), entry.delta_size),
            ("full", entry.full_key.as_deref(), Some(entry.size)),
        ] {
            let Some(key) = key else { continue };
            match publisher.head(key)? {
                Some(size) if Some(size) == expected => {}
                Some(size) => missing.push(format!(
                    "{kind} object {key} has size {size}, manifest says {expected:?}"
                )),
                None => missing.push(format!("{kind} object {key} is missing")),
            }
        }
        rows.push(AuditRow {
            path: path.clone(),
            strategy: entry.strategy.as_str().to_string(),
            size: entry.size,
            delta_size: entry.delta_size,
            ratio,
        });
    }

    Ok(ReleaseAudit {
        channel: manifest.channel,
        files: manifest.files.len(),
        patched,
        delta_bytes,
        patched_full_bytes,
        missing,
        rows,
    })
}

/// Verify a published release: signature, object sizes, and a patch sample.
pub(crate) fn verify_published(
    publisher: &dyn Publisher,
    public_keys: &[String],
    prefix: &str,
    version: &str,
    sample: usize,
) -> anyhow::Result<()> {
    let (raw_manifest, signature) = publish::fetch_manifest(publisher, prefix, version)?;
    delta_update_engine::crypto::verify_manifest(&raw_manifest, &signature, public_keys)
        .context("verify manifest signature")?;
    let manifest = parse_manifest(&raw_manifest, &Limits::default())?;
    if manifest.version != version {
        bail!(
            "manifest version is {}, expected {version}",
            manifest.version
        );
    }

    let mut checked = 0_u64;
    for (path, entry) in &manifest.files {
        if (entry.delta_key.is_some() || !entry.deltas.is_empty()) && entry.full_key.is_none() {
            bail!("{path}: patch entry is missing full_key");
        }
        if let Some(key) = &entry.full_key {
            match publisher.head(key)? {
                Some(size) if size == entry.size => {}
                Some(size) => bail!(
                    "{path}: full object has size {size}, expected {}",
                    entry.size
                ),
                None => bail!("{path}: full object is missing: {key}"),
            }
            checked += 1;
        }
        if let Some(key) = &entry.delta_key {
            let expected = entry.delta_size.unwrap_or(0);
            match publisher.head(key)? {
                Some(size) if size == expected => {}
                Some(size) => bail!("{path}: delta object has size {size}, expected {expected}"),
                None => bail!("{path}: delta object is missing: {key}"),
            }
            checked += 1;
        }
        for delta in &entry.deltas {
            match publisher.head(&delta.delta_key)? {
                Some(size) if size == delta.delta_size => {}
                Some(size) => bail!(
                    "{path}: delta for {} has size {size}, expected {}",
                    delta.from_version,
                    delta.delta_size
                ),
                None => bail!(
                    "{path}: delta for {} is missing: {}",
                    delta.from_version,
                    delta.delta_key
                ),
            }
            checked += 1;
        }
    }

    let mut patches_verified = 0_usize;
    let mut attempts = 0_usize;
    for (path, entry) in &manifest.files {
        if attempts >= sample {
            break;
        }
        // Sample one patch per file: the primary baseline when present,
        // otherwise the first extra baseline.
        let delta_key = entry
            .delta_key
            .as_ref()
            .or_else(|| entry.deltas.first().map(|delta| &delta.delta_key));
        let Some(delta_key) = delta_key else {
            continue;
        };
        attempts += 1;
        let patch = publisher
            .get(delta_key)
            .with_context(|| format!("download {delta_key}"))?;
        let (header, _) = PatchHeader::parse(&patch)?;
        if header.target_size != entry.size {
            bail!(
                "{path}: patch target size {} != {}",
                header.target_size,
                entry.size
            );
        }
        let target_hash = hex::encode(header.target_sha256);
        if target_hash != entry.sha256 {
            bail!("{path}: patch target hash does not match the manifest");
        }
        let (_from_version, to_version) = parse_delta_key(delta_key)
            .with_context(|| format!("cannot derive source version from {delta_key}"))?;
        if to_version != version {
            bail!("{path}: delta key targets {to_version}, expected {version}");
        }
        let source_key = prefixed(
            prefix,
            &format!("blobs/{}", hex::encode(header.source_sha256)),
        );
        match publisher.get_optional(&source_key)? {
            Some(source) => {
                apply_and_check(path, &source, &patch, &header, &entry.sha256)?;
                patches_verified += 1;
            }
            None => output::warn(&format!(
                "{path}: source blob {source_key} is not retained; patch header only"
            )),
        }
    }

    let mut full_verified = 0_usize;
    let mut seen_full = BTreeSet::new();
    for (path, entry) in &manifest.files {
        if full_verified >= sample {
            break;
        }
        let Some(full_key) = &entry.full_key else {
            continue;
        };
        if !seen_full.insert(full_key.clone()) {
            continue;
        }
        let bytes = publisher
            .get(full_key)
            .with_context(|| format!("download {full_key}"))?;
        let hash = delta_update_engine::crypto::sha256_bytes(&bytes);
        if hash != entry.sha256 || bytes.len() as u64 != entry.size {
            bail!("{path}: downloaded full object does not match the manifest");
        }
        full_verified += 1;
    }

    output::info(&format!(
        "verified {version}: {checked} objects, {patches_verified} patch(es), {full_verified} full object(s)"
    ));
    Ok(())
}

fn apply_and_check(
    path: &str,
    source: &[u8],
    patch: &[u8],
    header: &PatchHeader,
    expected_sha256: &str,
) -> anyhow::Result<()> {
    let temp = tempfile::tempdir().context("create verify temp dir")?;
    let source_path = temp.path().join("source");
    let patch_path = temp.path().join("patch");
    let target_path = temp.path().join("target");
    std::fs::write(&source_path, source)?;
    std::fs::write(&patch_path, patch)?;
    delta_update_engine::patch::apply_patch(
        &source_path,
        &patch_path,
        &target_path,
        header,
        &Limits::default(),
    )
    .with_context(|| format!("{path}: apply patch"))?;
    let actual = delta_update_engine::crypto::sha256_file(&target_path)?;
    if actual != expected_sha256 {
        bail!("{path}: patched output hash does not match the manifest");
    }
    Ok(())
}
