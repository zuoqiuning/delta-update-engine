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
///
/// This wrapper has no keys and therefore **fails closed**; use
/// [`run_applier_with_keys`] or the plugin's `run_applier_from_context`.
pub fn run_applier(
    journal_root: &Path,
    commit_id: &str,
    relaunch: bool,
) -> Result<(), EngineError> {
    run_applier_with_keys(journal_root, commit_id, relaunch, &[])
}

/// Same as [`run_applier`], but verifies the journal against the signed
/// manifest with the embedded public keys before executing it.
pub fn run_applier_with_keys(
    journal_root: &Path,
    commit_id: &str,
    relaunch: bool,
    keys: &[String],
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
    super::writer::apply_commit_verified(journal_root, commit_id, keys)?;

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
///
/// This wrapper has no keys and therefore **fails closed**; use
/// [`run_recovery_with_keys`].
pub fn run_recovery(journal_root: &Path, crash_loop_threshold: u32) -> Result<(), EngineError> {
    run_recovery_with_keys(journal_root, crash_loop_threshold, &[])
}

pub fn run_recovery_with_keys(
    journal_root: &Path,
    crash_loop_threshold: u32,
    keys: &[String],
) -> Result<(), EngineError> {
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
    crate::recovery::recover_worker(&config, journal_root, crash_loop_threshold, keys)
}

/// Detect replacement or recovery mode from process arguments. Returns
/// `None` for a normal application start.
pub fn run_applier_from_args() -> Option<i32> {
    run_applier_from_args_with_keys(&[])
}

/// Parse worker arguments and verify the journal against `keys` when provided.
pub fn run_applier_from_args_with_keys(keys: &[String]) -> Option<i32> {
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
        // Each worker process runs one mode only; resetting here keeps the
        // recovery line scoped to this pass instead of inheriting unknown state.
        crate::fs::reset_retry_count();
        crate::worker_log::append(
            Path::new(&journal_root),
            &format!(
                "recovery started journal={} exe={}",
                journal_root.display(),
                current_exe_display()
            ),
        );
        let started = Instant::now();
        return Some(
            match run_recovery_with_keys(Path::new(&journal_root), threshold, keys) {
                Ok(()) => {
                    crate::worker_log::append(
                        Path::new(&journal_root),
                        &format!(
                            "recovery ok in {} ms (retries {})",
                            started.elapsed().as_millis(),
                            crate::fs::retry_count()
                        ),
                    );
                    0
                }
                Err(error) => {
                    crate::worker_log::append(
                        Path::new(&journal_root),
                        &format!(
                            "recovery failed after {} ms (retries {}): {}",
                            started.elapsed().as_millis(),
                            crate::fs::retry_count(),
                            crate::worker_log::oneline(&error.to_string())
                        ),
                    );
                    eprintln!("delta update recovery failed: {error}");
                    1
                }
            },
        );
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
    let journal_path = Path::new(&journal_root);
    crate::fs::reset_retry_count();
    // Write this before any fallible step: if the worker aborts later (panic or
    // `std::process::abort` in a harness), the bundle still shows that it ran.
    crate::worker_log::append(
        journal_path,
        &format!(
            "apply started commit={commit_id} journal={} exe={}",
            journal_root.display(),
            current_exe_display()
        ),
    );
    let started = Instant::now();
    match run_applier_with_keys(journal_path, &commit_id, relaunch, keys) {
        Ok(()) => {
            // Success is silent by design, but support staff still need a
            // durable record that this commit ran and how long it took.
            let version = state::read_commit_status(journal_path)
                .ok()
                .flatten()
                .and_then(|status| match status {
                    state::CommitStatus::Completed { version, .. } => Some(version),
                    _ => None,
                })
                .unwrap_or_else(|| "unknown".into());
            crate::worker_log::append(
                journal_path,
                &format!(
                    "apply commit {commit_id} ok in {} ms (version {version}, retries {})",
                    started.elapsed().as_millis(),
                    crate::fs::retry_count()
                ),
            );
            Some(0)
        }
        Err(error) => {
            crate::worker_log::append(
                journal_path,
                &format!(
                    "apply commit {commit_id} failed after {} ms (retries {}): {}",
                    started.elapsed().as_millis(),
                    crate::fs::retry_count(),
                    crate::worker_log::oneline(&error.to_string())
                ),
            );
            eprintln!("delta update apply failed: {error}");
            Some(1)
        }
    }
}

fn current_exe_display() -> String {
    std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "unknown".into())
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
        // `install\sub\..` would slip past the containment check below once it
        // is resolved, so reject the spelling instead of guessing.
        if path
            .components()
            .any(|part| part == std::path::Component::ParentDir)
        {
            return Err(EngineError::ConfigInvalid(format!(
                "{name} must not contain '..': {}",
                path.display()
            )));
        }
    }
    let install_root = dunce::canonicalize(&config.install_root).map_err(|err| {
        EngineError::ConfigInvalid(format!(
            "install_root ({}) is not accessible: {err}",
            config.install_root.display()
        ))
    })?;
    let exe_parent = exe
        .parent()
        .ok_or_else(|| {
            EngineError::ApplyHelperFailed(format!(
                "worker executable has no parent: {}",
                exe.display()
            ))
        })
        .and_then(|parent| {
            dunce::canonicalize(parent).map_err(|err| {
                EngineError::ApplyHelperFailed(format!(
                    "cannot resolve worker directory ({}): {err}",
                    parent.display()
                ))
            })
        })?;
    if exe_parent != install_root {
        return Err(EngineError::ApplyHelperFailed(format!(
            "install_root ({}) must be the worker executable's directory ({})",
            install_root.display(),
            exe_parent.display()
        )));
    }
    if long_path(&config.staging_root).starts_with(&install_root)
        || long_path(&config.journal_root).starts_with(&install_root)
    {
        return Err(EngineError::ConfigInvalid(format!(
            "staging_root ({}) and journal_root ({}) must stay outside install_root ({})",
            config.staging_root.display(),
            config.journal_root.display(),
            install_root.display()
        )));
    }
    Ok(())
}

/// Canonical form of `path` even when it does not exist yet: resolve the
/// nearest existing ancestor and rejoin the missing tail.
///
/// `install_root` is canonicalized above, so comparing it against a raw
/// `staging_root` silently fails open wherever the same directory has two
/// spellings — most visibly on CI runners, where `%TEMP%` sits behind an 8.3
/// short name (`RUNNER~1`).
fn long_path(path: &Path) -> std::path::PathBuf {
    if let Ok(resolved) = dunce::canonicalize(path) {
        return resolved;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => {
            long_path(parent).join(name)
        }
        _ => path.to_path_buf(),
    }
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
    wait_for_parent_with_limit(pid, PARENT_WAIT_LIMIT)
}

fn wait_for_parent_with_limit(pid: u32, limit: Duration) -> Result<(), EngineError> {
    if pid == 0 || pid == std::process::id() {
        return Ok(());
    }
    let target = Pid::from_u32(pid);
    let mut system = System::new();
    let deadline = Instant::now() + limit;
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
    fn long_path_resolves_a_directory_that_does_not_exist_yet() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("staging");
        let expected = dunce::canonicalize(temp.path()).unwrap().join("staging");
        assert_eq!(long_path(&missing), expected);
    }

    #[test]
    fn parent_components_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("install");
        std::fs::create_dir_all(&install).unwrap();
        let exe = install.join("app.exe");
        let sneaky = install.join("sub").join("..").join("staging");
        let config = apply_config(&install, sneaky, PathBuf::from("journal"));
        assert!(validate_scope(&exe, &config).is_err());
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

    #[cfg(windows)]
    #[test]
    fn parent_wait_timeout_reports_a_helper_failure() {
        // A child that deliberately outlives the wait limit proves the worker
        // gives up instead of hanging forever behind a wedged parent.
        let mut child = Command::new("cmd")
            .args(["/c", "ping -n 30 127.0.0.1 > nul"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let error = wait_for_parent_with_limit(child.id(), Duration::from_millis(50)).unwrap_err();
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            matches!(error, EngineError::ApplyHelperFailed(_)),
            "unexpected error: {error:?}"
        );
    }
}
