mod common;

use delta_update_engine::journal::state;
use delta_update_engine::journal::{Journal, JournalOutcome, ReplayContext};
use delta_update_engine::{CommitStatus, DirectWriter, PrivilegedWriter, StartupOutcome};

use common::{apply, fixture, ADDED_FILE, NEW_APP, OLD_APP, REMOVED_FILE, VERSION};

/// A worker that never started must not leave a commit journal behind: the
/// next start has to reuse PREPARED instead of replaying it unelevated.
#[test]
fn discarded_pending_commit_keeps_prepared_plan() {
    let fixture = fixture();
    fixture.engine.begin_commit(&fixture.prepared).unwrap();
    assert!(fixture.journal().join("commit.journal").exists());
    assert!(fixture.journal().join("apply.json").exists());

    fixture.engine.discard_pending_commit().unwrap();
    assert!(!fixture.journal().join("commit.journal").exists());
    assert!(!fixture.journal().join("apply.json").exists());
    assert!(fixture.journal().join("prepared.json").exists());

    match fixture.engine.recover_and_cleanup().unwrap() {
        StartupOutcome::ReusedPrepared { version } => assert_eq!(version, VERSION),
        other => panic!("unexpected outcome: {other:?}"),
    }

    // A journal that already ran is left to the normal recovery path.
    let pending = fixture.engine.begin_commit(&fixture.prepared).unwrap();
    let config = state::read_apply_config(&fixture.journal())
        .unwrap()
        .unwrap();
    DirectWriter
        .apply_journal(&config, &pending.journal_path)
        .unwrap();
    fixture.engine.discard_pending_commit().unwrap();
    assert!(fixture.journal().join("commit.journal").exists());
    assert!(fixture.journal().join("commit.status").exists());
}

#[test]
fn commit_replaces_adds_and_trashes_files() {
    let fixture = fixture();
    let root = fixture.root();
    let (_config, pending, _) = apply(&fixture);

    assert_eq!(std::fs::read(root.join("App.exe")).unwrap(), NEW_APP);
    assert_eq!(std::fs::read(root.join("added.txt")).unwrap(), ADDED_FILE);
    assert!(!root.join("remove.txt").exists());
    assert_eq!(
        std::fs::read(root.join(".old").join(&pending.commit_id).join("App.exe")).unwrap(),
        OLD_APP
    );
    assert_eq!(
        std::fs::read(
            root.join(".trash")
                .join(&pending.commit_id)
                .join("remove.txt")
        )
        .unwrap(),
        REMOVED_FILE
    );

    // Replaying the same journal is a no-op.
    let journal = Journal::decode(&std::fs::read(&pending.journal_path).unwrap()).unwrap();
    let staging = fixture.staging();
    let context = ReplayContext {
        install_root: &root,
        staging_root: &staging,
        journal_root: &fixture.journal(),
        commit_id: &pending.commit_id,
    };
    let outcome = delta_update_engine::journal::replay(&context, &journal).unwrap();
    assert_eq!(outcome, JournalOutcome::Replayed { ops_completed: 4 });
}

#[test]
fn rollback_restores_old_and_removes_new_files() {
    let fixture = fixture();
    let root = fixture.root();
    let (config, pending, _) = apply(&fixture);
    let journal = Journal::decode(&std::fs::read(&pending.journal_path).unwrap()).unwrap();

    delta_update_engine::rollback::rollback(&config, &journal).unwrap();

    assert_eq!(std::fs::read(root.join("App.exe")).unwrap(), OLD_APP);
    assert_eq!(
        std::fs::read(root.join("remove.txt")).unwrap(),
        REMOVED_FILE
    );
    assert!(!root.join("added.txt").exists());
    assert_eq!(
        std::fs::read_to_string(config.journal_root.join("skipped_version")).unwrap(),
        VERSION
    );
    assert_eq!(
        std::fs::read_to_string(config.journal_root.join("attempts")).unwrap(),
        "0"
    );
    assert!(!pending.journal_path.exists());
}

/// An in-flight prepare holds the cross-process lock for the whole download.
/// Health confirmation must still reset the startup counter, otherwise two
/// slow launches would look like a crash loop.
#[test]
fn confirm_healthy_does_not_wait_for_a_prepare_lock() {
    let fixture = fixture();
    let journal = fixture.journal();
    std::fs::write(journal.join("attempts"), "2").unwrap();

    let _prepare_lock =
        delta_update_engine::journal::UpdateLock::acquire(&journal.join("update.lock")).unwrap();
    fixture.engine.confirm_healthy().unwrap();

    assert_eq!(
        std::fs::read_to_string(journal.join("attempts")).unwrap(),
        "0"
    );
}

#[test]
fn recovery_replays_pending_journal() {
    let fixture = fixture();
    let root = fixture.root();
    let pending = fixture.engine.begin_commit(&fixture.prepared).unwrap();
    assert!(!root.join("added.txt").exists());

    let outcome = fixture.engine.recover_and_cleanup().unwrap();
    match outcome {
        StartupOutcome::Recovered { ops_completed, .. } => assert_eq!(ops_completed, 4),
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert_eq!(std::fs::read(root.join("added.txt")).unwrap(), ADDED_FILE);
    assert_eq!(std::fs::read(root.join("App.exe")).unwrap(), NEW_APP);
    let status = state::read_commit_status(&fixture.journal())
        .unwrap()
        .unwrap();
    assert_eq!(
        status,
        CommitStatus::Completed {
            version: VERSION.into(),
            commit_id: pending.commit_id
        }
    );
}

#[test]
fn crash_loop_triggers_rollback_on_third_start() {
    let fixture = fixture();
    let root = fixture.root();
    apply(&fixture);

    assert!(matches!(
        fixture.engine.recover_and_cleanup().unwrap(),
        StartupOutcome::Committed { .. }
    ));
    assert!(matches!(
        fixture.engine.recover_and_cleanup().unwrap(),
        StartupOutcome::Committed { .. }
    ));
    match fixture.engine.recover_and_cleanup().unwrap() {
        StartupOutcome::RolledBack { to_version, .. } => assert_eq!(to_version, "1.0.0"),
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert_eq!(std::fs::read(root.join("App.exe")).unwrap(), OLD_APP);
    assert_eq!(
        std::fs::read(root.join("remove.txt")).unwrap(),
        REMOVED_FILE
    );
    assert!(!root.join("added.txt").exists());
    assert_eq!(
        std::fs::read_to_string(fixture.journal().join("attempts")).unwrap(),
        "0"
    );
}

#[test]
fn confirm_healthy_cleans_up_and_records_known_files() {
    let fixture = fixture();
    let journal = fixture.journal();
    apply(&fixture);
    std::fs::write(journal.join("attempts"), "3").unwrap();

    fixture.engine.confirm_healthy().unwrap();

    assert_eq!(
        std::fs::read_to_string(journal.join("attempts")).unwrap(),
        "0"
    );
    assert!(!journal.join("commit.journal").exists());
    assert!(!journal.join("commit.status").exists());
    assert!(!journal.join("apply.json").exists());
    assert!(!journal.join("prepared.json").exists());
    let known: serde_json::Value =
        serde_json::from_slice(&std::fs::read(journal.join("known_files.json")).unwrap()).unwrap();
    assert_eq!(known["files"], serde_json::json!(["App.exe", "added.txt"]));
}

#[test]
fn replay_resumes_after_partial_commit() {
    let fixture = fixture();
    let root = fixture.root();
    let pending = fixture.engine.begin_commit(&fixture.prepared).unwrap();
    let config = state::read_apply_config(&fixture.journal())
        .unwrap()
        .unwrap();
    let journal = Journal::decode(&std::fs::read(&pending.journal_path).unwrap()).unwrap();
    let staging = config.staging_root.join(VERSION);
    let context = ReplayContext {
        install_root: &config.install_root,
        staging_root: &staging,
        journal_root: &config.journal_root,
        commit_id: &pending.commit_id,
    };

    // Simulate a crash after the first Backup op.
    let partial = Journal::new(
        &journal.commit_id,
        &journal.version,
        &journal.from_version,
        vec![journal.ops[0].clone()],
    );
    assert_eq!(
        delta_update_engine::journal::replay(&context, &partial).unwrap(),
        JournalOutcome::Replayed { ops_completed: 1 }
    );
    assert!(!root.join("App.exe").exists());
    assert!(root
        .join(".old")
        .join(&pending.commit_id)
        .join("App.exe")
        .exists());

    let outcome = delta_update_engine::journal::replay(&context, &journal).unwrap();
    assert_eq!(outcome, JournalOutcome::Replayed { ops_completed: 4 });
    assert_eq!(std::fs::read(root.join("App.exe")).unwrap(), NEW_APP);
}

#[test]
fn corrupt_journal_needs_attention() {
    let fixture = fixture();
    let journal = fixture.journal().join("commit.journal");
    std::fs::write(&journal, b"not a journal").unwrap();

    match fixture.engine.recover_and_cleanup().unwrap() {
        StartupOutcome::NeedsAttention { error } => {
            assert_eq!(error.code(), "E_JOURNAL_CORRUPT");
        }
        other => panic!("unexpected outcome: {other:?}"),
    }
}

#[test]
fn install_writes_are_only_required_for_pending_or_expired_commits() {
    let fixture = fixture();
    assert!(!fixture.engine.recovery_needs_install_write().unwrap());

    apply(&fixture);
    std::fs::write(fixture.journal().join("attempts"), "1").unwrap();
    assert!(!fixture.engine.recovery_needs_install_write().unwrap());

    std::fs::write(fixture.journal().join("attempts"), "2").unwrap();
    assert!(fixture.engine.recovery_needs_install_write().unwrap());
}

#[test]
fn recovery_after_elevation_reuses_the_startup_attempt() {
    let fixture = fixture();
    apply(&fixture);
    std::fs::write(fixture.journal().join("attempts"), "1").unwrap();

    match fixture.engine.recover_after_elevation().unwrap() {
        StartupOutcome::Committed { version } => assert_eq!(version, VERSION),
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert_eq!(
        std::fs::read_to_string(fixture.journal().join("attempts")).unwrap(),
        "1"
    );
}
