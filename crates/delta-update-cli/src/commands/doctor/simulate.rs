//! `--simulate-failure`: reproduce client-visible errors through their real
//! code paths.

use std::path::Path;

use anyhow::{bail, Context};
use delta_update_engine::journal::{UpdateLock, UPDATE_LOCK};
use delta_update_engine::limits::Limits;
use delta_update_engine::patch::apply_patch;
use delta_update_engine::patch::container::{Codec, PatchFormat, PatchHeader};
use delta_update_engine::staging::plan::PREPARED_SCHEMA;
use delta_update_engine::staging::{verify_plan, PreparedPlan, StagedFile};
use delta_update_engine::{EngineError, Strategy};

use crate::cli::{DoctorArgs, PatchReason, SimulateMode};
use crate::output;

/// Reproduce one client-visible failure and print the payload the plugin would
/// return, so an application can render and test its error UI without waiting
/// for a real outage. The command itself succeeds; scripts assert on
/// `.simulated_error.code`.
pub(crate) fn simulate_failure(args: &DoctorArgs, mode: SimulateMode) -> anyhow::Result<()> {
    if args.reason.is_some() && mode != SimulateMode::Patch {
        bail!("--reason only applies to `--simulate-failure patch`");
    }
    let reason = (mode == SimulateMode::Patch).then(|| args.reason.unwrap_or_default());
    let payload = simulated_error(mode, reason)?.to_payload();
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "v": 1,
                "tool": "delta-update-cli",
                "mode": "simulate",
                "ok": true,
                "scenario": mode.as_str(),
                "reason": reason.map(PatchReason::as_str),
                "simulated_error": payload,
            }))?
        );
    } else {
        output::info(&format!(
            "scenario: {}{}",
            mode.as_str(),
            reason
                .map(|reason| format!(" / {}", reason.as_str()))
                .unwrap_or_default()
        ));
        output::warn(&format!("{}: {}", payload.code, payload.message));
        let action = serde_json::to_value(payload.action)?;
        let category = serde_json::to_value(payload.category)?;
        output::info(&format!(
            "action: {}  retryable: {}  category: {}",
            action.as_str().unwrap_or_default(),
            payload.retryable,
            category.as_str().unwrap_or_default()
        ));
        output::info(&payload.user_action);
    }
    Ok(())
}

/// The error a scenario reproduces, produced by the same code path the client
/// hits rather than by a hand-written payload.
pub fn simulated_error(
    mode: SimulateMode,
    reason: Option<PatchReason>,
) -> anyhow::Result<EngineError> {
    match mode {
        SimulateMode::Lock => simulate_lock(),
        SimulateMode::Patch => simulate_patch(reason.unwrap_or_default()),
        SimulateMode::Disk => simulate_disk(),
    }
}

/// A second acquisition of the same file is what another instance holding the
/// lock looks like. Windows locks are per handle, so this returns
/// `E_UPDATE_LOCKED` instead of deadlocking.
fn simulate_lock() -> anyhow::Result<EngineError> {
    let dir = tempfile::tempdir().context("create lock directory")?;
    let path = dir.path().join(UPDATE_LOCK);
    let _held = UpdateLock::acquire(&path)?;
    match UpdateLock::acquire(&path) {
        Ok(_) => bail!("simulated lock was acquired twice"),
        Err(error) => Ok(error),
    }
}

/// The four patch failures an application can see, each produced by the code
/// path that really raises it.
fn simulate_patch(reason: PatchReason) -> anyhow::Result<EngineError> {
    let dir = tempfile::tempdir().context("create patch directory")?;
    match reason {
        PatchReason::SourceMismatch => simulate_source_mismatch(dir.path()),
        PatchReason::Format => simulate_format_error(dir.path()),
        PatchReason::TargetHash => simulate_target_hash_mismatch(dir.path()),
        PatchReason::Memory => simulate_memory_budget(dir.path()),
    }
}

/// The real apply path rejects a source whose hash does not match the patch
/// header, which is what a tree that changed between check and prepare causes.
fn simulate_source_mismatch(dir: &Path) -> anyhow::Result<EngineError> {
    let source = dir.join("source.bin");
    std::fs::write(&source, b"local bytes that no longer match the patch")?;
    let header = patch_header(8);
    let patch = dir.join("patch.tdp");
    // The payload is never interpreted: the source hash is checked first.
    write_patch(&patch, &header, b"payload")?;
    apply_patch_expect_err(
        &source,
        &patch,
        &dir.join("target.bin"),
        &header,
        &Limits::default(),
    )
}

/// A container shorter than its fixed header is what a truncated or wrong
/// object key produces.
fn simulate_format_error(dir: &Path) -> anyhow::Result<EngineError> {
    let source = dir.join("source.bin");
    std::fs::write(&source, b"source bytes")?;
    let header = patch_header(8);
    let patch = dir.join("truncated.tdp");
    std::fs::write(&patch, b"TDP1\x01\x00\x00")?;
    apply_patch_expect_err(
        &source,
        &patch,
        &dir.join("target.bin"),
        &header,
        &Limits::default(),
    )
}

/// The reused-plan check compares staged files against the manifest, so a plan
/// whose recorded hash is stale reproduces the target-hash failure.
fn simulate_target_hash_mismatch(dir: &Path) -> anyhow::Result<EngineError> {
    let staged = dir.join("staged.bin");
    let content = b"staged bytes";
    std::fs::write(&staged, content)?;
    let plan = PreparedPlan {
        schema: PREPARED_SCHEMA,
        version: "0.0.0".into(),
        manifest_hash: "f".repeat(64),
        staging_root: dir.to_path_buf(),
        files: vec![StagedFile {
            path: "App.exe".into(),
            staging_path: staged,
            sha256: "0".repeat(64),
            size: content.len() as u64,
        }],
        removed: Vec::new(),
        all_files: Vec::new(),
    };
    match verify_plan(&plan) {
        Ok(()) => bail!("simulated plan unexpectedly verified"),
        Err(error) => Ok(error),
    }
}

/// The memory guard uses a 16 MiB floor, so the source has to cross it for the
/// budget to be exceeded without allocating anything large here.
fn simulate_memory_budget(dir: &Path) -> anyhow::Result<EngineError> {
    let source = dir.join("source.bin");
    std::fs::write(&source, vec![0_u8; 17 * 1024 * 1024])?;
    let header = patch_header(8);
    let patch = dir.join("patch.tdp");
    write_patch(&patch, &header, b"payload")?;
    let mut limits = Limits::default();
    limits.patch_memory_factor = 0.1;
    apply_patch_expect_err(&source, &patch, &dir.join("target.bin"), &header, &limits)
}

fn patch_header(target_size: u64) -> PatchHeader {
    PatchHeader {
        format: PatchFormat::V1 as u16,
        strategy_id: Strategy::QbsdiffZstd.patch_id().unwrap_or_default(),
        codec: Codec::None,
        target_size,
        source_sha256: [0x11; 32],
        target_sha256: [0x22; 32],
    }
}

fn write_patch(path: &Path, header: &PatchHeader, payload: &[u8]) -> anyhow::Result<()> {
    let mut bytes = header.encode().to_vec();
    bytes.extend_from_slice(payload);
    std::fs::write(path, bytes)?;
    Ok(())
}

fn apply_patch_expect_err(
    source: &Path,
    patch: &Path,
    dest: &Path,
    header: &PatchHeader,
    limits: &Limits,
) -> anyhow::Result<EngineError> {
    match apply_patch(source, patch, dest, header, limits) {
        Ok(()) => bail!("simulated patch unexpectedly applied"),
        Err(error) => Ok(error),
    }
}

/// Report the real free space and ask for one byte more, so the payload carries
/// believable numbers without filling a disk.
fn simulate_disk() -> anyhow::Result<EngineError> {
    let dir = tempfile::tempdir().context("create staging directory")?;
    let available = fs4::available_space(dir.path()).context("read free space")?;
    Ok(EngineError::InsufficientDiskSpace {
        required: available.saturating_add(1),
        available,
    })
}
