use anyhow::Context;
use delta_update_engine::diagnostics::{self, DiagnosticReport};
use delta_update_engine::history::{self, UpdateRecord, UpdateResult};
use delta_update_engine::journal::CommitStatus;

use crate::cli::InspectArgs;
use crate::pipeline::audit::audit_release;
use crate::pipeline::keys::{human_bytes, normalize_prefix, read_public_keys};
use crate::publisher;

/// Identifies the producer in the bundle; the plugin reuses the same code.
const TOOL: &str = "delta-update-cli";

pub fn run(args: &InspectArgs) -> anyhow::Result<()> {
    if let Some(bundle) = &args.bundle {
        return crate::pipeline::bundle::inspect_bundle(args, bundle);
    }
    if args.from_release.is_some() {
        return inspect_release(args);
    }
    let journal_dir = args
        .journal_dir
        .as_deref()
        .context("--journal-dir is required")?;
    if !journal_dir.is_dir() {
        anyhow::bail!(
            "journal directory does not exist: {}",
            journal_dir.display()
        );
    }
    let report = diagnostics::collect(journal_dir, args.verbose)?;
    let worker_event = last_worker_event(&report.journal_dir);
    let history = history::read(&report.journal_dir, 10);
    if args.json {
        let mut value = serde_json::to_value(&report)?;
        if let Some(map) = value.as_object_mut() {
            map.insert("v".into(), 1.into());
            map.insert("tool".into(), TOOL.into());
            if let Some(event) = &worker_event {
                map.insert("worker_event".into(), event_json(event));
            }
            map.insert("history".into(), serde_json::to_value(&history)?);
        }
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        print_human(&report, worker_event.as_ref(), &history);
    }
    if let Some(path) = &args.export {
        diagnostics::write_bundle(journal_dir, args.verbose, TOOL, path, None, args.redact)?;
        println!("诊断包已写入 {}", path.display());
    }
    if report.needs_manual_attention() {
        eprintln!(
            "inspect: {} warning(s); corrupt or unreadable state needs manual attention",
            report.warnings.len()
        );
        std::process::exit(2);
    }
    if report.has_pending_journal() {
        eprintln!("inspect: unfinished update journal; restart the app to recover or retry");
        std::process::exit(1);
    }
    Ok(())
}

fn print_human(
    report: &DiagnosticReport,
    worker_event: Option<&delta_update_engine::worker_log::Event>,
    history: &[UpdateRecord],
) {
    println!("日志目录: {}", report.journal_dir.display());
    match &report.apply {
        Some(apply) => println!(
            "应用配置: version={}, commit_id={}, install_root={}, staging_root={}, parent_pid={}",
            apply.version,
            apply.commit_id,
            apply.install_root.display(),
            apply.staging_root.display(),
            apply.parent_pid
        ),
        None => println!("应用配置: 无"),
    }
    match &report.status {
        Some(CommitStatus::Completed { version, commit_id }) => {
            println!("提交状态: completed version={version} commit_id={commit_id}");
        }
        Some(CommitStatus::Failed { op_index, message }) => {
            println!("提交状态: failed op_index={op_index} message={message}");
        }
        None => println!("提交状态: 无"),
    }
    for (label, journal) in [
        ("提交日志", report.journal.as_ref()),
        ("回滚日志", report.rollback_journal.as_ref()),
    ] {
        match journal {
            Some(journal) => {
                println!(
                    "{label}: {} {} -> {}, {} 个操作（Backup={}, Place={}, Trash={}, Restore={}, RemoveTrash={}）",
                    journal.file,
                    journal.from_version,
                    journal.version,
                    journal.ops,
                    journal.backup,
                    journal.place,
                    journal.trash,
                    journal.restore,
                    journal.remove_trash
                );
                if let Some(ops) = &journal.ops_detail {
                    for op in ops {
                        println!("  - [{}] {}: {}", op.kind, op.path, op.status);
                    }
                }
            }
            None => println!("{label}: 无"),
        }
    }
    match &report.prepared {
        Some(prepared) => println!(
            "PREPARED: version={}, 变化文件={}, 删除={}, 文件全集={}, staging={}",
            prepared.version,
            prepared.changed_files,
            prepared.removed_files,
            prepared.all_files,
            if prepared.staging_exists {
                "存在"
            } else {
                "缺失"
            }
        ),
        None => println!("PREPARED: 无"),
    }
    println!("启动尝试: {}", report.attempts);
    if let Some(threshold) = report.crash_loop_threshold {
        println!("崩溃循环阈值: {threshold}");
        if report.attempts.saturating_add(1) > u64::from(threshold) {
            println!("警告: 下一次未确认启动将回滚到上一版本");
        }
    }
    println!(
        "跳过版本: {}",
        report.skipped_version.as_deref().unwrap_or("无")
    );
    for warning in &report.warnings {
        println!("警告: {warning}");
    }
    if let Some(event) = worker_event {
        println!(
            "最近 worker 事件: {}{}",
            event.name,
            event
                .get("msg")
                .map(|msg| format!(" — {msg}"))
                .unwrap_or_default()
        );
    }
    print_history(history);
    println!("诊断: {}", report.diagnosis);
}

/// Newest three terminal outcomes, so "did the last update succeed?" is
/// answered by the same command that explains the current state.
fn print_history(history: &[UpdateRecord]) {
    if history.is_empty() {
        println!("最近更新: 无");
        return;
    }
    println!("最近更新:");
    for record in history.iter().rev().take(3) {
        let reason = record.reason.as_deref().or(record.error_code.as_deref());
        println!(
            "  - v{} {}{} · {}",
            record.version,
            result_label(record.result),
            reason
                .map(|reason| format!(" ({reason})"))
                .unwrap_or_default(),
            human_age(record.finished_at_unix)
        );
    }
}

fn result_label(result: UpdateResult) -> &'static str {
    match result {
        UpdateResult::Completed => "成功",
        UpdateResult::RolledBack => "已回滚",
        UpdateResult::Failed => "失败",
    }
}

fn human_age(finished_at_unix: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let seconds = now.saturating_sub(finished_at_unix);
    match seconds {
        0..=59 => format!("{seconds} 秒前"),
        60..=3_599 => format!("{} 分钟前", seconds / 60),
        3_600..=86_399 => format!("{} 小时前", seconds / 3_600),
        _ => format!("{} 天前", seconds / 86_400),
    }
}

/// Newest structured `worker.log` event, shared by the human and `--json`
/// outputs so both report the same outcome.
fn last_worker_event(dir: &std::path::Path) -> Option<delta_update_engine::worker_log::Event> {
    std::fs::read_to_string(dir.join(delta_update_engine::worker_log::FILE))
        .ok()
        .and_then(|log| crate::pipeline::bundle::last_event(&log))
}

fn event_json(event: &delta_update_engine::worker_log::Event) -> serde_json::Value {
    serde_json::json!({
        "name": &event.name,
        "ts": event.get("ts"),
        "msg": event.get("msg"),
    })
}

/// Audit a published release without downloading its objects.
///
/// Prints the same per-file table as `release --verbose`, but read back from
/// the bucket, plus a coverage summary and a HEAD check for every object key.
pub fn inspect_release(args: &InspectArgs) -> anyhow::Result<()> {
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
        crate::output::info(&format!(
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
            crate::output::warn(item);
        }
        anyhow::bail!(
            "{} object(s) are missing or have the wrong size",
            audit.missing.len()
        );
    }
    Ok(())
}
