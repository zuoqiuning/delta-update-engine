use std::path::{Path, PathBuf};

use delta_update_engine::journal::{state, CommitStatus, FileOp, Journal};
use delta_update_engine::staging::PreparedPlan;
use serde::Serialize;

use crate::cli::InspectArgs;

#[derive(Debug, Serialize)]
struct Report {
    journal_dir: PathBuf,
    apply: Option<ApplyInfo>,
    status: Option<CommitStatus>,
    journal: Option<JournalInfo>,
    rollback_journal: Option<JournalInfo>,
    prepared: Option<PreparedInfo>,
    attempts: u64,
    skipped_version: Option<String>,
    warnings: Vec<String>,
    diagnosis: String,
}

#[derive(Debug, Serialize)]
struct ApplyInfo {
    version: String,
    commit_id: String,
    install_root: PathBuf,
    staging_root: PathBuf,
    parent_pid: u32,
}

#[derive(Debug, Serialize)]
struct JournalInfo {
    file: String,
    commit_id: String,
    version: String,
    from_version: String,
    ops: usize,
    backup: usize,
    place: usize,
    trash: usize,
    restore: usize,
    remove_trash: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    ops_detail: Option<Vec<OpInfo>>,
}

#[derive(Debug, Serialize)]
struct OpInfo {
    kind: String,
    path: String,
    status: String,
}

#[derive(Debug, Serialize)]
struct PreparedInfo {
    version: String,
    changed_files: usize,
    removed_files: usize,
    all_files: usize,
    staging_exists: bool,
}

impl JournalInfo {
    fn from_journal(
        file: &str,
        journal: &Journal,
        install_root: Option<&Path>,
        verbose: bool,
    ) -> Self {
        let mut info = Self {
            file: file.to_string(),
            commit_id: journal.commit_id.clone(),
            version: journal.version.clone(),
            from_version: journal.from_version.clone(),
            ops: journal.ops.len(),
            backup: 0,
            place: 0,
            trash: 0,
            restore: 0,
            remove_trash: 0,
            ops_detail: None,
        };
        let mut details = Vec::new();
        for op in &journal.ops {
            let (kind, path, status) = match op {
                FileOp::Backup { src, dst } => {
                    info.backup += 1;
                    (
                        "Backup",
                        src.clone(),
                        if install_root.is_some_and(|root| root.join(dst).exists()) {
                            "backed_up"
                        } else {
                            "pending"
                        },
                    )
                }
                FileOp::Place { dst, .. } => {
                    info.place += 1;
                    (
                        "Place",
                        dst.clone(),
                        if install_root.is_some_and(|root| root.join(dst).exists()) {
                            "landed"
                        } else {
                            "pending"
                        },
                    )
                }
                FileOp::Trash { src, trash_dst } => {
                    info.trash += 1;
                    let moved = install_root.is_some_and(|root| {
                        !root.join(src).exists() || root.join(".trash").join(trash_dst).exists()
                    });
                    (
                        "Trash",
                        src.clone(),
                        if moved { "landed" } else { "pending" },
                    )
                }
                FileOp::Restore { dst, .. } => {
                    info.restore += 1;
                    (
                        "Restore",
                        dst.clone(),
                        if install_root.is_some_and(|root| root.join(dst).exists()) {
                            "landed"
                        } else {
                            "pending"
                        },
                    )
                }
                FileOp::RemoveTrash { trash_path } => {
                    info.remove_trash += 1;
                    let removed = install_root
                        .is_none_or(|root| !root.join(".trash").join(trash_path).exists());
                    (
                        "RemoveTrash",
                        trash_path.clone(),
                        if removed { "landed" } else { "pending" },
                    )
                }
                _ => ("Unknown", String::new(), "unknown"),
            };
            if verbose {
                details.push(OpInfo {
                    kind: kind.into(),
                    path,
                    status: status.into(),
                });
            }
        }
        if verbose {
            info.ops_detail = Some(details);
        }
        info
    }
}

pub fn run(args: &InspectArgs) -> anyhow::Result<()> {
    if !args.journal_dir.is_dir() {
        anyhow::bail!(
            "journal directory does not exist: {}",
            args.journal_dir.display()
        );
    }
    let report = inspect(&args.journal_dir, args.verbose)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human(&report);
    }
    if !report.warnings.is_empty() {
        eprintln!(
            "inspect: {} warning(s); corrupt or unreadable state needs manual attention",
            report.warnings.len()
        );
        std::process::exit(2);
    }
    if report.journal.is_some() || report.rollback_journal.is_some() {
        eprintln!("inspect: unfinished update journal; restart the app to recover or retry");
        std::process::exit(1);
    }
    Ok(())
}

fn inspect(dir: &Path, verbose: bool) -> anyhow::Result<Report> {
    let mut warnings = Vec::new();

    let apply = match state::read_apply_config(dir) {
        Ok(config) => config.map(|config| ApplyInfo {
            version: config.version,
            commit_id: config.commit_id,
            install_root: config.install_root,
            staging_root: config.staging_root,
            parent_pid: config.parent_pid,
        }),
        Err(error) => {
            warnings.push(format!("apply.json 无法读取：{error}"));
            None
        }
    };
    let status = match state::read_commit_status(dir) {
        Ok(status) => status,
        Err(error) => {
            warnings.push(format!("commit.status 无法读取：{error}"));
            None
        }
    };
    let install_root = apply.as_ref().map(|apply| apply.install_root.clone());
    let journal = load_journal(
        dir,
        state::COMMIT_JOURNAL,
        &mut warnings,
        install_root.as_deref(),
        verbose,
    );
    let rollback_journal = load_journal(
        dir,
        state::ROLLBACK_JOURNAL,
        &mut warnings,
        install_root.as_deref(),
        verbose,
    );
    let prepared = load_prepared(dir, &mut warnings);
    let attempts = state::read_attempts(dir);
    let skipped_version = state::read_skipped_version(dir);

    let diagnosis = diagnose(
        journal.as_ref(),
        &status,
        prepared.as_ref(),
        skipped_version.as_deref(),
        &warnings,
    );

    Ok(Report {
        journal_dir: dir.to_path_buf(),
        apply,
        status,
        journal,
        rollback_journal,
        prepared,
        attempts,
        skipped_version,
        warnings,
        diagnosis,
    })
}

fn load_journal(
    dir: &Path,
    file: &str,
    warnings: &mut Vec<String>,
    install_root: Option<&Path>,
    verbose: bool,
) -> Option<JournalInfo> {
    let path = dir.join(file);
    if !path.exists() {
        return None;
    }
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            warnings.push(format!("{file} 无法读取：{error}"));
            return None;
        }
    };
    match Journal::decode(&bytes) {
        Ok(journal) => Some(JournalInfo::from_journal(
            file,
            &journal,
            install_root,
            verbose,
        )),
        Err(error) => {
            warnings.push(format!("{file} 损坏：{error}"));
            None
        }
    }
}

fn load_prepared(dir: &Path, warnings: &mut Vec<String>) -> Option<PreparedInfo> {
    let path = dir.join(state::PREPARED_JSON);
    if !path.exists() {
        return None;
    }
    match PreparedPlan::load(&path) {
        Ok(plan) => Some(PreparedInfo {
            staging_exists: plan.staging_root.is_dir(),
            version: plan.version,
            changed_files: plan.files.len(),
            removed_files: plan.removed.len(),
            all_files: plan.all_files.len(),
        }),
        Err(error) => {
            warnings.push(format!("prepared.json 损坏：{error}"));
            None
        }
    }
}

fn diagnose(
    journal: Option<&JournalInfo>,
    status: &Option<CommitStatus>,
    prepared: Option<&PreparedInfo>,
    skipped_version: Option<&str>,
    warnings: &[String],
) -> String {
    if !warnings.is_empty() {
        return "检测到损坏或不可读的状态文件；保留 journal 目录并上报，不要手动改写 Journal。"
            .into();
    }
    if let Some(journal) = journal {
        return match status {
            Some(CommitStatus::Completed { .. }) => {
                format!(
                    "{} 已完成替换，等待健康确认；确认后会清理备份与 Journal。",
                    journal.version
                )
            }
            Some(CommitStatus::Failed { op_index, message }) => format!(
                "{} 替换在第 {} 个操作失败（{message}）；下次启动会重试，仍失败则回滚。",
                journal.version, op_index
            ),
            None => format!(
                "{} 存在未完成的提交日志；下次启动会幂等重放并完成替换。",
                journal.version
            ),
        };
    }
    if let Some(CommitStatus::Completed { version, .. }) = status {
        return format!("{version} 已完成替换（Journal 已清理），等待健康确认。");
    }
    if let Some(CommitStatus::Failed { op_index, message }) = status {
        return format!(
            "提交失败记录在第 {op_index} 个操作（{message}），但找不到 commit.journal；保留目录并上报。"
        );
    }
    if let Some(prepared) = prepared {
        return format!(
            "{} 已准备完成，将在应用退出或 restart_to_apply 后应用。",
            prepared.version
        );
    }
    if let Some(version) = skipped_version {
        return format!("上次更新已回滚，当前跳过版本 {version}。");
    }
    "没有待处理的更新状态。".into()
}

fn print_human(report: &Report) {
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
    println!(
        "跳过版本: {}",
        report.skipped_version.as_deref().unwrap_or("无")
    );
    for warning in &report.warnings {
        println!("警告: {warning}");
    }
    println!("诊断: {}", report.diagnosis);
}
