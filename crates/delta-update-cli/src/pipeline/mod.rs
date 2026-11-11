//! Release pipeline. Stages are separate modules so each can be tested alone.

pub mod manifest;
pub mod patches;
pub mod plan;
pub mod publish;
pub mod scan;
pub mod sign;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{bail, Context};
use delta_update_engine::limits::Limits;
use delta_update_engine::manifest::parse::{parse_manifest, MAX_NOTES_BYTES};
use delta_update_engine::patch::container::PatchHeader;

use crate::cli::{DoctorArgs, ReleaseArgs, VerifyArgs, VerifyKeyArgs};
use crate::ignore::DeltaIgnore;
use crate::output;
use crate::pipeline::manifest::prefixed;
use crate::pipeline::manifest::ManifestFile;
use crate::pipeline::patches::{PatchAttempt, SkipReason};
use crate::pipeline::plan::ChangeKind;
use crate::pipeline::publish::{ManifestSet, ObjectBody, ObjectUpload};
use crate::publisher::{self, Publisher};

pub fn release(args: &ReleaseArgs) -> anyhow::Result<()> {
    let version = semver::Version::parse(&args.version)
        .with_context(|| format!("invalid --version: {}", args.version))?;
    if args.channel.trim().is_empty() || args.channel.chars().any(char::is_whitespace) {
        bail!("--channel must be a non-empty string without whitespace");
    }
    if !(0.0 < args.delta_max_ratio && args.delta_max_ratio <= 1.0) {
        bail!("--delta-max-ratio must be in (0, 1]");
    }
    if args.initial && args.from.is_some() {
        bail!("--initial cannot be combined with --from");
    }
    if args.initial && args.rollback {
        bail!("--rollback requires --from");
    }
    if args.rollback && args.from.is_none() {
        bail!("--rollback requires --from");
    }
    if matches!(args.jobs, Some(0)) {
        bail!("--jobs must be at least 1");
    }
    if !args.to.is_dir() {
        bail!("--to is not a directory: {}", args.to.display());
    }
    let prefix = normalize_prefix(args.prefix.as_deref())?;

    let from_version = match (&args.from, &args.from_version) {
        (Some(_), Some(value)) => {
            let parsed = semver::Version::parse(value)
                .with_context(|| format!("invalid --from-version: {value}"))?;
            if args.rollback {
                if version >= parsed {
                    bail!("--rollback requires --version ({version}) < --from-version ({parsed})");
                }
            } else if version <= parsed {
                bail!("--version ({version}) must be greater than --from-version ({parsed})");
            }
            Some((value.clone(), parsed))
        }
        (Some(_), None) => bail!("--from requires --from-version"),
        (None, Some(_)) => bail!("--from-version requires --from"),
        (None, None) => None,
    };
    if let Some(from) = &args.from {
        if !from.is_dir() {
            bail!("--from is not a directory: {}", from.display());
        }
    }
    if !args.key.is_file() {
        bail!("--key is not a file: {}", args.key.display());
    }

    let ignore = DeltaIgnore::from_root(&args.to)?;
    let to_scan = scan::scan(&args.to, &ignore)?;
    let from_scan = match &args.from {
        Some(root) => Some(scan::scan(root, &ignore)?),
        None => None,
    };
    let planned = plan::plan(from_scan.as_ref(), &to_scan);
    if planned.len() > Limits::default().max_files {
        bail!(
            "{} files exceeds the manifest limit of {}",
            planned.len(),
            Limits::default().max_files
        );
    }

    let publisher = if args.dry_run {
        None
    } else {
        Some(publisher::open(&args.bucket, args.region.as_deref())?)
    };

    let mut patch_results = match (&args.from, &from_scan) {
        (Some(from_root), Some(from_scan)) => {
            let generate = || {
                patches::generate(
                    from_root,
                    &args.to,
                    &planned,
                    from_scan,
                    &to_scan,
                    args.patch_max_file_size,
                    args.delta_max_ratio,
                )
            };
            match args.jobs {
                Some(jobs) => {
                    let pool = rayon::ThreadPoolBuilder::new()
                        .num_threads(jobs)
                        .build()
                        .context("build patch thread pool")?;
                    pool.install(generate)?
                }
                None => generate()?,
            }
        }
        _ => BTreeMap::new(),
    };

    if args.require_delta {
        let failed: Vec<String> = patch_results
            .iter()
            .filter_map(|(path, attempt)| match attempt {
                PatchAttempt::Skipped(SkipReason::Ratio) => Some(path.clone()),
                _ => None,
            })
            .collect();
        if !failed.is_empty() {
            bail!(
                "--require-delta: {} file(s) fell back to full: {}",
                failed.len(),
                failed.join(", ")
            );
        }
    }

    if args.verbose {
        println!(
            "{:<44} {:<16} {:>10} {:>10} {:>8}",
            "FILE", "STRATEGY", "FULL", "DELTA", "RATIO"
        );
    }

    let plan_map: BTreeMap<&str, ChangeKind> = planned
        .iter()
        .map(|file| (file.path.as_str(), file.kind))
        .collect();
    let mut files = BTreeMap::new();
    let mut changed = 0_u64;
    let mut objects = 0_u64;
    let mut changed_bytes = 0_u64;
    let mut delta_bytes = 0_u64;

    for (path, scanned) in &to_scan {
        delta_update_engine::path::validate_rel_path(path)
            .with_context(|| format!("invalid path in --to: {path}"))?;
        let kind = plan_map
            .get(path.as_str())
            .copied()
            .unwrap_or(ChangeKind::Added);
        let full_key = full_key(&prefix, &args.version, path);
        let mut entry = ManifestFile {
            sha256: scanned.sha256.clone(),
            size: scanned.size,
            strategy: "full".to_string(),
            delta_key: None,
            delta_size: None,
            full_key: None,
        };
        let mut delta_size = None;
        let mut ratio = None;
        let mut skip_reason = None;

        match kind {
            ChangeKind::Unchanged => {}
            ChangeKind::Added => {
                changed += 1;
                changed_bytes += scanned.size;
                entry.full_key = Some(full_key.clone());
            }
            ChangeKind::Modified => {
                changed += 1;
                changed_bytes += scanned.size;
                entry.full_key = Some(full_key.clone());
                match patch_results.remove(path) {
                    Some(PatchAttempt::Candidate(candidate)) => {
                        let key = delta_key(
                            &prefix,
                            from_version
                                .as_ref()
                                .map(|(value, _)| value.as_str())
                                .unwrap_or("initial"),
                            &args.version,
                            path,
                        );
                        delta_size = Some(candidate.bytes.len() as u64);
                        delta_bytes += candidate.bytes.len() as u64;
                        ratio = Some(candidate.ratio());
                        entry.strategy = "qbsdiff+zstd".to_string();
                        entry.delta_key = Some(key);
                        entry.delta_size = delta_size;
                        objects += 1;
                        if let Some(publisher) = &publisher {
                            publish::upload_objects(
                                publisher.as_ref(),
                                &[ObjectUpload {
                                    key: entry.delta_key.clone().unwrap(),
                                    body: ObjectBody::Bytes(candidate.bytes),
                                    size: delta_size.unwrap(),
                                }],
                            )?;
                        }
                    }
                    Some(PatchAttempt::Skipped(reason)) => skip_reason = Some(reason),
                    None => {}
                }
            }
        }

        if matches!(kind, ChangeKind::Added | ChangeKind::Modified) {
            objects += 1;
            if let Some(publisher) = &publisher {
                publish::upload_objects(
                    publisher.as_ref(),
                    &[ObjectUpload {
                        key: full_key,
                        body: ObjectBody::File(args.to.join(Path::new(path))),
                        size: scanned.size,
                    }],
                )?;
            }
        }

        if args.verbose {
            let strategy = match skip_reason {
                Some(SkipReason::Incompressible) => "full (compressed)",
                Some(SkipReason::TooLarge) => "full (too large)",
                Some(SkipReason::Ratio) => "full (ratio)",
                None if delta_size.is_some() => "qbsdiff+zstd",
                None => "full",
            };
            println!(
                "{:<44} {:<16} {:>10} {:>10} {:>8}",
                path,
                strategy,
                human_bytes(scanned.size),
                delta_size.map(human_bytes).unwrap_or_else(|| "-".into()),
                ratio
                    .map(|value| format!("{:.2}%", value * 100.0))
                    .unwrap_or_else(|| "-".into()),
            );
        }

        files.insert(path.clone(), entry);
    }

    let notes = match &args.notes_file {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("read --notes-file {}", path.display()))?;
            if text.len() > MAX_NOTES_BYTES {
                bail!("--notes-file exceeds the {MAX_NOTES_BYTES} byte limit");
            }
            Some(text)
        }
        None => None,
    };
    let manifest = manifest::build(manifest::ManifestInput {
        version: &args.version,
        channel: &args.channel,
        rollback: args.rollback,
        notes: notes.as_deref(),
        mandatory: args.mandatory,
        files,
    })?;
    if manifest.len() as u64 > Limits::default().manifest_max_bytes {
        bail!("manifest exceeds the client size limit");
    }
    let signature = sign::sign_manifest(&manifest, &args.key)?;

    if let Some(publisher) = &publisher {
        publish::publish_manifests(
            publisher.as_ref(),
            &ManifestSet {
                prefix: &prefix,
                version: &args.version,
                manifest: &manifest,
                signature: &signature.bytes,
            },
        )?;
        if args.verify {
            let public_key = sign::public_key_base64(&args.key)?;
            verify_published(publisher.as_ref(), &[public_key], &prefix, &args.version, 3)?;
        }
    } else if args.verify {
        output::warn("--verify is skipped for --dry-run");
    }

    output::info(&format!(
        "{}released {}: {} files, {} changed, {} objects, delta {}, full {}",
        if args.dry_run { "dry-run " } else { "" },
        args.version,
        to_scan.len(),
        changed,
        objects,
        human_bytes(delta_bytes),
        human_bytes(changed_bytes),
    ));
    Ok(())
}

pub fn verify(args: &VerifyArgs) -> anyhow::Result<()> {
    let publisher = publisher::open(&args.bucket, args.region.as_deref())?;
    let keys = read_public_keys(&args.public_key)?;
    let prefix = normalize_prefix(args.prefix.as_deref())?;
    verify_published(
        publisher.as_ref(),
        &keys,
        &prefix,
        &args.version,
        args.sample,
    )
}

pub fn verify_key(args: &VerifyKeyArgs) -> anyhow::Result<()> {
    let secret_key = sign::load_secret_key(&args.key)?;
    let public_key = minisign::PublicKey::from_secret_key(&secret_key)
        .context("derive public key from secret key")?
        .to_base64();
    let expected: Vec<&str> = args
        .public
        .split(',')
        .flat_map(str::lines)
        .map(str::trim)
        .filter(|value| !value.is_empty() && !value.starts_with("untrusted comment:"))
        .collect();
    if expected.is_empty() {
        bail!("--public must contain at least one public key");
    }
    if expected.contains(&public_key.as_str()) {
        output::info(&format!("key pair matches: {public_key}"));
        Ok(())
    } else {
        bail!("secret key does not match any provided public key")
    }
}

pub fn doctor(args: &DoctorArgs) -> anyhow::Result<()> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .context("build doctor http client")?;
    let keys = match &args.public_key {
        Some(path) => Some(read_public_keys(path)?),
        None => None,
    };

    let mut reports = Vec::new();
    for endpoint in &args.endpoint {
        reports.push(check_endpoint(&client, endpoint, keys.as_deref()));
    }
    let versions: BTreeSet<&str> = reports
        .iter()
        .filter_map(|report| report.version.as_deref())
        .collect();
    let consistent = versions.len() <= 1;
    let all_ok = reports.iter().all(|report| report.ok) && consistent;

    if args.json {
        let value = serde_json::json!({
            "ok": all_ok,
            "consistent": consistent,
            "versions": versions,
            "endpoints": reports.iter().map(|report| serde_json::json!({
                "endpoint": report.endpoint,
                "ok": report.ok,
                "version": report.version,
                "verified": report.verified,
                "errors": report.errors,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        for report in &reports {
            output::info(&format!(
                "{}: {}",
                report.endpoint,
                report.version.as_deref().unwrap_or("no manifest")
            ));
            for error in &report.errors {
                output::warn(error);
            }
            if !report.verified {
                output::warn(&format!(
                    "{}: signature not verified (pass --public-key)",
                    report.endpoint
                ));
            }
        }
        if !consistent {
            output::error("endpoints expose different versions");
        }
    }

    if !all_ok {
        bail!("doctor found problems");
    }
    Ok(())
}

struct DoctorReport {
    endpoint: String,
    ok: bool,
    version: Option<String>,
    verified: bool,
    errors: Vec<String>,
}

fn check_endpoint(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    keys: Option<&[String]>,
) -> DoctorReport {
    let mut report = DoctorReport {
        endpoint: endpoint.to_string(),
        ok: true,
        version: None,
        verified: false,
        errors: Vec::new(),
    };
    let url = match url::Url::parse(endpoint) {
        Ok(url) => url,
        Err(error) => {
            report.ok = false;
            report.errors.push(format!("invalid URL: {error}"));
            return report;
        }
    };
    if !delta_update_engine::config::is_allowed_endpoint(&url, true) {
        report.ok = false;
        report
            .errors
            .push("endpoint must use https (http is allowed only on loopback)".into());
        return report;
    }
    if url.query().is_some() {
        report.ok = false;
        report
            .errors
            .push("presigned endpoints are not supported by doctor".into());
        return report;
    }

    let manifest = match fetch_limited(client, endpoint, Limits::default().manifest_max_bytes) {
        Ok(bytes) => bytes,
        Err(error) => {
            report.ok = false;
            report.errors.push(format!("manifest: {error}"));
            return report;
        }
    };
    let signature_url = format!("{endpoint}.minisig");
    let signature = match fetch_limited(client, &signature_url, 64 * 1024) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) => {
            report.ok = false;
            report.errors.push(format!("signature: {error}"));
            return report;
        }
    };
    if let Some(keys) = keys {
        if let Err(error) =
            delta_update_engine::crypto::verify_manifest(&manifest, &signature, keys)
        {
            report.ok = false;
            report.errors.push(format!("signature verify: {error}"));
            return report;
        }
        report.verified = true;
    }
    let parsed = match parse_manifest(&manifest, &Limits::default()) {
        Ok(manifest) => manifest,
        Err(error) => {
            report.ok = false;
            report.errors.push(format!("manifest parse: {error}"));
            return report;
        }
    };
    report.version = Some(parsed.version.clone());

    let origin = url.origin().ascii_serialization();
    for (path, entry) in &parsed.files {
        if entry.delta_key.is_some() && entry.full_key.is_none() {
            report.ok = false;
            report
                .errors
                .push(format!("{path}: patch entry is missing full_key"));
        }
        for (kind, key, expected) in [
            ("full", entry.full_key.as_deref(), Some(entry.size)),
            ("delta", entry.delta_key.as_deref(), entry.delta_size),
        ] {
            let Some(key) = key else { continue };
            let object_url = format!(
                "{origin}{}",
                crate::publisher::cos_signer::encode_path(&format!("/{key}"))
            );
            match head_object(client, &object_url) {
                Ok(Some(size)) if expected == Some(size) => {}
                Ok(Some(size)) => {
                    report.ok = false;
                    report.errors.push(format!(
                        "{path} {kind} object has size {size}, expected {expected:?}"
                    ));
                }
                Ok(None) => {
                    report.ok = false;
                    report
                        .errors
                        .push(format!("{path} {kind} object is missing: {key}"));
                }
                Err(error) => {
                    report.ok = false;
                    report.errors.push(format!("{path} {kind} object: {error}"));
                }
            }
        }
    }
    report
}

fn fetch_limited(
    client: &reqwest::blocking::Client,
    url: &str,
    limit: u64,
) -> anyhow::Result<Vec<u8>> {
    use std::io::Read;
    let response = client.get(url).send().context("GET")?;
    if !response.status().is_success() {
        bail!("GET returned {}", response.status());
    }
    let mut bytes = Vec::new();
    response
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .context("read response")?;
    if bytes.len() as u64 > limit {
        bail!("response exceeds {limit} bytes");
    }
    Ok(bytes)
}

fn head_object(client: &reqwest::blocking::Client, url: &str) -> anyhow::Result<Option<u64>> {
    let response = client.head(url).send().context("HEAD")?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        bail!("HEAD returned {}", response.status());
    }
    Ok(Some(
        response
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
    ))
}

fn verify_published(
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
        if entry.delta_key.is_some() && entry.full_key.is_none() {
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
    }

    let mut patches_verified = 0_usize;
    let mut attempts = 0_usize;
    for (path, entry) in &manifest.files {
        if attempts >= sample {
            break;
        }
        let Some(delta_key) = &entry.delta_key else {
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
        let (from_version, to_version) = parse_delta_key(delta_key)
            .with_context(|| format!("cannot derive source version from {delta_key}"))?;
        if to_version != version {
            bail!("{path}: delta key targets {to_version}, expected {version}");
        }
        let source_key = prefixed(prefix, &format!("artifacts/full/{from_version}/{path}"));
        match publisher.get_optional(&source_key)? {
            Some(source) => {
                apply_and_check(path, &source, &patch, &header, &entry.sha256)?;
                patches_verified += 1;
            }
            None => output::warn(&format!(
                "{path}: source object {source_key} is not retained; patch header only"
            )),
        }
    }

    let mut full_verified = 0_usize;
    for (path, entry) in &manifest.files {
        if attempts + full_verified >= sample {
            break;
        }
        if entry.delta_key.is_some() {
            continue;
        }
        let Some(full_key) = &entry.full_key else {
            continue;
        };
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

fn read_public_keys(path: &Path) -> anyhow::Result<Vec<String>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read public key {}", path.display()))?;
    let mut keys = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("untrusted comment:") {
            continue;
        }
        for item in line.split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            delta_update_engine::crypto::validate_public_key(item)?;
            keys.push(item.to_string());
        }
    }
    if keys.is_empty() {
        bail!("{} contains no public keys", path.display());
    }
    Ok(keys)
}
fn full_key(prefix: &str, version: &str, path: &str) -> String {
    prefixed(prefix, &format!("artifacts/full/{version}/{path}"))
}

fn delta_key(prefix: &str, from_version: &str, version: &str, path: &str) -> String {
    prefixed(
        prefix,
        &format!("artifacts/{from_version}-to-{version}/{path}.tdp"),
    )
}

fn normalize_prefix(value: Option<&str>) -> anyhow::Result<String> {
    let prefix = value.unwrap_or("").trim_matches('/');
    if prefix.is_empty() {
        return Ok(String::new());
    }
    delta_update_engine::path::validate_rel_path(prefix)
        .with_context(|| format!("invalid --prefix: {prefix}"))?;
    Ok(prefix.to_string())
}

fn parse_delta_key(key: &str) -> Option<(&str, &str)> {
    let segments: Vec<&str> = key.split('/').collect();
    for pair in segments.windows(2) {
        if pair[0] == "artifacts" {
            if let Some(parts) = pair[1].rsplit_once("-to-") {
                return Some(parts);
            }
        }
    }
    None
}

fn human_bytes(value: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_from_version_from_delta_key() {
        assert_eq!(
            parse_delta_key("artifacts/2.0.0-to-2.1.0/App.exe.tdp"),
            Some(("2.0.0", "2.1.0"))
        );
        assert_eq!(
            parse_delta_key("artifacts/2.0.0-beta.1-to-2.1.0/a.tdp"),
            Some(("2.0.0-beta.1", "2.1.0"))
        );
        assert_eq!(
            parse_delta_key("releases/artifacts/2.0.0-to-2.1.0/a.tdp"),
            Some(("2.0.0", "2.1.0"))
        );
    }

    #[test]
    fn formats_bytes_for_humans() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
    }
}
