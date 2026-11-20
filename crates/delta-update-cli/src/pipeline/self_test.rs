//! `--self-test`: apply and roll back the produced update on a copy of the
//! previous build, so a patch that passes header checks but does not reproduce
//! the target bytes fails before publishing.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context};
use delta_update_engine::limits::Limits;

use crate::pipeline::patches::PatchAttempt;
use crate::pipeline::plan::{ChangeKind, PlannedFile};
use crate::pipeline::scan::ScanResult;

/// Simulate the produced update on a copy of the previous build: journal apply,
/// then inverse-journal rollback. This catches patches that pass header checks
/// but do not reproduce the target bytes on a real install tree.
#[allow(clippy::too_many_arguments)]
pub(crate) fn self_test(
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
        schema: delta_update_engine::staging::plan::PREPARED_SCHEMA,
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
