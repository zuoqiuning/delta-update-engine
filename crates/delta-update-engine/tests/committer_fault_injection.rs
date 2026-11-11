mod common;

use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use delta_update_engine::journal::{state, UpdateLock};
use delta_update_engine::{ApplyConfig, CommitStatus, DirectWriter, PendingCommit, StartupOutcome};
use delta_update_testkit::FaultyFs;

use common::{apply, fixture, fixture_with_threshold, ADDED_FILE, NEW_APP, OLD_APP, REMOVED_FILE};

/// C-01: the worker replaces its own running image. The old executable is
/// renamed into the backup directory while the process keeps running from
/// memory, and the new file lands at the original path.
#[test]
fn commit_replaces_a_running_executable() {
    let fixture = fixture();
    let root = fixture.root();

    let (sleeper, args) = sleeper();
    std::fs::copy(&sleeper, root.join("App.exe")).expect("copy running image");
    let running_image = std::fs::read(root.join("App.exe")).expect("read running image");

    let mut child = ChildGuard(
        Command::new(root.join("App.exe"))
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn running image"),
    );
    // Give the loader time to map the image before the commit touches it.
    std::thread::sleep(Duration::from_millis(300));

    let (_config, pending, _) = apply(&fixture);

    assert_eq!(std::fs::read(root.join("App.exe")).unwrap(), NEW_APP);
    assert_eq!(
        std::fs::read(root.join(".old").join(&pending.commit_id).join("App.exe")).unwrap(),
        running_image
    );
    assert!(
        child.0.try_wait().unwrap().is_none(),
        "the old image must keep running after replacement"
    );
}

/// Kill a spawned helper even when an assertion unwinds.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(windows)]
fn sleeper() -> (PathBuf, &'static [&'static str]) {
    let windir = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    (
        PathBuf::from(windir).join("System32").join("ping.exe"),
        &["-n", "30", "127.0.0.1"],
    )
}

#[cfg(not(windows))]
fn sleeper() -> (PathBuf, &'static [&'static str]) {
    (PathBuf::from("/bin/sleep"), &["30"])
}

fn begin(fixture: &common::Fixture) -> (ApplyConfig, PendingCommit) {
    let pending = fixture.engine.begin_commit(&fixture.prepared).unwrap();
    let config = state::read_apply_config(&fixture.journal())
        .unwrap()
        .expect("apply.json");
    (config, pending)
}

fn assert_failed_status(fixture: &common::Fixture) {
    assert!(
        matches!(
            state::read_commit_status(&fixture.journal()).unwrap(),
            Some(CommitStatus::Failed { .. })
        ),
        "the worker must record the failure"
    );
}

/// C-03: a disk-full rename midway through the journal is retried on the next
/// start, so a transient failure does not leave a mixed install.
#[test]
fn disk_full_during_commit_is_retried_on_next_start() {
    let fixture = fixture();
    let (config, pending) = begin(&fixture);
    let app = fixture.root().join("App.exe");
    let faulty = FaultyFs::new().fail_on_rename(&app, io::ErrorKind::Other);

    let error = DirectWriter
        .apply_journal_with_fs(&config, &pending.journal_path, &faulty)
        .unwrap_err();
    assert_eq!(error.code(), "E_JOURNAL_REPLAY_FAILED");
    assert_failed_status(&fixture);
    // The backup was taken before the failing place.
    assert!(fixture
        .root()
        .join(".old")
        .join(&pending.commit_id)
        .join("App.exe")
        .exists());
    assert!(!app.exists());

    match fixture.engine.recover_and_cleanup().unwrap() {
        StartupOutcome::Recovered { version, .. } => assert_eq!(version, common::VERSION),
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert_eq!(std::fs::read(app).unwrap(), NEW_APP);
    assert_eq!(
        std::fs::read(fixture.root().join("added.txt")).unwrap(),
        ADDED_FILE
    );
    assert!(!fixture.root().join("remove.txt").exists());
}

/// C-04: a permission failure stops the replay without unwinding; the retry
/// runs once the permission is available again.
#[test]
fn permission_denied_during_commit_is_retried_on_next_start() {
    let fixture = fixture();
    let (config, pending) = begin(&fixture);
    let app = fixture.root().join("App.exe");
    let faulty = FaultyFs::new().fail_on_rename(&app, io::ErrorKind::PermissionDenied);

    let error = DirectWriter
        .apply_journal_with_fs(&config, &pending.journal_path, &faulty)
        .unwrap_err();
    assert_eq!(error.code(), "E_JOURNAL_REPLAY_FAILED");
    match state::read_commit_status(&fixture.journal()).unwrap() {
        Some(CommitStatus::Failed { message, .. }) => {
            assert!(message.contains("injected failure"), "{message}");
        }
        other => panic!("unexpected status: {other:?}"),
    }

    match fixture.engine.recover_and_cleanup().unwrap() {
        StartupOutcome::Recovered { version, .. } => assert_eq!(version, common::VERSION),
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert_eq!(std::fs::read(app).unwrap(), NEW_APP);
    assert!(fixture.root().join("added.txt").exists());
}

/// C-06: a target that fails verification after placement is removed; the next
/// start cannot complete the journal and rolls back to the previous version.
#[test]
fn corrupted_target_after_place_rolls_back() {
    let fixture = fixture();
    let (config, pending) = begin(&fixture);
    let app = fixture.root().join("App.exe");
    let faulty = FaultyFs::new().corrupt_on_rename(&app);

    let error = DirectWriter
        .apply_journal_with_fs(&config, &pending.journal_path, &faulty)
        .unwrap_err();
    assert_eq!(error.code(), "E_JOURNAL_REPLAY_FAILED");
    assert_failed_status(&fixture);
    assert!(!app.exists(), "the corrupted target must be removed");

    match fixture.engine.recover_and_cleanup().unwrap() {
        StartupOutcome::RolledBack { to_version, .. } => assert_eq!(to_version, "1.0.0"),
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert_eq!(
        std::fs::read(fixture.root().join("App.exe")).unwrap(),
        OLD_APP
    );
    assert!(!fixture.root().join("added.txt").exists());
    assert!(fixture.root().join("remove.txt").exists());
    assert_eq!(
        state::read_skipped_version(&fixture.journal()).as_deref(),
        Some(common::VERSION)
    );
    assert!(!fixture
        .root()
        .join(".old")
        .join(&pending.commit_id)
        .exists());
    assert!(!fixture.staging().exists());
}

/// C-02: a backup directory left by a previous commit must not collide with a
/// new commit and must survive the new confirmation.
#[test]
fn leftover_backup_directory_is_isolated() {
    let fixture = fixture();
    let leftover = fixture.root().join(".old").join("previous-commit");
    std::fs::create_dir_all(&leftover).unwrap();
    std::fs::write(leftover.join("App.exe"), b"older-backup").unwrap();

    let (_config, pending, _) = apply(&fixture);
    assert_eq!(
        std::fs::read(
            fixture
                .root()
                .join(".old")
                .join(&pending.commit_id)
                .join("App.exe")
        )
        .unwrap(),
        OLD_APP
    );
    assert!(leftover.join("App.exe").exists());

    fixture.engine.confirm_healthy().unwrap();
    assert!(!fixture
        .root()
        .join(".old")
        .join(&pending.commit_id)
        .exists());
    assert!(leftover.join("App.exe").exists());
}

/// C-05: the trash directory does not exist before the commit and is created
/// on demand; removed files are preserved there.
#[test]
fn trash_directory_is_created_on_demand() {
    let fixture = fixture();
    let root = fixture.root();
    assert!(!root.join(".trash").exists());

    let (_config, pending, _) = apply(&fixture);

    assert_eq!(
        std::fs::read(
            root.join(".trash")
                .join(&pending.commit_id)
                .join("remove.txt")
        )
        .unwrap(),
        REMOVED_FILE
    );
}

/// C-07: a second commit cannot acquire the update lock.
#[test]
fn concurrent_commit_is_rejected() {
    let fixture = fixture();
    let pending = fixture.engine.begin_commit(&fixture.prepared).unwrap();
    let _held = UpdateLock::acquire(&fixture.journal().join("update.lock")).unwrap();

    let error = delta_update_engine::apply::apply_commit(&fixture.journal(), &pending.commit_id)
        .unwrap_err();
    assert_eq!(error.code(), "E_UPDATE_LOCKED");

    drop(_held);
    delta_update_engine::apply::apply_commit(&fixture.journal(), &pending.commit_id).unwrap();
    assert_eq!(
        std::fs::read(fixture.root().join("App.exe")).unwrap(),
        NEW_APP
    );
    assert_eq!(
        std::fs::read(fixture.root().join("added.txt")).unwrap(),
        ADDED_FILE
    );
}

/// P-13: committing without a persisted PREPARED plan must not exit the app
/// into a half-written commit.
#[test]
fn commit_without_prepared_plan_is_rejected() {
    let fixture = fixture();
    std::fs::remove_file(fixture.journal().join("prepared.json")).unwrap();

    let error = fixture.engine.begin_commit(&fixture.prepared).unwrap_err();
    assert_eq!(error.code(), "E_NOT_PREPARED");
    assert!(!fixture.journal().join("commit.journal").exists());
    assert!(!fixture.journal().join("apply.json").exists());
}

/// R-09: once health is confirmed the journal is gone, so later crashes never
/// roll the update back.
#[test]
fn confirmed_update_is_not_rolled_back() {
    let fixture = fixture();
    apply(&fixture);
    fixture.engine.confirm_healthy().unwrap();

    for _ in 0..3 {
        match fixture.engine.recover_and_cleanup().unwrap() {
            StartupOutcome::Clean => {}
            other => panic!("unexpected outcome: {other:?}"),
        }
    }
    assert_eq!(
        std::fs::read(fixture.root().join("App.exe")).unwrap(),
        NEW_APP
    );
    assert_eq!(
        std::fs::read(fixture.root().join("added.txt")).unwrap(),
        ADDED_FILE
    );
}

/// R-10: a threshold of 1 rolls back on the second unconfirmed start.
#[test]
fn threshold_one_rolls_back_on_second_start() {
    let fixture = fixture_with_threshold(1);
    apply(&fixture);

    assert!(matches!(
        fixture.engine.recover_and_cleanup().unwrap(),
        StartupOutcome::Committed { .. }
    ));
    match fixture.engine.recover_and_cleanup().unwrap() {
        StartupOutcome::RolledBack { to_version, .. } => assert_eq!(to_version, "1.0.0"),
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert_eq!(
        std::fs::read(fixture.root().join("App.exe")).unwrap(),
        OLD_APP
    );
    assert!(!fixture.root().join("added.txt").exists());
}
