use anyhow::Context;
use delta_update_engine::diagnostics::{self, DiagnosticReport};
use delta_update_engine::journal::CommitStatus;

use crate::cli::InspectArgs;

/// Identifies the producer in the bundle; the plugin reuses the same code.
const TOOL: &str = "delta-update-cli";

pub fn run(args: &InspectArgs) -> anyhow::Result<()> {
    if let Some(bundle) = &args.bundle {
        return crate::pipeline::bundle::inspect_bundle(args, bundle);
    }
    if args.from_release.is_some() {
        return crate::pipeline::inspect_release(args);
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
    if args.json {
        let mut value = serde_json::to_value(&report)?;
        if let Some(map) = value.as_object_mut() {
            map.insert("v".into(), 1.into());
            map.insert("tool".into(), TOOL.into());
        }
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        print_human(&report);
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

fn print_human(report: &DiagnosticReport) {
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
    println!("诊断: {}", report.diagnosis);
}
