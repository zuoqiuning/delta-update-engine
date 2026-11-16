//! `inspect --bundle`: rebuild "which path did this user actually take" from a
//! diagnostic bundle, the published manifest and the per-file fallback lines
//! that `prepare` appends to `worker.log`.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context};
use delta_update_engine::diagnostics::DiagnosticReport;
use serde::Deserialize;

use crate::cli::InspectArgs;
use crate::output;
use crate::pipeline::{
    audit_release, human_bytes, normalize_prefix, read_public_keys, ReleaseAudit,
};
use crate::publisher;

/// Only the two fields this command consumes; the rest of the bundle is left
/// untouched so its schema can evolve independently.
#[derive(Deserialize)]
struct BundleView {
    report: DiagnosticReport,
    #[serde(default)]
    worker_log: Option<String>,
}

#[derive(Default)]
struct Row {
    strategy: Option<String>,
    size: Option<u64>,
    delta_size: Option<u64>,
    ops: Vec<String>,
    fallbacks: Vec<String>,
}

pub(crate) fn inspect_bundle(args: &InspectArgs, bundle_path: &Path) -> anyhow::Result<()> {
    let raw =
        std::fs::read(bundle_path).with_context(|| format!("read {}", bundle_path.display()))?;
    let bundle: BundleView =
        serde_json::from_slice(&raw).with_context(|| format!("parse {}", bundle_path.display()))?;
    let report = bundle.report;

    let audit = match args.from_release.as_deref() {
        Some(bucket) => {
            let version = args
                .version
                .as_deref()
                .context("--version is required with --from-release")?;
            let public_key = args
                .public_key
                .as_deref()
                .context("--public-key is required with --from-release")?;
            let publisher = publisher::open(bucket, None)?;
            let prefix = normalize_prefix(args.prefix.as_deref())?;
            let keys = read_public_keys(public_key)?;
            Some(audit_release(publisher.as_ref(), &prefix, version, &keys)?)
        }
        None => None,
    };

    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    for journal in [report.journal.as_ref(), report.rollback_journal.as_ref()]
        .into_iter()
        .flatten()
    {
        if let Some(ops) = &journal.ops_detail {
            for op in ops {
                let row = rows.entry(op.path.clone()).or_default();
                row.ops.push(format!("{} ({})", op.kind, op.status));
            }
        }
    }
    if let Some(audit) = &audit {
        for file in &audit.rows {
            let row = rows.entry(file.path.clone()).or_default();
            row.strategy = Some(file.strategy.clone());
            row.size = Some(file.size);
            row.delta_size = file.delta_size;
        }
    }
    let mut fell_back = 0_usize;
    let mut worker_event = None;
    if let Some(log) = bundle.worker_log.as_deref() {
        worker_event = last_event(log);
        for (path, code) in fallbacks(log) {
            let row = rows.entry(path).or_default();
            row.fallbacks.push(code);
            fell_back += 1;
        }
    }

    if rows.is_empty() {
        bail!(
            "{} has no journal ops, manifest entries or patch fallback lines; nothing to cross-check",
            bundle_path.display()
        );
    }
    if let Some(journal) = &report.journal {
        if journal.ops_detail.is_none() {
            output::warn(
                "bundle was exported without --verbose, so per-file journal ops are missing; re-export with `inspect --export --verbose`",
            );
        }
    }

    let patched = audit.as_ref().map_or(0, |audit| audit.patched);
    let full_by_design = rows
        .values()
        .filter(|row| row.strategy.as_deref() == Some("full"))
        .count();
    let removed = rows
        .values()
        .filter(|row| row.strategy.is_none() && row.ops.iter().any(|op| op.starts_with("Trash")))
        .count();
    // A journal entry the signed manifest does not list cannot happen with a
    // worker that verifies every Place, so a non-empty result means the bundle
    // mixes versions or the journal was tampered with. Removals are excluded:
    // a removed path is absent from the new manifest by design. "Manifest but
    // no journal op" is not reported because unchanged files legitimately have
    // no operations.
    let journal_only = journal_only(&rows);

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "v": 1,
                "tool": "delta-update-cli",
                "bundle": bundle_path,
                "version": report.apply.as_ref().map(|apply| &apply.version),
                "files": rows.iter().map(|(path, row)| serde_json::json!({
                    "path": path,
                    "strategy": row.strategy,
                    "size": row.size,
                    "delta_size": row.delta_size,
                    "ops": row.ops,
                    "fallbacks": row.fallbacks,
                })).collect::<Vec<_>>(),
                "patched": patched,
                "fell_back": fell_back,
                "full_by_design": full_by_design,
                "removed": removed,
                "journal_only": journal_only,
                "worker_event": worker_event.as_ref().map(|event| serde_json::json!({
                    "name": &event.name,
                    "ts": event.get("ts"),
                    "msg": event.get("msg"),
                })),
            }))?
        );
    } else {
        print_table(&rows);
        output::info(&format!(
            "bundle: {patched} files patched, {fell_back} fell back to full, {full_by_design} full by design, {removed} removed"
        ));
        if let Some(event) = &worker_event {
            output::info(&format!(
                "last worker event: {}{}",
                event.name,
                event
                    .get("msg")
                    .map(|msg| format!(" — {msg}"))
                    .unwrap_or_default()
            ));
        }
        if !journal_only.is_empty() {
            output::warn(&format!(
                "{} journal entr(y/ies) are missing from the signed manifest (bundle mixes versions or the journal was tampered with): {}",
                journal_only.len(),
                journal_only
                    .iter()
                    .map(|path| path.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    if let Some(audit) = &audit {
        warn_missing(audit);
    }
    Ok(())
}

/// Newest structured event, so a support engineer sees the outcome without
/// reading the raw log. Legacy prose lines are not events.
fn last_event(log: &str) -> Option<delta_update_engine::worker_log::Event> {
    log.lines()
        .filter_map(delta_update_engine::worker_log::parse)
        .next_back()
}

/// Per-file patch fallbacks, from `event=patch_fallback` lines and, for logs
/// written before that format, from `patch fallback: <path>: <code>` prose.
fn fallbacks(log: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for line in log.lines() {
        if let Some(event) = delta_update_engine::worker_log::parse(line) {
            if event.name == "patch_fallback" {
                if let (Some(path), Some(code)) = (event.get("path"), event.get("code")) {
                    found.push((path.to_string(), code.to_string()));
                }
            }
            continue;
        }
        let Some((path, code)) = line
            .split_once("patch fallback: ")
            .map(|(_, rest)| rest)
            .and_then(|rest| rest.rsplit_once(": "))
        else {
            continue;
        };
        found.push((path.to_string(), code.to_string()));
    }
    found
}

fn print_table(rows: &BTreeMap<String, Row>) {
    println!(
        "{:<44} {:<20} {:<22} {:<24} WORKER.LOG",
        "FILE", "MANIFEST", "JOURNAL", "SIZES"
    );
    for (path, row) in rows {
        let strategy = row.strategy.as_deref().unwrap_or("-");
        let sizes = match (row.size, row.delta_size) {
            (Some(size), Some(delta)) => {
                format!("{} / delta {}", human_bytes(size), human_bytes(delta))
            }
            (Some(size), None) => human_bytes(size),
            _ => "-".into(),
        };
        let ops = if row.ops.is_empty() {
            "-".to_string()
        } else {
            row.ops.join(", ")
        };
        let evidence = if row.fallbacks.is_empty() {
            "-".to_string()
        } else {
            row.fallbacks.join(", ")
        };
        println!(
            "{:<44} {:<20} {:<22} {:<24} {}",
            path, strategy, ops, sizes, evidence
        );
    }
}

fn warn_missing(audit: &ReleaseAudit) {
    for item in &audit.missing {
        output::warn(item);
    }
}

/// Paths with a journal operation but no manifest entry, ignoring removals.
fn journal_only(rows: &BTreeMap<String, Row>) -> Vec<&String> {
    rows.iter()
        .filter(|(_, row)| {
            row.strategy.is_none() && row.ops.iter().any(|op| !op.starts_with("Trash"))
        })
        .map(|(path, _)| path)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(strategy: Option<&str>, ops: &[&str]) -> Row {
        Row {
            strategy: strategy.map(str::to_string),
            ops: ops.iter().map(|op| (*op).to_string()).collect(),
            ..Row::default()
        }
    }

    #[test]
    fn journal_only_ignores_removals_and_unchanged_files() {
        let mut rows = BTreeMap::new();
        // Listed in the manifest, touched by the journal: consistent.
        rows.insert(
            "App.exe".to_string(),
            row(Some("qbsdiff+zstd"), &["Place (landed)"]),
        );
        // Unchanged file: listed in the manifest, no journal ops.
        rows.insert("same.txt".to_string(), row(Some("full"), &[]));
        // Removed by design: journal Trash, absent from the new manifest.
        rows.insert("legacy.txt".to_string(), row(None, &["Trash (landed)"]));
        // Journal wrote a path the signed manifest does not list.
        rows.insert("ghost.dll".to_string(), row(None, &["Place (landed)"]));

        assert_eq!(journal_only(&rows), vec!["ghost.dll"]);
    }

    #[test]
    fn last_event_reports_the_newest_structured_line() {
        let log = "event=apply_started ts=1 commit=c1\n\
                   event=apply_ok ts=2 commit=c1 retries=0\n\
                   apply commit c1 ok in 12 ms (retries 0)\n";
        let event = last_event(log).expect("structured lines are events");
        assert_eq!(event.name, "apply_ok");
        assert_eq!(event.get("ts"), Some("2"));
        assert!(last_event("apply commit c1 failed: locked").is_none());
    }
}
