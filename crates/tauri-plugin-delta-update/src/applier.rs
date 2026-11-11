use std::path::Path;
use std::process::{Command, Stdio};
#[cfg(windows)]
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use delta_update_engine::EngineError;

/// Windows `CREATE_NO_WINDOW`: the worker has no console and must not flash one.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `ShellExecute` returns this when the user declines the UAC prompt.
#[cfg(windows)]
const ERROR_CANCELLED: usize = 1223;

/// Startup must not hang indefinitely on a stuck elevated worker.
#[cfg(windows)]
const RECOVERY_TIMEOUT_MS: u32 = 120_000;

/// Start the same signed binary in replacement mode.
///
/// The child waits for the parent PID to exit, then reads the journal from
/// disk. There is no IPC between the two processes. `relaunch` starts the new
/// binary again after the replacement (the `restart_to_apply` path).
///
/// A per-machine install (`Program Files`) is not writable by the current
/// user, so the worker is started through `ShellExecuteW("runas")` and the UAC
/// prompt appears after the app exits. Per-user installs spawn the worker
/// directly, with no prompt.
pub fn spawn_replacement(
    install_root: &Path,
    journal_root: &Path,
    commit_id: &str,
    relaunch: bool,
) -> Result<(), EngineError> {
    let exe = std::env::current_exe()
        .map_err(|err| EngineError::ApplyHelperFailed(format!("current exe: {err}")))?;
    let args = worker_args(journal_root, commit_id, relaunch);

    if needs_elevation(install_root)? {
        #[cfg(windows)]
        return spawn_elevated(&exe, &args);
    }
    spawn_direct(&exe, &args)
}

/// Run startup recovery in an elevated worker and wait for it to finish.
///
/// Used when a per-machine install left a journal that the current user token
/// cannot replay. The worker is the same signed binary in
/// `--delta-update-recover` mode; the UAC prompt appears during startup,
/// before the main window.
pub(crate) fn run_elevated_recovery(
    journal_root: &Path,
    crash_loop_threshold: u32,
) -> Result<(), EngineError> {
    #[cfg(windows)]
    {
        let exe = std::env::current_exe()
            .map_err(|err| EngineError::ApplyHelperFailed(format!("current exe: {err}")))?;
        let args = vec![
            "--delta-update-recover".to_string(),
            journal_root.display().to_string(),
            crash_loop_threshold.to_string(),
        ];
        spawn_elevated_and_wait(&exe, &args, RECOVERY_TIMEOUT_MS)
    }
    #[cfg(not(windows))]
    {
        let _ = (journal_root, crash_loop_threshold);
        Err(EngineError::ApplyHelperFailed(
            "elevated recovery is only supported on Windows".into(),
        ))
    }
}

fn worker_args(journal_root: &Path, commit_id: &str, relaunch: bool) -> Vec<String> {
    let mut args = vec![
        "--delta-update-apply".to_string(),
        journal_root.display().to_string(),
        commit_id.to_string(),
    ];
    if relaunch {
        args.push("--relaunch".into());
    }
    args
}

fn spawn_direct(exe: &Path, args: &[String]) -> Result<(), EngineError> {
    let mut command = Command::new(exe);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
        .spawn()
        .map_err(|err| EngineError::ApplyHelperFailed(format!("spawn: {err}")))?;
    Ok(())
}

/// Whether the current token cannot write the install root.
///
/// The probe creates a real file instead of inspecting ACLs: it also catches
/// read-only shares, group policy and per-directory deny entries. A transient
/// lock is retried once before deciding to elevate.
#[cfg(not(windows))]
pub(crate) fn needs_elevation(_install_root: &Path) -> Result<bool, EngineError> {
    Ok(false)
}

#[cfg(windows)]
pub(crate) fn needs_elevation(install_root: &Path) -> Result<bool, EngineError> {
    let mut last = None;
    for attempt in 0..2 {
        let path = install_root.join(probe_name(attempt));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => {
                drop(file);
                let _ = std::fs::remove_file(&path);
                return Ok(false);
            }
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                last = Some(err);
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                last = Some(err);
            }
            Err(err) => {
                return Err(EngineError::ApplyHelperFailed(format!(
                    "cannot probe install root {}: {err}",
                    install_root.display()
                )))
            }
        }
    }
    match last {
        Some(err) if err.kind() == std::io::ErrorKind::PermissionDenied => Ok(true),
        Some(err) => Err(EngineError::ApplyHelperFailed(format!(
            "cannot probe install root {}: {err}",
            install_root.display()
        ))),
        None => Ok(false),
    }
}

#[cfg(windows)]
fn probe_name(attempt: u32) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!(
        ".delta-update-probe-{}-{nanos}-{attempt}",
        std::process::id()
    )
}

/// Start the worker elevated so it can write a per-machine install.
#[cfg(windows)]
#[allow(unsafe_code)]
fn spawn_elevated(exe: &Path, args: &[String]) -> Result<(), EngineError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;

    let file: Vec<u16> = exe
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let operation: Vec<u16> = "runas".encode_utf16().chain(std::iter::once(0)).collect();
    let parameters = args
        .iter()
        .map(|arg| quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let parameters: Vec<u16> = parameters
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: every pointer references a NUL-terminated UTF-16 buffer that
    // outlives the call; no output buffer is written by ShellExecuteW.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            parameters.as_ptr(),
            std::ptr::null(),
            0, // SW_HIDE
        )
    };
    let code = result as usize;
    if code > 32 {
        return Ok(());
    }
    if code == ERROR_CANCELLED {
        return Err(EngineError::UacCancelled);
    }
    Err(EngineError::ApplyHelperFailed(format!(
        "ShellExecuteW(runas) failed with code {code}"
    )))
}

/// Start the recovery worker elevated and wait for it to exit.
///
/// `ShellExecuteExW` is used instead of `ShellExecuteW` because recovery must
/// finish before the app reads its own state: it is the process image that may
/// be replaced or rolled back.
#[cfg(windows)]
#[allow(unsafe_code)]
fn spawn_elevated_and_wait(
    exe: &Path,
    args: &[String],
    timeout_ms: u32,
) -> Result<(), EngineError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::WaitForSingleObject;
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };

    let file: Vec<u16> = exe
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let operation: Vec<u16> = "runas".encode_utf16().chain(std::iter::once(0)).collect();
    let parameters = args
        .iter()
        .map(|arg| quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let parameters: Vec<u16> = parameters
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: operation.as_ptr(),
        lpFile: file.as_ptr(),
        lpParameters: parameters.as_ptr(),
        nShow: 0, // SW_HIDE
        ..SHELLEXECUTEINFOW::default()
    };

    // SAFETY: every pointer references a NUL-terminated UTF-16 buffer that
    // outlives the call; ShellExecuteExW only writes into `info`.
    let started = unsafe { ShellExecuteExW(&mut info) };
    if started == 0 {
        let code = unsafe { GetLastError() };
        if code == ERROR_CANCELLED as u32 {
            return Err(EngineError::UacCancelled);
        }
        return Err(EngineError::ApplyHelperFailed(format!(
            "ShellExecuteExW(runas) failed with code {code}"
        )));
    }
    if info.hProcess.is_null() {
        return Err(EngineError::ApplyHelperFailed(
            "elevated recovery returned no process handle".into(),
        ));
    }

    // SAFETY: `hProcess` is a valid handle because SEE_MASK_NOCLOSEPROCESS was
    // requested and ShellExecuteExW succeeded; it is closed exactly once.
    let wait = unsafe { WaitForSingleObject(info.hProcess, timeout_ms) };
    unsafe { CloseHandle(info.hProcess) };
    match wait {
        WAIT_OBJECT_0 => Ok(()),
        WAIT_TIMEOUT => Err(EngineError::ApplyHelperFailed(format!(
            "elevated recovery did not finish within {timeout_ms} ms"
        ))),
        other => Err(EngineError::ApplyHelperFailed(format!(
            "waiting for elevated recovery failed with code {other}"
        ))),
    }
}

#[cfg(windows)]
fn quote(arg: &str) -> String {
    if arg.contains([' ', '\t']) {
        format!("\"{arg}\"")
    } else {
        arg.to_string()
    }
}
