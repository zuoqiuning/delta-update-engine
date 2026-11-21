//! `release`: scan, diff, patch, sign and publish a release.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use delta_update_engine::limits::Limits;
use delta_update_engine::manifest::parse::MAX_NOTES_BYTES;

use crate::cli::ReleaseArgs;
use crate::ignore::DeltaIgnore;
use crate::output;
use crate::pipeline::keys::{delta_key, full_key, human_bytes, normalize_prefix};
use crate::pipeline::manifest::{ManifestDelta, ManifestFile};
use crate::pipeline::patches::{PatchAttempt, SkipReason};
use crate::pipeline::plan::ChangeKind;
use crate::pipeline::publish::{ManifestSet, ObjectBody, ObjectUpload};
use crate::pipeline::{audit, manifest, patches, plan, publish, sbom, scan, self_test, sign};
use crate::publisher;

/// A non-primary patch baseline (`--baseline VERSION=DIR`).
struct ExtraBaseline {
    version: String,
    root: PathBuf,
    scan: scan::ScanResult,
    plan: Vec<plan::PlannedFile>,
}

/// What one baseline produced, for the report and the summary line.
#[derive(Default)]
struct BaselineStat {
    patches: u64,
    delta_bytes: u64,
    /// Target size of the files served as patches; the saving is the gap to
    /// `delta_bytes`.
    patched_full_bytes: u64,
}

pub fn run(args: &ReleaseArgs) -> anyhow::Result<()> {
    release(args)
}

pub fn release(args: &ReleaseArgs) -> anyhow::Result<()> {
    let version = semver::Version::parse(&args.version)
        .with_context(|| format!("invalid --version: {}", args.version))?;
    if args.channel.trim().is_empty() || args.channel.chars().any(char::is_whitespace) {
        bail!("--channel must be a non-empty string without whitespace");
    }
    if let Some(value) = args.min_supported.as_deref() {
        let min = semver::Version::parse(value)
            .with_context(|| format!("invalid --min-supported: {value}"))?;
        if min > version {
            bail!("--min-supported ({min}) must not exceed --version ({version})");
        }
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
    if args.pipeline && args.dry_run {
        bail!("--pipeline publishes and audits the result; remove --dry-run");
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
    if args.initial && !args.baselines.is_empty() {
        bail!("--initial cannot be combined with --baseline");
    }
    if !args.baselines.is_empty() && args.from.is_none() {
        // Extra baselines sit next to a primary one: legacy fields, coverage
        // statistics and self-test all describe `--from`.
        bail!("--baseline requires --from and --from-version");
    }
    let mut extra_baselines: Vec<(String, PathBuf)> = Vec::new();
    for spec in &args.baselines {
        let (value, dir) = spec
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--baseline must be VERSION=DIR, got {spec}"))?;
        semver::Version::parse(value)
            .with_context(|| format!("invalid --baseline version: {value}"))?;
        if value == args.version {
            bail!("--baseline {value} equals --version");
        }
        if from_version
            .as_ref()
            .is_some_and(|(primary, _)| primary == value)
        {
            bail!("--baseline {value} duplicates --from-version");
        }
        if extra_baselines
            .iter()
            .any(|(existing, _)| existing == value)
        {
            bail!("--baseline {value} was repeated");
        }
        let root = PathBuf::from(dir);
        if !root.is_dir() {
            bail!("--baseline {value}: not a directory: {}", root.display());
        }
        extra_baselines.push((value.to_string(), root));
    }
    if !args.key.is_file() {
        bail!("--key is not a file: {}", args.key.display());
    }

    let ignore = DeltaIgnore::from_root(&args.to)?;
    let jobs = effective_jobs(args.jobs);
    let to_scan = scan::scan_with_jobs(&args.to, &ignore, jobs)?;
    let from_scan = match &args.from {
        Some(root) => Some(scan::scan_with_jobs(root, &ignore, jobs)?),
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

    let mut extras: Vec<ExtraBaseline> = Vec::new();
    for (version, root) in &extra_baselines {
        let scan = scan::scan_with_jobs(root, &ignore, jobs)?;
        let plan = plan::plan(Some(&scan), &to_scan);
        extras.push(ExtraBaseline {
            version: version.clone(),
            root: root.clone(),
            scan,
            plan,
        });
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
            // Each patch job holds source + target + suffix array + delta.
            // `jobs` (the same value used for scanning) caps both pools.
            let inputs =
                patches::patch_input_sizes(&planned, from_scan, &to_scan, args.patch_max_file_size);
            let estimated_peak = patches::estimated_peak_bytes(&inputs, jobs);
            output::info(&format!(
                "patch jobs: {jobs} ({} files, largest {}, estimated peak ~{})",
                inputs.len(),
                human_bytes(inputs.first().map_or(0, |(_, size)| *size)),
                human_bytes(estimated_peak),
            ));
            if args.dry_run {
                for (path, size) in inputs.iter().take(10) {
                    output::info(&format!("  patch input: {path} {}", human_bytes(*size)));
                }
            }
            let mut system = sysinfo::System::new();
            system.refresh_memory();
            let total_memory = system.total_memory();
            if total_memory > 0 && estimated_peak > total_memory / 4 * 3 {
                output::warn(&format!(
                    "estimated patch memory {} exceeds 75% of system memory {}; lower --jobs or --patch-max-file-size",
                    human_bytes(estimated_peak),
                    human_bytes(total_memory),
                ));
            }
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(jobs)
                .build()
                .context("build patch thread pool")?;
            pool.install(generate)?
        }
        _ => BTreeMap::new(),
    };

    // Extra baselines run one after another: each pool is internally parallel,
    // so peak memory stays bounded by `jobs` rather than jobs x baselines.
    let mut extra_patch_results: Vec<(String, BTreeMap<String, PatchAttempt>)> = Vec::new();
    for baseline in &extras {
        let inputs = patches::patch_input_sizes(
            &baseline.plan,
            &baseline.scan,
            &to_scan,
            args.patch_max_file_size,
        );
        let estimated_peak = patches::estimated_peak_bytes(&inputs, jobs);
        output::info(&format!(
            "baseline {}: {} patch candidates, estimated peak ~{}",
            baseline.version,
            inputs.len(),
            human_bytes(estimated_peak)
        ));
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(jobs)
            .build()
            .context("build patch thread pool")?;
        let results = pool.install(|| {
            patches::generate(
                &baseline.root,
                &args.to,
                &baseline.plan,
                &baseline.scan,
                &to_scan,
                args.patch_max_file_size,
                args.delta_max_ratio,
            )
        })?;
        extra_patch_results.push((baseline.version.clone(), results));
    }

    // Coverage is computed before the loop consumes each attempt below.
    let modified_total = planned
        .iter()
        .filter(|file| file.kind == ChangeKind::Modified)
        .count();
    let patched = patch_results
        .values()
        .filter(|attempt| matches!(attempt, PatchAttempt::Candidate(_)))
        .count();
    let mut too_large = 0_usize;
    let mut ratio_skipped = 0_usize;
    let mut incompressible = 0_usize;
    for attempt in patch_results.values() {
        if let PatchAttempt::Skipped(reason) = attempt {
            match reason {
                SkipReason::TooLarge => too_large += 1,
                SkipReason::Ratio => ratio_skipped += 1,
                SkipReason::Incompressible => incompressible += 1,
            }
        }
    }

    let mut self_test_report = serde_json::Value::Null;
    if args.self_test || args.pipeline {
        if args.self_test && args.from.is_none() {
            bail!("--self-test requires --from and --from-version");
        }
        match (&args.from, &from_scan) {
            (Some(from_root), Some(from_scan)) => {
                let from_version = from_version
                    .as_ref()
                    .map(|(value, _)| value.as_str())
                    .unwrap_or("0.0.0");
                let started = std::time::Instant::now();
                self_test::self_test(
                    from_root,
                    &args.to,
                    &args.version,
                    from_version,
                    &planned,
                    from_scan,
                    &to_scan,
                    &patch_results,
                )?;
                let millis = started.elapsed().as_millis();
                output::info(&format!(
                    "self-test passed: apply + rollback in {millis} ms"
                ));
                self_test_report = serde_json::json!({ "ran": true, "millis": millis });
            }
            // Only `--pipeline --initial` reaches this arm; plain --self-test
            // bailed above.
            _ => {
                output::warn("--pipeline: initial release, self-test skipped");
                self_test_report = serde_json::json!({ "ran": false, "reason": "initial release" });
            }
        }
    }

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
                "--require-delta: {} file(s) fell back to full because the patch ratio exceeded the limit; small files may legitimately fall back — exclude them via .deltaignore or relax --require-delta for those files: {}",
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
    // Full size of the files that are actually delivered as patches; the
    // difference against `delta_bytes` is the client's real saving.
    let mut patched_bytes = 0_u64;
    let mut uploaded_full_bytes = 0_u64;
    let mut uploaded_delta_bytes = 0_u64;
    let mut reused_full_bytes = 0_u64;
    let mut reused_full_blobs = 0_u64;
    let mut uploaded_blobs = HashSet::new();
    let mut fallbacks: Vec<(String, &'static str)> = Vec::new();
    let primary_version = from_version.as_ref().map(|(value, _)| value.clone());
    let mut baseline_stats: BTreeMap<String, BaselineStat> = extras
        .iter()
        .map(|baseline| (baseline.version.clone(), BaselineStat::default()))
        .collect();

    for (path, scanned) in &to_scan {
        delta_update_engine::path::validate_rel_path(path)
            .with_context(|| format!("invalid path in --to: {path}"))?;
        let kind = plan_map
            .get(path.as_str())
            .copied()
            .unwrap_or(ChangeKind::Added);
        let full_key = full_key(&prefix, &scanned.sha256);
        let mut entry = ManifestFile {
            sha256: scanned.sha256.clone(),
            size: scanned.size,
            strategy: "full".to_string(),
            delta_key: None,
            delta_size: None,
            full_key: Some(full_key.clone()),
            deltas: Vec::new(),
        };
        let mut delta_size = None;
        let mut ratio = None;
        let mut skip_reason = None;

        match kind {
            ChangeKind::Unchanged => {}
            ChangeKind::Added => {
                changed += 1;
                changed_bytes += scanned.size;
            }
            ChangeKind::Modified => {
                changed += 1;
                changed_bytes += scanned.size;
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
                        let size = candidate.bytes.len() as u64;
                        delta_size = Some(size);
                        delta_bytes += size;
                        patched_bytes += scanned.size;
                        ratio = Some(candidate.ratio());
                        entry.strategy = "qbsdiff+zstd".to_string();
                        entry.delta_key = Some(key.clone());
                        entry.delta_size = Some(size);
                        if let Some(from) = &primary_version {
                            entry.deltas.push(ManifestDelta {
                                from_version: from.clone(),
                                strategy: "qbsdiff+zstd".into(),
                                delta_key: key.clone(),
                                delta_size: size,
                            });
                        }
                        objects += 1;
                        if let Some(publisher) = &publisher {
                            publish::upload_objects(
                                publisher.as_ref(),
                                &[ObjectUpload {
                                    key,
                                    body: ObjectBody::Bytes(candidate.bytes),
                                    size,
                                }],
                            )?;
                            uploaded_delta_bytes += size;
                        }
                    }
                    Some(PatchAttempt::Skipped(reason)) => skip_reason = Some(reason),
                    None => {}
                }
            }
        }

        for (version, results) in &mut extra_patch_results {
            let Some(PatchAttempt::Candidate(candidate)) = results.remove(path) else {
                continue;
            };
            let key = delta_key(&prefix, version, &args.version, path);
            let size = candidate.bytes.len() as u64;
            objects += 1;
            delta_bytes += size;
            if let Some(stats) = baseline_stats.get_mut(version) {
                stats.patches += 1;
                stats.delta_bytes += size;
                stats.patched_full_bytes += scanned.size;
            }
            if let Some(publisher) = &publisher {
                publish::upload_objects(
                    publisher.as_ref(),
                    &[ObjectUpload {
                        key: key.clone(),
                        body: ObjectBody::Bytes(candidate.bytes),
                        size,
                    }],
                )?;
                uploaded_delta_bytes += size;
            }
            entry.deltas.push(ManifestDelta {
                from_version: version.clone(),
                strategy: "qbsdiff+zstd".into(),
                delta_key: key,
                delta_size: size,
            });
        }

        // Every entry points at a content-addressed blob, including unchanged
        // files. This is what makes repair, skipped-version upgrades, and
        // rollback verification possible without re-uploading identical data.
        if uploaded_blobs.insert(full_key.clone()) {
            objects += 1;
            if let Some(publisher) = &publisher {
                match publisher.head(&full_key)? {
                    Some(size) if size == scanned.size => {
                        // Content-addressed key plus size: the same bytes are
                        // already published, so this release does not upload.
                        reused_full_bytes += scanned.size;
                        reused_full_blobs += 1;
                    }
                    Some(size) => bail!(
                        "blob {full_key} already exists with size {size}, expected {}",
                        scanned.size
                    ),
                    None => {
                        publish::upload_objects(
                            publisher.as_ref(),
                            &[ObjectUpload {
                                key: full_key,
                                body: ObjectBody::File(args.to.join(Path::new(path))),
                                size: scanned.size,
                            }],
                        )?;
                        uploaded_full_bytes += scanned.size;
                    }
                }
            }
        }

        if let Some(reason) = skip_reason {
            fallbacks.push((
                path.clone(),
                match reason {
                    SkipReason::TooLarge => "too_large",
                    SkipReason::Ratio => "ratio",
                    SkipReason::Incompressible => "incompressible",
                },
            ));
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
    let primary_key_id = sign::key_id(&args.key)?;
    let mut extra_keys: Vec<(String, &PathBuf)> = Vec::new();
    for path in &args.additional_signing_keys {
        if !path.is_file() {
            bail!("--additional-signing-key is not a file: {}", path.display());
        }
        let id = sign::key_id(path)?;
        if id == primary_key_id {
            bail!("--additional-signing-key duplicates the primary --key");
        }
        if extra_keys.iter().any(|(existing, _)| *existing == id) {
            bail!("--additional-signing-key repeated the same key id: {id}");
        }
        extra_keys.push((id, path));
    }
    let signing_key_ids: Vec<String> = extra_keys.iter().map(|(id, _)| id.clone()).collect();

    let manifest = manifest::build(manifest::ManifestInput {
        version: &args.version,
        channel: &args.channel,
        rollback: args.rollback,
        notes: notes.as_deref(),
        mandatory: args.mandatory,
        min_supported_version: args.min_supported.as_deref(),
        signing_key_ids: &signing_key_ids,
        files,
    })?;
    let manifest_limit = Limits::default().manifest_max_bytes;
    if manifest.len() as u64 > manifest_limit {
        bail!(
            "manifest is {} bytes, above the client limit of {manifest_limit}; every --baseline adds one entry per changed file — reduce baselines or exclude files via .deltaignore",
            manifest.len()
        );
    }
    if !extras.is_empty() && manifest.len() as u64 > manifest_limit / 4 * 3 {
        output::warn(&format!(
            "manifest is {} of the {manifest_limit} byte client limit; each --baseline costs one entry per file",
            manifest.len()
        ));
    }
    let signature = sign::sign_manifest(&manifest, &args.key)?;
    let mut attachments: Vec<(String, Vec<u8>)> = Vec::new();
    for (id, path) in &extra_keys {
        attachments.push((id.clone(), sign::sign_manifest(&manifest, path)?.bytes));
    }
    let sbom_bytes = sbom::build_sbom(&manifest, &args.channel, &args.version)?;

    let mut audit_report = serde_json::Value::Null;
    let mut watch_report = serde_json::Value::Null;
    if let Some(publisher) = &publisher {
        publish::publish_manifests(
            publisher.as_ref(),
            &ManifestSet {
                prefix: &prefix,
                version: &args.version,
                manifest: &manifest,
                signature: &signature.bytes,
                attachments: &attachments,
            },
        )?;
        let sbom_key =
            publish::publish_sbom(publisher.as_ref(), &prefix, &args.version, &sbom_bytes)?;
        output::info(&format!("sbom: {sbom_key}"));
        if args.verify {
            let public_key = sign::public_key_base64(&args.key)?;
            audit::verify_published(publisher.as_ref(), &[public_key], &prefix, &args.version, 3)?;
            if publisher.head(&sbom_key)?.is_none() {
                bail!("{sbom_key} is missing after upload");
            }
        }
        if args.pipeline {
            let public_key = sign::public_key_base64(&args.key)?;
            let audit = audit::audit_release(
                publisher.as_ref(),
                &prefix,
                &args.version,
                std::slice::from_ref(&public_key),
            )?;
            if !audit.missing.is_empty() {
                for item in &audit.missing {
                    output::warn(item);
                }
                bail!(
                    "{} object(s) are missing or have the wrong size",
                    audit.missing.len()
                );
            }
            output::info(&format!(
                "audit: {} files, {} patched, 0 missing",
                audit.files, audit.patched
            ));
            audit_report = serde_json::json!({
                "ok": true,
                "files": audit.files,
                "patched": audit.patched,
                "delta_bytes": audit.delta_bytes,
                "patched_full_bytes": audit.patched_full_bytes,
                "missing_objects": audit.missing,
            });
            if let Some(seconds) = args.watch.filter(|seconds| *seconds > 0) {
                if args.interval == 0 {
                    bail!("--interval must be at least 1 second");
                }
                // Re-check every object while a CDN/object store propagates the
                // release; any round that sees a missing object fails the run.
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
                let mut rounds = 0_u32;
                loop {
                    rounds += 1;
                    output::info(&format!("watch round {rounds}"));
                    let round = audit::audit_release(
                        publisher.as_ref(),
                        &prefix,
                        &args.version,
                        std::slice::from_ref(&public_key),
                    )?;
                    if !round.missing.is_empty() {
                        for item in &round.missing {
                            output::warn(item);
                        }
                        bail!(
                            "watch round {rounds}: {} object(s) missing or the wrong size",
                            round.missing.len()
                        );
                    }
                    let now = std::time::Instant::now();
                    if now >= deadline {
                        break;
                    }
                    let remaining = deadline.duration_since(now);
                    std::thread::sleep(
                        std::time::Duration::from_secs(args.interval).min(remaining),
                    );
                }
                output::info(&format!("watch finished: {rounds} round(s)"));
                watch_report = serde_json::json!({
                    "seconds": seconds,
                    "interval": args.interval,
                    "rounds": rounds,
                });
            }
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
    if modified_total > 0 {
        let fallback = modified_total.saturating_sub(patched);
        if fallback == 0 {
            output::info(&format!(
                "delta coverage: {patched}/{modified_total} modified files patched"
            ));
        } else {
            output::info(&format!(
                "delta coverage: {patched}/{modified_total} modified files patched ({fallback} fell back: {too_large} too large, {ratio_skipped} ratio, {incompressible} compressed)"
            ));
        }
    }
    for (version, stats) in &baseline_stats {
        if stats.patches > 0 {
            output::info(&format!(
                "baseline {version}: {} patch(es), delta {}, client savings {}",
                stats.patches,
                human_bytes(stats.delta_bytes),
                human_bytes(stats.patched_full_bytes.saturating_sub(stats.delta_bytes)),
            ));
        }
    }
    if args.pipeline {
        // Honest accounting, split by cost: `client` is what users download,
        // `publish` is what this run writes to the bucket. A first release has
        // no baseline, so its delta saving is zero by definition.
        let release_kind = if args.rollback {
            "rollback"
        } else if from_version.is_some() {
            "incremental"
        } else {
            "initial"
        };
        let full_download_bytes = changed_bytes.saturating_sub(patched_bytes);
        let net_savings_bytes = patched_bytes.saturating_sub(delta_bytes);
        if release_kind == "initial" {
            output::info(&format!(
                "publish: {} full uploaded (baseline; a first release has no delta savings)",
                human_bytes(uploaded_full_bytes)
            ));
        } else {
            output::info(&format!(
                "{release_kind}: client savings {} over full downloads; publish {} full + {} delta uploaded, {} full blobs reused",
                human_bytes(net_savings_bytes),
                human_bytes(uploaded_full_bytes),
                human_bytes(uploaded_delta_bytes),
                reused_full_blobs
            ));
        }
        // `client` and `coverage` describe the primary `--from` baseline; extra
        // baselines only add upload cost and per-file patch choices, so they get
        // their own counter instead of diluting the head-to-head saving.
        let baselines_report: Vec<serde_json::Value> = baseline_stats
            .iter()
            .map(|(version, stats)| {
                serde_json::json!({
                    "version": version,
                    "patches": stats.patches,
                    "delta_bytes": stats.delta_bytes,
                    "client": {
                        "patched_full_bytes": stats.patched_full_bytes,
                        "net_savings_bytes": stats
                            .patched_full_bytes
                            .saturating_sub(stats.delta_bytes),
                    },
                })
            })
            .collect();
        let report = serde_json::json!({
            "v": 1,
            "tool": "delta-update-cli",
            "generated_at_unix": unix_now(),
            "version": &args.version,
            "channel": &args.channel,
            "bucket": &args.bucket,
            "prefix": &prefix,
            "release_kind": release_kind,
            "files": to_scan.len(),
            "changed": changed,
            "objects": objects,
            "delta_bytes": delta_bytes,
            "changed_bytes": changed_bytes,
            "client": {
                "full_download_bytes": full_download_bytes,
                "net_savings_bytes": net_savings_bytes,
            },
            "publish": {
                "uploaded_full_bytes": uploaded_full_bytes,
                "uploaded_delta_bytes": uploaded_delta_bytes,
                "reused_full_bytes": reused_full_bytes,
                "reused_full_blobs": reused_full_blobs,
            },
            "baselines": baselines_report,
            "coverage": {
                "modified": modified_total,
                "patched": patched,
                "fallback": {
                    "too_large": too_large,
                    "ratio": ratio_skipped,
                    "incompressible": incompressible,
                },
            },
            "fallbacks": fallbacks.iter().map(|(path, reason)| serde_json::json!({
                "path": path,
                "reason": reason,
            })).collect::<Vec<_>>(),
            "self_test": self_test_report,
            "audit": audit_report,
            "watch": watch_report,
        });
        let path = report_path(&args.report, &args.version);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::write(&path, serde_json::to_vec_pretty(&report)?)
            .with_context(|| format!("write {}", path.display()))?;
        output::info(&format!("report: {}", path.display()));
    }
    Ok(())
}

/// One parallelism knob for the whole release.
///
/// `--jobs 0` would make rayon pick its own thread count while the memory
/// estimate assumed a single slot, underestimating peak usage; clamping here
/// keeps scanning, patching and the estimate on the same number.
fn effective_jobs(requested: Option<usize>) -> usize {
    requested.unwrap_or_else(scan::default_jobs).max(1)
}

/// `--report` with a `.json` extension is a file; anything else (or an existing
/// directory) accumulates `<dir>/<version>.json` so `report --history` works.
fn report_path(base: &Path, version: &str) -> PathBuf {
    if base.extension().is_none() || base.is_dir() {
        base.join(format!("{version}.json"))
    } else {
        base.to_path_buf()
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_jobs_clamps_zero_and_defaults() {
        assert_eq!(effective_jobs(Some(0)), 1);
        assert_eq!(effective_jobs(Some(3)), 3);
        assert!(effective_jobs(None) >= 1);
    }
}
