//! Release pipeline. Stages are separate modules so each can be tested alone.

pub mod bundle;
pub mod manifest;
pub mod patches;
pub mod plan;
pub mod publish;
pub mod scan;
pub mod sign;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use delta_update_engine::limits::Limits;
use delta_update_engine::manifest::parse::{parse_manifest, MAX_NOTES_BYTES};
use delta_update_engine::patch::container::PatchHeader;

use crate::cli::{
    DoctorArgs, KeyInfoArgs, MigrateArgs, ReleaseArgs, RollbackArgs, VerifyArgs, VerifyKeyArgs,
};
use crate::ignore::DeltaIgnore;
use crate::output;
use crate::pipeline::manifest::prefixed;
use crate::pipeline::manifest::ManifestFile;
use crate::pipeline::patches::{PatchAttempt, SkipReason};
use crate::pipeline::plan::{ChangeKind, PlannedFile};
use crate::pipeline::publish::{ManifestSet, ObjectBody, ObjectUpload};
use crate::pipeline::scan::ScanResult;
use crate::publisher::{self, Publisher};

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
            // Rayon's global pool uses every core, and each patch job holds
            // source + target + suffix array + delta. Cap the default so a
            // many-core CI machine does not OOM; `--jobs` still overrides.
            let default_jobs = (std::thread::available_parallelism()
                .map(std::num::NonZeroUsize::get)
                .unwrap_or(1)
                / 2)
            .clamp(1, 4);
            let jobs = args.jobs.unwrap_or(default_jobs);
            output::info(&format!(
                "patch jobs: {jobs} (max file {}, estimated peak ~{})",
                human_bytes(args.patch_max_file_size),
                human_bytes(jobs as u64 * args.patch_max_file_size * 4),
            ));
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(jobs)
                .build()
                .context("build patch thread pool")?;
            pool.install(generate)?
        }
        _ => BTreeMap::new(),
    };

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
                self_test(
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
    let mut uploaded_blobs = HashSet::new();
    let mut fallbacks: Vec<(String, &'static str)> = Vec::new();

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

        // Every entry points at a content-addressed blob, including unchanged
        // files. This is what makes repair, skipped-version upgrades, and
        // rollback verification possible without re-uploading identical data.
        if uploaded_blobs.insert(full_key.clone()) {
            objects += 1;
            if let Some(publisher) = &publisher {
                match publisher.head(&full_key)? {
                    Some(size) if size == scanned.size => {}
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
    let manifest = manifest::build(manifest::ManifestInput {
        version: &args.version,
        channel: &args.channel,
        rollback: args.rollback,
        notes: notes.as_deref(),
        mandatory: args.mandatory,
        min_supported_version: args.min_supported.as_deref(),
        files,
    })?;
    if manifest.len() as u64 > Limits::default().manifest_max_bytes {
        bail!("manifest exceeds the client size limit");
    }
    let signature = sign::sign_manifest(&manifest, &args.key)?;
    let sbom = build_sbom(&manifest, &args.channel, &args.version)?;

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
            },
        )?;
        let sbom_key = publish::publish_sbom(publisher.as_ref(), &prefix, &args.version, &sbom)?;
        output::info(&format!("sbom: {sbom_key}"));
        if args.verify {
            let public_key = sign::public_key_base64(&args.key)?;
            verify_published(publisher.as_ref(), &[public_key], &prefix, &args.version, 3)?;
            if publisher.head(&sbom_key)?.is_none() {
                bail!("{sbom_key} is missing after upload");
            }
        }
        if args.pipeline {
            let public_key = sign::public_key_base64(&args.key)?;
            let audit = audit_release(
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
                    let round = audit_release(
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
    if args.pipeline {
        let report = serde_json::json!({
            "v": 1,
            "tool": "delta-update-cli",
            "generated_at_unix": unix_now(),
            "version": &args.version,
            "channel": &args.channel,
            "bucket": &args.bucket,
            "prefix": &prefix,
            "files": to_scan.len(),
            "changed": changed,
            "objects": objects,
            "delta_bytes": delta_bytes,
            "changed_bytes": changed_bytes,
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

/// Inventory derived from the signed manifest: the same files and hashes plus
/// the object keys that carry them. Advisory only; clients never fetch it.
fn build_sbom(raw: &[u8], channel: &str, version: &str) -> anyhow::Result<Vec<u8>> {
    let value: serde_json::Value =
        serde_json::from_slice(raw).context("parse manifest for sbom")?;
    let mut files: Vec<serde_json::Value> = value
        .get("files")
        .and_then(serde_json::Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(path, entry)| {
                    serde_json::json!({
                        "path": path,
                        "sha256": entry.get("sha256"),
                        "size": entry.get("size"),
                        "strategy": entry.get("strategy"),
                        "delta_key": entry.get("delta_key"),
                        "full_key": entry.get("full_key"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    let generated_at_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let sbom = serde_json::json!({
        "v": 1,
        "tool": "delta-update-cli",
        "format": "delta-update-sbom",
        "version": version,
        "channel": channel,
        "generated_at_unix": generated_at_unix,
        "signed_manifest": manifest::version_key("", version),
        "files": files,
    });
    serde_json::to_vec_pretty(&sbom).context("serialize sbom")
}

/// Simulate the produced update on a copy of the previous build: journal apply,
/// then inverse-journal rollback. This catches patches that pass header checks
/// but do not reproduce the target bytes on a real install tree.
#[allow(clippy::too_many_arguments)]
fn self_test(
    from_root: &Path,
    to_root: &Path,
    version: &str,
    from_version: &str,
    planned: &[PlannedFile],
    from_scan: &ScanResult,
    to_scan: &ScanResult,
    patch_results: &BTreeMap<String, PatchAttempt>,
) -> anyhow::Result<()> {
    use delta_update_engine::journal::state::OldFileHash;
    use delta_update_engine::journal::{
        plan as journal_plan, replay, Journal, JournalOutcome, ReplayContext,
    };
    use delta_update_engine::staging::{PreparedPlan, StagedFile};

    const COMMIT: &str = "00000000-0000-4000-8000-000000000001";

    let temp = tempfile::tempdir().context("create self-test directory")?;
    let install = temp.path().join("install");
    copy_tree(from_root, &install)?;
    let staging_root = temp.path().join("staging").join(version);
    let files_root = staging_root.join("files");
    let patches_root = temp.path().join("patches");
    std::fs::create_dir_all(&files_root)?;
    std::fs::create_dir_all(&patches_root)?;
    let journal_root = temp.path().join("journal");
    std::fs::create_dir_all(&journal_root)?;

    let old_hashes = from_scan
        .iter()
        .map(|(path, file)| {
            (
                path.clone(),
                OldFileHash {
                    sha256: file.sha256.clone(),
                    size: file.size,
                },
            )
        })
        .collect();

    let mut staged = Vec::new();
    for file in planned {
        let dest = files_root.join(&file.path);
        let scanned = match file.kind {
            ChangeKind::Unchanged => continue,
            ChangeKind::Added => {
                copy_file(&to_root.join(&file.path), &dest)?;
                to_scan
                    .get(&file.path)
                    .with_context(|| format!("missing target scan for {}", file.path))?
            }
            ChangeKind::Modified => {
                match patch_results.get(&file.path) {
                    Some(PatchAttempt::Candidate(candidate)) => {
                        if let Some(parent) = dest.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        let patch_path = patches_root.join(format!("{}.tdp", staged.len()));
                        std::fs::write(&patch_path, &candidate.bytes)
                            .context("write self-test patch")?;
                        let (header, _) =
                            delta_update_engine::patch::PatchHeader::parse(&candidate.bytes)
                                .context("parse self-test patch header")?;
                        delta_update_engine::patch::apply_patch(
                            &install.join(&file.path),
                            &patch_path,
                            &dest,
                            &header,
                            &Limits::default(),
                        )
                        .with_context(|| format!("apply patch for {}", file.path))?;
                    }
                    _ => copy_file(&to_root.join(&file.path), &dest)?,
                }
                to_scan
                    .get(&file.path)
                    .with_context(|| format!("missing target scan for {}", file.path))?
            }
        };
        staged.push(StagedFile {
            path: file.path.clone(),
            staging_path: dest,
            sha256: scanned.sha256.clone(),
            size: scanned.size,
        });
    }

    let plan = PreparedPlan {
        version: version.to_string(),
        manifest_hash: "self-test".into(),
        staging_root: staging_root.clone(),
        files: staged,
        removed: Vec::new(),
        all_files: to_scan.keys().cloned().collect(),
    };
    let journal = journal_plan::build(&plan, COMMIT, from_version, &install)
        .context("build self-test journal")?;
    let context = ReplayContext {
        install_root: &install,
        staging_root: &staging_root,
        journal_root: &journal_root,
        commit_id: COMMIT,
    };
    match replay(&context, &journal).context("apply self-test journal")? {
        JournalOutcome::Replayed { .. } | JournalOutcome::AlreadyCompleted => {}
        other => bail!("self-test apply did not complete: {other:?}"),
    }
    verify_scan(&install, to_scan).context("verify applied tree")?;

    let ops = delta_update_engine::rollback::inverse::inverse(&install, &journal, &old_hashes)
        .context("build inverse journal")?;
    let rollback = Journal::new(COMMIT, from_version, version, ops);
    match replay(&context, &rollback).context("rollback self-test journal")? {
        JournalOutcome::Replayed { .. } | JournalOutcome::AlreadyCompleted => {}
        other => bail!("self-test rollback did not complete: {other:?}"),
    }
    verify_scan(&install, from_scan).context("verify rolled-back tree")?;
    for path in to_scan.keys() {
        if !from_scan.contains_key(path) && install.join(path).exists() {
            bail!("rollback left the added file behind: {path}");
        }
    }
    Ok(())
}

fn copy_tree(source: &Path, dest: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in walkdir::WalkDir::new(source) {
        let entry = entry.context("walk self-test source")?;
        let relative = entry
            .path()
            .strip_prefix(source)
            .context("strip self-test root")?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let target = dest.join(relative);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() {
            copy_file(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn copy_file(source: &Path, dest: &Path) -> anyhow::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(source, dest)
        .with_context(|| format!("copy {} -> {}", source.display(), dest.display()))?;
    Ok(())
}

fn verify_scan(root: &Path, expected: &ScanResult) -> anyhow::Result<()> {
    for (path, file) in expected {
        let target = root.join(path);
        let metadata = std::fs::metadata(&target)
            .with_context(|| format!("missing after self-test: {path}"))?;
        if metadata.len() != file.size {
            bail!("self-test size mismatch for {path}");
        }
        let actual = delta_update_engine::crypto::sha256_file(&target)
            .with_context(|| format!("hash {path}"))?;
        if actual != file.sha256 {
            bail!("self-test hash mismatch for {path}");
        }
    }
    Ok(())
}

/// Print a migration plan from the official `tauri-plugin-updater`.
///
/// `--write` additionally creates `capabilities/delta-update.json` and
/// `delta-update-migration.md`; it never rewrites `tauri.conf.json`, because
/// silently reformatting a user's config would hide the one difference that
/// matters (the official `latest.json` cannot be reused).
pub fn migrate(args: &MigrateArgs) -> anyhow::Result<()> {
    let path = ["src-tauri/tauri.conf.json", "tauri.conf.json"]
        .iter()
        .map(|relative| args.dir.join(relative))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no tauri.conf.json under {}; run from a Tauri project root or pass --dir",
                args.dir.display()
            )
        })?;
    let json: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
    )
    .with_context(|| format!("parse {}", path.display()))?;
    let Some(updater) = json.pointer("/plugins/updater") else {
        bail!(
            "no plugins.updater block in {}; nothing to migrate",
            path.display()
        );
    };
    if json.pointer("/plugins/delta-update").is_some() {
        bail!(
            "plugins.delta-update already exists in {}; remove it first for a fresh migration plan",
            path.display()
        );
    }

    let endpoint = updater
        .get("endpoints")
        .and_then(|value| value.as_array())
        .and_then(|items| items.iter().find_map(|item| item.as_str()))
        .or_else(|| updater.get("endpoint").and_then(|value| value.as_str()));
    let pubkey = updater.get("pubkey").and_then(|value| value.as_str());

    println!("检测到 {} 的 plugins.updater。\n", path.display());
    if let Some(endpoint) = endpoint {
        if endpoint.contains("{{") {
            output::warn(
                "官方 endpoint 含 {{target}}/{{arch}} 模板；delta 清单是单平台的，请改为固定的 manifests/latest.json",
            );
        }
    }
    output::warn(
        "官方 updater 的 latest.json 不能复用：必须用 `delta-update release` 生成 delta 清单与签名",
    );

    let config = serde_json::json!({
        "plugins": {
            "delta-update": {
                "config_version": 1,
                "endpoint": endpoint.unwrap_or("<https://.../manifests/latest.json>"),
                "signing_public_keys": [pubkey.unwrap_or("<your minisign public key>")],
                "channel": "stable",
                "auto_check": true,
                "auto_apply": false,
                "auto_confirm": false
            }
        }
    });
    let capabilities = serde_json::json!({
        "identifier": "delta-update",
        "windows": ["main"],
        "permissions": ["core:default", "delta-update:manage"]
    });
    let guide = migration_guide(&path, &config, &capabilities)?;

    if args.write {
        crate::scaffold::write_capability(&args.dir, &capabilities)?;
        crate::scaffold::write_new_file(&args.dir.join("delta-update-migration.md"), &guide)?;
        println!("\ntauri.conf.json、Cargo.toml 与 main.rs 保持原样，请按指南手动修改。");
    } else {
        print!("{guide}");
        println!("\n提示：加 --write 可生成 capability 与 delta-update-migration.md（不会修改现有文件）。");
    }
    Ok(())
}

/// Migration guide written by `migrate --write`. Everything here is also
/// printed, so the file and stdout never drift apart.
fn migration_guide(
    conf: &Path,
    config: &serde_json::Value,
    capabilities: &serde_json::Value,
) -> anyhow::Result<String> {
    let config = serde_json::to_string_pretty(config)?;
    let capabilities = serde_json::to_string_pretty(capabilities)?;
    Ok(format!(
        "# 从 tauri-plugin-updater 迁移到 delta-update\n\n\
         检测到 `{}` 的 `plugins.updater`。\n\n\
         ## 1. tauri.conf.json\n\n用以下配置替换 `plugins.updater`：\n\n```json\n{config}\n```\n\n\
         ## 2. capability\n\n写入 `src-tauri/capabilities/delta-update.json`：\n\n```json\n{capabilities}\n```\n\n\
         ## 3. Cargo.toml\n\n```diff\n- tauri-plugin-updater = \"2\"\n+ tauri-plugin-delta-update = \"0.1\"\n```\n\n\
         ## 4. src-tauri/src/main.rs\n\n```diff\n  use tauri_plugin_delta_update::BuilderExt;\n\n  tauri::Builder::default()\n\
         -     .plugin(tauri_plugin_updater::Builder::new().build())\n\
         +     .plugin(tauri_plugin_delta_update::init())\n\
         -     .run(tauri::generate_context!())\n\
         +     .run_with_delta_update(tauri::generate_context!())\n```\n\n\
         ## 检查清单\n\n\
         - 删除 `plugins.updater` 配置块与 `tauri-plugin-updater` 依赖\n\
         - 删除默认 capability 里的 `updater:default` 权限\n\
         - 用 `delta-update release` 重新发布清单；官方 `latest.json` 不兼容\n\
         - 若前端需要 `confirm_healthy`，把 capability 改为 `delta-update:default`\n",
        conf.display()
    ))
}

/// One row of the published inventory.
struct AuditRow {
    path: String,
    strategy: String,
    size: u64,
    delta_size: Option<u64>,
    ratio: f64,
}

/// Result of reading a published release back from the object store.
struct ReleaseAudit {
    channel: Option<String>,
    files: usize,
    patched: usize,
    delta_bytes: u64,
    patched_full_bytes: u64,
    missing: Vec<String>,
    rows: Vec<AuditRow>,
}

/// Fetch, verify and HEAD-check a published release. Shared by
/// `inspect --from-release` and `release --pipeline`.
fn audit_release(
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

/// Audit a published release without downloading its objects.
///
/// Prints the same per-file table as `release --verbose`, but read back from
/// the bucket, plus a coverage summary and a HEAD check for every object key.
pub fn inspect_release(args: &crate::cli::InspectArgs) -> anyhow::Result<()> {
    let bucket = args
        .from_release
        .as_deref()
        .context("--from-release is required")?;
    let version = args.version.as_deref().context("--version is required")?;
    let public_key = args
        .public_key
        .as_deref()
        .context("--public-key is required")?;
    let publisher = publisher::open(bucket, None)?;
    let prefix = normalize_prefix(args.prefix.as_deref())?;
    let keys = read_public_keys(public_key)?;
    let audit = audit_release(publisher.as_ref(), &prefix, version, &keys)?;

    if args.json {
        let value = serde_json::json!({
            "v": 1,
            "tool": "delta-update-cli",
            "version": version,
            "channel": audit.channel,
            "files": audit.rows.iter().map(|row| serde_json::json!({
                "path": row.path,
                "strategy": row.strategy,
                "size": row.size,
                "delta_size": row.delta_size,
                "ratio": row.ratio,
            })).collect::<Vec<_>>(),
            "delta_bytes": audit.delta_bytes,
            "patched_full_bytes": audit.patched_full_bytes,
            "missing_objects": audit.missing,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!(
            "{:<44} {:<16} {:>10} {:>10} {:>8}",
            "FILE", "STRATEGY", "FULL", "DELTA", "RATIO"
        );
        for row in &audit.rows {
            println!(
                "{:<44} {:<16} {:>10} {:>10} {:>8}",
                row.path,
                row.strategy,
                human_bytes(row.size),
                row.delta_size
                    .map(human_bytes)
                    .unwrap_or_else(|| "-".into()),
                row.delta_size
                    .map(|_| format!("{:.2}%", row.ratio * 100.0))
                    .unwrap_or_else(|| "-".into()),
            );
        }
        let saved = audit.patched_full_bytes.saturating_sub(audit.delta_bytes);
        let percent = if audit.patched_full_bytes == 0 {
            0.0
        } else {
            saved as f64 / audit.patched_full_bytes as f64 * 100.0
        };
        output::info(&format!(
            "published {}: {} files, delta bytes {} vs {} for patched files (saved {:.1}%)",
            version,
            audit.files,
            human_bytes(audit.delta_bytes),
            human_bytes(audit.patched_full_bytes),
            percent
        ));
    }

    if !audit.missing.is_empty() {
        for item in &audit.missing {
            output::warn(item);
        }
        bail!(
            "{} object(s) are missing or have the wrong size",
            audit.missing.len()
        );
    }
    Ok(())
}

struct HistoryRow {
    version: semver::Version,
    changed: u64,
    modified: u64,
    patched: u64,
    delta_bytes: u64,
    changed_bytes: u64,
    fallbacks: Vec<(String, String)>,
}

/// Cross-version trend from the reports written by `release --pipeline`.
///
/// The only view that shows whether the delta strategy is regressing: a file
/// that silently grew past `--patch-max-file-size` shows up as a new fallback
/// next to a coverage drop.
pub fn report_history(dir: &Path) -> anyhow::Result<()> {
    if !dir.is_dir() {
        bail!("--history is not a directory: {}", dir.display());
    }
    let mut rows = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let value: serde_json::Value =
            serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        let Some(version) = value.get("version").and_then(|value| value.as_str()) else {
            continue;
        };
        let Ok(version) = semver::Version::parse(version) else {
            output::warn(&format!(
                "skip {}: version {version} is not SemVer",
                path.display()
            ));
            continue;
        };
        let fallbacks = value
            .get("fallbacks")
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        Some((
                            item.get("path")?.as_str()?.to_string(),
                            item.get("reason")?.as_str()?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        rows.push(HistoryRow {
            version,
            changed: value.get("changed").and_then(|v| v.as_u64()).unwrap_or(0),
            modified: value
                .pointer("/coverage/modified")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            patched: value
                .pointer("/coverage/patched")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            delta_bytes: value
                .get("delta_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            changed_bytes: value
                .get("changed_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            fallbacks,
        });
    }
    if rows.is_empty() {
        bail!("no release reports found in {}", dir.display());
    }
    rows.sort_by(|a, b| a.version.cmp(&b.version));

    println!(
        "{:<12} {:>8} {:>9} {:>10} {:>10} {:>12}",
        "VERSION", "CHANGED", "COVERAGE", "DELTA", "FULL", "PATCH/FALLBACK"
    );
    let mut previous: Option<&HistoryRow> = None;
    for row in &rows {
        let coverage = coverage_percent(row);
        println!(
            "{:<12} {:>8} {:>9} {:>10} {:>10} {:>12}",
            row.version,
            row.changed,
            coverage
                .map(|value| format!("{value:.1}%"))
                .unwrap_or_else(|| "n/a".into()),
            human_bytes(row.delta_bytes),
            human_bytes(row.changed_bytes),
            format!("{}/{}", row.patched, row.fallbacks.len()),
        );
        if let Some(prev) = previous {
            if let (Some(current), Some(before)) = (coverage, coverage_percent(prev)) {
                if before - current > 20.0 {
                    let known: BTreeSet<&str> = prev
                        .fallbacks
                        .iter()
                        .map(|(path, _)| path.as_str())
                        .collect();
                    let fresh: Vec<String> = row
                        .fallbacks
                        .iter()
                        .filter(|(path, _)| !known.contains(path.as_str()))
                        .map(|(path, reason)| format!("{path} ({reason})"))
                        .collect();
                    let detail = if fresh.is_empty() {
                        String::new()
                    } else {
                        format!("; new fallbacks: {}", fresh.join(", "))
                    };
                    output::warn(&format!(
                        "{} coverage {current:.1}% is {:.0} points below {}{detail}",
                        row.version,
                        before - current,
                        prev.version
                    ));
                }
            }
        }
        previous = Some(row);
    }
    Ok(())
}

fn coverage_percent(row: &HistoryRow) -> Option<f64> {
    if row.modified == 0 {
        None
    } else {
        Some(row.patched as f64 / row.modified as f64 * 100.0)
    }
}

/// Point `latest.json` at an already published manifest.
///
/// Unlike `release --rollback`, nothing is rebuilt: the versioned manifest and
/// its objects stay byte-identical, only `latest.json` is re-signed with
/// `rollback: true`. This is the incident-response path.
pub fn rollback(args: &RollbackArgs) -> anyhow::Result<()> {
    semver::Version::parse(&args.to_version)
        .with_context(|| format!("invalid --to-version: {}", args.to_version))?;
    let prefix = normalize_prefix(args.prefix.as_deref())?;
    let publisher = publisher::open(&args.bucket, args.region.as_deref())?;
    let public_key = sign::public_key_base64(&args.key)?;
    let (raw, signature) = publish::fetch_manifest(publisher.as_ref(), &prefix, &args.to_version)?;
    delta_update_engine::crypto::verify_manifest(
        &raw,
        &signature,
        std::slice::from_ref(&public_key),
    )
    .context("verify the published manifest with --key")?;
    let parsed = parse_manifest(&raw, &Limits::default())?;
    if parsed.version != args.to_version {
        bail!(
            "manifest under {} reports version {}",
            args.to_version,
            parsed.version
        );
    }

    let mut value: serde_json::Value =
        serde_json::from_slice(&raw).context("parse the published manifest")?;
    value["rollback"] = serde_json::Value::Bool(true);
    value["published_at"] = serde_json::Value::String(
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    );
    let manifest = serde_json::to_vec_pretty(&value).context("serialize rollback manifest")?;
    let signature = sign::sign_manifest(&manifest, &args.key)?;

    if args.dry_run {
        output::info(&format!(
            "dry-run rollback: latest.json -> {} ({} bytes)",
            args.to_version,
            manifest.len()
        ));
        return Ok(());
    }
    let key = publish::publish_latest(publisher.as_ref(), &prefix, &manifest, &signature.bytes)?;
    output::info(&format!(
        "{key} now points at {} (rollback: true); the versioned manifest is unchanged",
        args.to_version
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

pub fn key_info(args: &KeyInfoArgs) -> anyhow::Result<()> {
    let secret_key = sign::load_secret_key(&args.key)?;
    let public_key = minisign::PublicKey::from_secret_key(&secret_key)
        .context("derive public key from secret key")?;
    let key_id = public_key
        .keynum()
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<String>();
    output::info(&format!("public key: {}", public_key.to_base64()));
    output::info(&format!("key id: {key_id}"));
    Ok(())
}

pub fn doctor(args: &DoctorArgs) -> anyhow::Result<()> {
    if args.check_config {
        return check_plugin_config(args);
    }
    let Some(duration) = args.watch else {
        return doctor_once(args);
    };
    if args.json {
        bail!("--watch cannot be combined with --json; run doctor per interval instead");
    }
    if args.interval == 0 {
        bail!("--interval must be at least 1 second");
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(duration);
    let mut pass = 0_u32;
    loop {
        pass += 1;
        output::info(&format!("watch pass {pass}"));
        doctor_once(args)?;
        let now = std::time::Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline.duration_since(now);
        std::thread::sleep(std::time::Duration::from_secs(args.interval).min(remaining));
    }
    output::info(&format!("watch finished: {pass} pass(es)"));
    Ok(())
}

fn doctor_once(args: &DoctorArgs) -> anyhow::Result<()> {
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
            "v": 1,
            "tool": "delta-update-cli",
            "mode": "endpoints",
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

/// Validate `plugins.delta-update` from a project's `tauri.conf.json`.
///
/// This uses the engine's schema, so CI catches the same config errors the
/// plugin would report at runtime. It does not touch the network or require
/// `install_dir` to exist.
fn check_plugin_config(args: &DoctorArgs) -> anyhow::Result<()> {
    let path = ["src-tauri/tauri.conf.json", "tauri.conf.json"]
        .into_iter()
        .map(|name| args.dir.join(name))
        .find(|path| path.is_file())
        .with_context(|| {
            format!(
                "tauri.conf.json not found under {} (pass --dir <project root>)",
                args.dir.display()
            )
        })?;
    let raw = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let value: serde_json::Value =
        serde_json::from_slice(&raw).with_context(|| format!("parse {}", path.display()))?;
    let section = value
        .get("plugins")
        .and_then(|plugins| plugins.get("delta-update"))
        .with_context(|| {
            format!(
                "{} has no plugins.delta-update section; run `delta-update init` for a snippet",
                path.display()
            )
        })?;
    let config = delta_update_engine::config::PluginConfig::from_json(section)?;

    if args.json {
        let value = serde_json::json!({
            "v": 1,
            "tool": "delta-update-cli",
            "mode": "config",
            "ok": true,
            "config": path,
            "endpoint": config.endpoint,
            "channel": config.channel,
            "config_version": config.config_version,
            "errors": [],
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        output::info(&format!("config ok: {}", path.display()));
        output::info(&format!(
            "endpoint: {}  channel: {}  keys: {}",
            config.endpoint,
            config.channel,
            config.signing_public_keys.len()
        ));
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
        report.errors.push(
            "presigned endpoints are not supported by doctor; use `delta-update verify --bucket <uri>` against the bucket instead"
                .into(),
        );
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
    let length = response
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| anyhow::anyhow!("HEAD response is missing a valid Content-Length"))?;
    Ok(Some(length))
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
fn full_key(prefix: &str, sha256: &str) -> String {
    prefixed(prefix, &format!("blobs/{sha256}"))
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
