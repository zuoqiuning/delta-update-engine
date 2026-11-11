use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use sysinfo::{Pid, ProcessesToUpdate, System};

use crate::error::EngineError;
use crate::journal::state::{self, ApplyConfig};

const PARENT_WAIT_LIMIT: Duration = Duration::from_secs(600);

/// Entry point for `--delta-update-apply <journal_dir> <commit_id> [--relaunch]`.
///
/// The replacement process has no Tauri or WebView. It waits for the parent
/// process to exit, then reads `apply.json` and executes the journal. When
/// `relaunch` is set (the `restart_to_apply` path), the new binary is started
/// after the files have been replaced.
pub fn run_applier(
    journal_root: &Path,
    commit_id: &str,
    relaunch: bool,
) -> Result<(), EngineError> {
    let exe = std::env::current_exe()
        .map_err(|err| EngineError::ApplyHelperFailed(format!("current exe: {err}")))?;
    let config = state::read_apply_config(journal_root)?.ok_or(EngineError::NotPrepared)?;
    if config.commit_id != commit_id {
        return Err(EngineError::JournalCorrupt(
            "commit_id does not match apply.json".into(),
        ));
    }
    validate_scope(&exe, &config)?;
    wait_for_parent(config.parent_pid)?;
    super::writer::apply_commit(journal_root, commit_id)?;

    if relaunch {
        // The file at `exe` is the new version now. Spawning it must not use
        // the journal arguments; a plain start is a normal application launch.
        if let Err(error) = spawn_detached(&exe) {
            eprintln!("delta update: relaunch failed: {error}");
        }
    }
    Ok(())
}

/// Entry point for `--delta-update-recover <journal_dir> <crash_loop_threshold>`.
///
/// Used by the plugin at startup when a per-machine install left a journal
/// that cannot be replayed with the current user token. The worker reads
/// `apply.json` for the roots, re-validates them against its own executable,
/// and counts this process as the startup attempt.
pub fn run_recovery(journal_root: &Path, crash_loop_threshold: u32) -> Result<(), EngineError> {
    if crash_loop_threshold == 0 {
        return Err(EngineError::ConfigInvalid(
            "crash_loop_threshold must be at least 1".into(),
        ));
    }
    let exe = std::env::current_exe()
        .map_err(|err| EngineError::ApplyHelperFailed(format!("current exe: {err}")))?;
    let config = state::read_apply_config(journal_root)?.ok_or(EngineError::NotPrepared)?;
    if config.journal_root != journal_root {
        return Err(EngineError::JournalCorrupt(
            "journal_root does not match apply.json".into(),
        ));
    }
    validate_scope(&exe, &config)?;
    crate::recovery::recover_worker(&config, journal_root, crash_loop_threshold)
}

/// Detect replacement or recovery mode from process arguments. Returns
/// `None` for a normal application start.
pub fn run_applier_from_args() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    let mode = args.next()?;
    if mode == std::ffi::OsStr::new("--delta-update-recover") {
        let usage = || {
            eprintln!("usage: --delta-update-recover <journal_dir> <crash_loop_threshold>");
            2
        };
        let Some(journal_root) = args.next() else {
            return Some(usage());
        };
        let Some(threshold) = args.next() else {
            return Some(usage());
        };
        if args.next().is_some() {
            return Some(usage());
        }
        let Ok(threshold) = threshold.to_string_lossy().parse::<u32>() else {
            return Some(usage());
        };
        return Some(match run_recovery(Path::new(&journal_root), threshold) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("delta update recovery failed: {error}");
                1
            }
        });
    }
    if mode != std::ffi::OsStr::new("--delta-update-apply") {
        return None;
    }

    let usage = || {
        eprintln!("usage: --delta-update-apply <journal_dir> <commit_id> [--relaunch]");
        2
    };
    let Some(journal_root) = args.next() else {
        return Some(usage());
    };
    let Some(commit_id) = args.next() else {
        return Some(usage());
    };
    let relaunch = match args.next() {
        None => false,
        Some(flag) if flag == std::ffi::OsStr::new("--relaunch") => true,
        Some(_) => return Some(usage()),
    };
    if args.next().is_some() {
        return Some(usage());
    }
    let commit_id = commit_id.to_string_lossy();
    match run_applier(Path::new(&journal_root), &commit_id, relaunch) {
        Ok(()) => Some(0),
        Err(error) => {
            eprintln!("delta update apply failed: {error}");
            Some(1)
        }
    }
}

/// A worker must never write outside the installation that contains it. This
/// bounds the impact of a crafted `apply.json` when the worker is started by a
/// third party: the executable has to live inside `install_root`, and the
/// staging/journal roots may not be inside it.
fn validate_scope(exe: &Path, config: &ApplyConfig) -> Result<(), EngineError> {
    for (name, path) in [
        ("install_root", &config.install_root),
        ("staging_root", &config.staging_root),
        ("journal_root", &config.journal_root),
    ] {
        if !path.is_absolute() {
            return Err(EngineError::ConfigInvalid(format!(
                "{name} must be an absolute path: {}",
                path.display()
            )));
        }
    }
    if !exe.starts_with(&config.install_root) {
        return Err(EngineError::ApplyHelperFailed(format!(
            "install_root ({}) does not contain the worker executable ({})",
            config.install_root.display(),
            exe.display()
        )));
    }
    if config.staging_root.starts_with(&config.install_root)
        || config.journal_root.starts_with(&config.install_root)
    {
        return Err(EngineError::ConfigInvalid(format!(
            "staging_root ({}) and journal_root ({}) must stay outside install_root ({})",
            config.staging_root.display(),
            config.journal_root.display(),
            config.install_root.display()
        )));
    }
    Ok(())
}

#[cfg(windows)]
fn spawn_detached(exe: &Path) -> std::io::Result<()> {
    use std::path::PathBuf;

    // The worker may be running with an elevated token. Launching through the
    // shell starts the new version as the signed-in user instead of passing
    // the administrator token to the application.
    let explorer = std::env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join("explorer.exe"))
        .unwrap_or_else(|| PathBuf::from("explorer.exe"));
    Command::new(explorer)
        .arg(exe)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

#[cfg(not(windows))]
fn spawn_detached(exe: &Path) -> std::io::Result<()> {
    Command::new(exe)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

fn wait_for_parent(pid: u32) -> Result<(), EngineError> {
    if pid == 0 || pid == std::process::id() {
        return Ok(());
    }
    let target = Pid::from_u32(pid);
    let mut system = System::new();
    let deadline = Instant::now() + PARENT_WAIT_LIMIT;
    loop {
        system.refresh_processes(ProcessesToUpdate::Some(&[target]), true);
        if system.process(target).is_none() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(EngineError::ApplyHelperFailed(format!(
                "parent process {pid} did not exit"
            )));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn apply_config(install: &Path, staging: PathBuf, journal: PathBuf) -> ApplyConfig {
        ApplyConfig {
            v: 1,
            install_root: install.to_path_buf(),
            staging_root: staging,
            journal_root: journal,
            version: "2.0.0".into(),
            commit_id: "test".into(),
            parent_pid: 0,
        }
    }

    #[test]
    fn worker_writes_are_bound_to_its_install_root() {
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("install");
        std::fs::create_dir_all(&install).unwrap();
        let exe = install.join("app.exe");
        let staging = temp.path().join("staging");
        let journal = temp.path().join("journal");

        let config = apply_config(&install, staging.clone(), journal.clone());
        assert!(validate_scope(&exe, &config).is_ok());

        let elsewhere = temp.path().join("elsewhere.exe");
        assert!(validate_scope(&elsewhere, &config).is_err());

        let inside = apply_config(&install, install.join("staging"), journal);
        assert!(validate_scope(&exe, &inside).is_err());
    }

    #[test]
    fn relative_roots_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("install");
        std::fs::create_dir_all(&install).unwrap();
        let exe = install.join("app.exe");
        let config = apply_config(&install, PathBuf::from("staging"), PathBuf::from("journal"));
        assert!(validate_scope(&exe, &config).is_err());
    }
}
