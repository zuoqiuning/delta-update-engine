//! Offline diagnostics for a journal directory.
//!
//! Shared by `delta-update inspect` and the plugin's `export_diagnostics`
//! command so support staff and the application itself produce the same
//! report. Everything here is read-only apart from writing the bundle file.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::EngineError;
use crate::journal::state::{self, CommitStatus};
use crate::journal::{FileOp, Journal};
use crate::staging::PreparedPlan;

/// The bundle only keeps the newest part of `worker.log`; the rest has no
/// diagnostic value and users should be able to attach one small file.
pub const WORKER_LOG_TAIL_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticReport {
    pub journal_dir: PathBuf,
    pub apply: Option<ApplyInfo>,
    pub status: Option<CommitStatus>,
    pub journal: Option<JournalInfo>,
    pub rollback_journal: Option<JournalInfo>,
    pub prepared: Option<PreparedInfo>,
    pub attempts: u64,
    /// Recorded by the last startup; `None` on a machine that never started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crash_loop_threshold: Option<u32>,
    pub skipped_version: Option<String>,
    pub warnings: Vec<String>,
    pub diagnosis: String,
}

impl DiagnosticReport {
    /// A journal exists, so a restart may recover or retry automatically.
    pub fn has_pending_journal(&self) -> bool {
        self.journal.is_some() || self.rollback_journal.is_some()
    }

    /// Corrupt or unreadable state files; manual attention is required.
    pub fn needs_manual_attention(&self) -> bool {
        !self.warnings.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyInfo {
    pub version: String,
    pub commit_id: String,
    pub install_root: PathBuf,
    pub staging_root: PathBuf,
    pub parent_pid: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalInfo {
    pub file: String,
    pub commit_id: String,
    pub version: String,
    pub from_version: String,
    pub ops: usize,
    pub backup: usize,
    pub place: usize,
    pub trash: usize,
    pub restore: usize,
    pub remove_trash: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ops_detail: Option<Vec<OpInfo>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpInfo {
    pub kind: String,
    pub path: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedInfo {
    pub version: String,
    pub changed_files: usize,
    pub removed_files: usize,
    pub all_files: usize,
    pub staging_exists: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BundleFile {
    pub name: String,
    pub size: u64,
}

/// Platform context the CLI cannot see but the host application can.
#[derive(Debug, Clone, Serialize)]
pub struct SystemInfo {
    pub os: &'static str,
    pub os_version: Option<String>,
    pub kernel_version: Option<String>,
    /// Heuristic from the install path: `per-machine`, `per-user`, `portable`.
    pub install_form: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticBundle {
    pub tool: String,
    pub tool_version: String,
    pub platform: &'static str,
    pub generated_at_unix: u64,
    /// Absolute paths were replaced with `<install_root>` style placeholders.
    pub redacted: bool,
    pub system: SystemInfo,
    pub report: DiagnosticReport,
    pub worker_log: Option<String>,
    pub journal_dir_files: Vec<BundleFile>,
    /// Newest terminal outcomes, so a ticket answers "did previous updates
    /// succeed" without asking the user to run a second command.
    pub history: Vec<crate::history::UpdateRecord>,
}

impl DiagnosticBundle {
    /// Replace absolute roots with placeholders so the bundle can be attached
    /// to an external ticket without leaking user names or install layout.
    fn redact(&mut self) {
        let mut pairs: Vec<(String, &'static str)> = Vec::new();
        if let Some(apply) = &self.report.apply {
            pairs.push((apply.install_root.display().to_string(), "<install_root>"));
            pairs.push((apply.staging_root.display().to_string(), "<staging_root>"));
        }
        pairs.push((
            self.report.journal_dir.display().to_string(),
            "<journal_root>",
        ));
        if let Ok(home) = std::env::var("USERPROFILE") {
            pairs.push((home, "<user_home>"));
        }
        pairs.retain(|(from, _)| !from.is_empty());
        // Longest first so a nested root is replaced before its parent.
        pairs.sort_by_key(|(from, _)| std::cmp::Reverse(from.len()));

        if let Some(apply) = &mut self.report.apply {
            apply.install_root = PathBuf::from("<install_root>");
            apply.staging_root = PathBuf::from("<staging_root>");
        }
        self.report.journal_dir = PathBuf::from("<journal_root>");
        self.report.diagnosis = redact_text(&self.report.diagnosis, &pairs);
        if let Some(CommitStatus::Failed { message, .. }) = &mut self.report.status {
            *message = redact_text(message, &pairs);
        }
        if let Some(log) = &mut self.worker_log {
            *log = redact_text(log, &pairs);
        }
        self.redacted = true;
    }
}

/// Case-insensitive (ASCII) substring replacement that preserves the rest of
/// the text byte-for-byte.
fn redact_text(text: &str, pairs: &[(String, &'static str)]) -> String {
    let mut out = text.to_string();
    for (from, to) in pairs {
        let needle = ascii_lower(from);
        let haystack = ascii_lower(&out);
        let mut rebuilt = String::with_capacity(out.len());
        let mut start = 0;
        while let Some(offset) = haystack[start..].find(&needle) {
            let index = start + offset;
            rebuilt.push_str(&out[start..index]);
            rebuilt.push_str(to);
            start = index + needle.len();
        }
        rebuilt.push_str(&out[start..]);
        out = rebuilt;
    }
    out
}

/// Lowercase ASCII only, so byte offsets still line up with the original.
fn ascii_lower(value: &str) -> String {
    value
        .chars()
        .map(|character| character.to_ascii_lowercase())
        .collect()
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
                        !root.join(src).exists()
                            || root
                                .join(".trash")
                                .join(&journal.commit_id)
                                .join(trash_dst)
                                .exists()
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
                    let removed = install_root.is_none_or(|root| {
                        !root
                            .join(".trash")
                            .join(&journal.commit_id)
                            .join(trash_path)
                            .exists()
                    });
                    (
                        "RemoveTrash",
                        trash_path.clone(),
                        if removed { "landed" } else { "pending" },
                    )
                }
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

/// Collect the report without touching the install root.
pub fn collect(journal_dir: &Path, verbose: bool) -> Result<DiagnosticReport, EngineError> {
    let mut warnings = Vec::new();

    let apply = match state::read_apply_config(journal_dir) {
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
    let status = match state::read_commit_status(journal_dir) {
        Ok(status) => status,
        Err(error) => {
            warnings.push(format!("commit.status 无法读取：{error}"));
            None
        }
    };
    let install_root = apply.as_ref().map(|apply| apply.install_root.clone());
    let journal = load_journal(
        journal_dir,
        state::COMMIT_JOURNAL,
        &mut warnings,
        install_root.as_deref(),
        verbose,
    );
    let rollback_journal = load_journal(
        journal_dir,
        state::ROLLBACK_JOURNAL,
        &mut warnings,
        install_root.as_deref(),
        verbose,
    );
    let prepared = load_prepared(journal_dir, &mut warnings);
    let attempts = state::read_attempts(journal_dir);
    let crash_loop_threshold = state::read_crash_loop_threshold(journal_dir);
    let skipped_version = state::read_skipped_version(journal_dir);

    let diagnosis = diagnose(
        journal.as_ref(),
        rollback_journal.as_ref(),
        &status,
        prepared.as_ref(),
        skipped_version.as_deref(),
        &warnings,
    );

    Ok(DiagnosticReport {
        journal_dir: journal_dir.to_path_buf(),
        apply,
        status,
        journal,
        rollback_journal,
        prepared,
        attempts,
        crash_loop_threshold,
        skipped_version,
        warnings,
        diagnosis,
    })
}

/// Build the single-file bundle: report + worker log tail + file list + system.
///
/// `install_root_hint` lets the host application supply the install root when
/// `apply.json` is absent; the report itself stays authoritative.
pub fn build_bundle(
    journal_dir: &Path,
    verbose: bool,
    tool: &str,
    install_root_hint: Option<&Path>,
    redact: bool,
) -> Result<DiagnosticBundle, EngineError> {
    let report = collect(journal_dir, verbose)?;
    let install_root = report
        .apply
        .as_ref()
        .map(|apply| apply.install_root.as_path())
        .or(install_root_hint);
    let mut files = Vec::new();
    let entries = std::fs::read_dir(journal_dir)
        .map_err(|err| EngineError::Internal(format!("read {}: {err}", journal_dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|err| {
            EngineError::Internal(format!("read {}: {err}", journal_dir.display()))
        })?;
        let metadata = entry.metadata().map_err(|err| {
            EngineError::Internal(format!("stat {}: {err}", entry.path().display()))
        })?;
        if metadata.is_file() {
            files.push(BundleFile {
                name: entry.file_name().to_string_lossy().into_owned(),
                size: metadata.len(),
            });
        }
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));

    let mut bundle = DiagnosticBundle {
        tool: tool.to_string(),
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        platform: std::env::consts::OS,
        generated_at_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs()),
        redacted: false,
        system: system_info(install_root),
        report,
        worker_log: read_tail(&journal_dir.join("worker.log"), WORKER_LOG_TAIL_BYTES)?,
        journal_dir_files: files,
        history: crate::history::read(journal_dir, 20),
    };
    if redact {
        bundle.redact();
    }
    Ok(bundle)
}

/// Write the bundle as pretty JSON, creating the parent directory if needed.
pub fn write_bundle(
    journal_dir: &Path,
    verbose: bool,
    tool: &str,
    path: &Path,
    install_root_hint: Option<&Path>,
    redact: bool,
) -> Result<DiagnosticBundle, EngineError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|err| EngineError::Internal(format!("create {}: {err}", parent.display())))?;
    }
    let bundle = build_bundle(journal_dir, verbose, tool, install_root_hint, redact)?;
    let bytes = serde_json::to_vec_pretty(&bundle)
        .map_err(|err| EngineError::Internal(format!("serialize bundle: {err}")))?;
    std::fs::write(path, bytes)
        .map_err(|err| EngineError::Internal(format!("write {}: {err}", path.display())))?;
    Ok(bundle)
}

fn system_info(install_root: Option<&Path>) -> SystemInfo {
    SystemInfo {
        os: std::env::consts::OS,
        os_version: sysinfo::System::os_version(),
        kernel_version: sysinfo::System::kernel_version(),
        install_form: install_root.and_then(install_form),
    }
}

/// Classify the install location from well-known Windows environment roots.
/// This is a heuristic for support context, never used for a security decision.
fn install_form(install_root: &Path) -> Option<&'static str> {
    let root = install_root.to_string_lossy().to_lowercase();
    for (variable, form) in [
        ("ProgramFiles", "per-machine"),
        ("ProgramFiles(x86)", "per-machine"),
        ("LOCALAPPDATA", "per-user"),
    ] {
        if let Ok(base) = std::env::var(variable) {
            let base = base.to_lowercase();
            if !base.is_empty() && root.starts_with(&base) {
                return Some(form);
            }
        }
    }
    Some("portable")
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
    rollback_journal: Option<&JournalInfo>,
    status: &Option<CommitStatus>,
    prepared: Option<&PreparedInfo>,
    skipped_version: Option<&str>,
    warnings: &[String],
) -> String {
    if !warnings.is_empty() {
        return "检测到损坏或不可读的状态文件；保留 journal 目录并上报，不要手动改写 Journal。"
            .into();
    }
    if let Some(rollback) = rollback_journal {
        return format!(
            "{} 的回滚尚未完成；下次启动会自动续完回滚，不要手动删除 rollback.journal。",
            rollback.from_version
        );
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

/// Read at most the last `limit` bytes of a file, as lossy UTF-8.
///
/// When the boundary cuts a line in half, the partial first line is dropped so
/// the tail always starts on a line boundary. Redaction depends on this:
/// a path split across the boundary would otherwise survive unredacted.
fn read_tail(path: &Path, limit: u64) -> Result<Option<String>, EngineError> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(EngineError::Internal(format!(
                "read {}: {error}",
                path.display()
            )))
        }
    };
    let len = file
        .metadata()
        .map_err(|err| EngineError::Internal(format!("stat {}: {err}", path.display())))?
        .len();
    if len > limit {
        file.seek(SeekFrom::Start(len - limit))
            .map_err(|err| EngineError::Internal(format!("seek {}: {err}", path.display())))?;
    }
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)
        .map_err(|err| EngineError::Internal(format!("read {}: {err}", path.display())))?;
    let mut text = String::from_utf8_lossy(&buffer).into_owned();
    if len > limit {
        // Drop the partial first line so the tail stays readable.
        if let Some(position) = text.find('\n') {
            text.drain(..=position);
        }
    }
    Ok(Some(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_directory_produces_a_stable_report() {
        let dir = tempfile::tempdir().unwrap();
        let report = collect(dir.path(), false).unwrap();
        assert!(report.apply.is_none());
        assert!(!report.has_pending_journal());
        assert!(!report.needs_manual_attention());
        assert_eq!(report.diagnosis, "没有待处理的更新状态。");
    }

    #[test]
    fn corrupt_state_is_reported_as_manual_attention() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(state::COMMIT_JOURNAL), b"broken").unwrap();
        let report = collect(dir.path(), false).unwrap();
        assert!(report.needs_manual_attention());
        assert!(report.diagnosis.contains("损坏"));
    }

    #[test]
    fn install_form_is_classified_from_the_path() {
        let root = std::env::var("ProgramFiles")
            .ok()
            .map(std::path::PathBuf::from);
        if let Some(root) = root {
            assert_eq!(install_form(&root.join("App")), Some("per-machine"));
        }
        assert_eq!(
            install_form(Path::new("\\somewhere\\else")),
            Some("portable")
        );
    }

    #[test]
    fn redaction_replaces_roots_case_insensitively() {
        let pairs = vec![
            ("C:\\Users\\Alice\\App".to_string(), "<install_root>"),
            ("C:\\Users\\Alice".to_string(), "<user_home>"),
        ];
        let text = "create c:\\users\\alice\\app\\bin\\x.dll: denied; home C:\\USERS\\ALICE\\doc";
        assert_eq!(
            redact_text(text, &pairs),
            "create <install_root>\\bin\\x.dll: denied; home <user_home>\\doc"
        );
    }

    #[test]
    fn tail_starts_on_a_line_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("worker.log");
        let padding = "x".repeat(70_000);
        std::fs::write(
            &path,
            format!("{padding}\nfirst complete line\nsecond line\n"),
        )
        .unwrap();

        let tail = read_tail(&path, 64 * 1024).unwrap().unwrap();
        assert!(
            tail.starts_with("first complete line\n"),
            "tail head: {:?}",
            &tail[..40.min(tail.len())]
        );
        assert!(!tail.starts_with('x'));
    }
}
