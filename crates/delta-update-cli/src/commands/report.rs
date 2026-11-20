//! `report`: cross-version coverage trend from `release --pipeline` reports.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{bail, Context};

use crate::cli::ReportArgs;
use crate::output;
use crate::pipeline::keys::human_bytes;

pub fn run(args: &ReportArgs) -> anyhow::Result<()> {
    report_history(&args.history, args.json)
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
pub fn report_history(dir: &Path, json: bool) -> anyhow::Result<()> {
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

    if json {
        let releases: Vec<serde_json::Value> = rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let drop = index.checked_sub(1).and_then(|previous| {
                    let before = coverage_percent(&rows[previous])?;
                    let current = coverage_percent(row)?;
                    (before - current > 20.0).then(|| {
                        let fresh = new_fallbacks(&rows[previous], row)
                            .into_iter()
                            .map(|(path, reason)| {
                                serde_json::json!({"path": path, "reason": reason})
                            })
                            .collect::<Vec<_>>();
                        serde_json::json!({
                            "from": rows[previous].version.to_string(),
                            "points": before - current,
                            "new_fallbacks": fresh,
                        })
                    })
                });
                serde_json::json!({
                    "version": row.version.to_string(),
                    "changed": row.changed,
                    "modified": row.modified,
                    "patched": row.patched,
                    "coverage_percent": coverage_percent(row),
                    "delta_bytes": row.delta_bytes,
                    "changed_bytes": row.changed_bytes,
                    "fallbacks": row
                        .fallbacks
                        .iter()
                        .map(|(path, reason)| serde_json::json!({"path": path, "reason": reason}))
                        .collect::<Vec<_>>(),
                    "coverage_drop": drop,
                })
            })
            .collect();
        let document = serde_json::json!({
            "v": 1,
            "tool": "delta-update-cli",
            "history": dir.display().to_string(),
            "releases": releases,
        });
        println!("{}", serde_json::to_string_pretty(&document)?);
        return Ok(());
    }

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
                    let fresh: Vec<String> = new_fallbacks(prev, row)
                        .into_iter()
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

fn new_fallbacks<'a>(previous: &HistoryRow, row: &'a HistoryRow) -> Vec<&'a (String, String)> {
    let known: BTreeSet<&str> = previous
        .fallbacks
        .iter()
        .map(|(path, _)| path.as_str())
        .collect();
    row.fallbacks
        .iter()
        .filter(|(path, _)| !known.contains(path.as_str()))
        .collect()
}

fn coverage_percent(row: &HistoryRow) -> Option<f64> {
    if row.modified == 0 {
        None
    } else {
        Some(row.patched as f64 / row.modified as f64 * 100.0)
    }
}
