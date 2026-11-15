mod common;

use delta_update_engine::journal::state::{self, ApplyConfig};
use delta_update_engine::journal::{replay, Journal, JournalOutcome, ReplayContext};
use delta_update_engine::ErrorAction;
use delta_update_engine::StartupOutcome;

use common::{apply, fixture, ADDED_FILE, NEW_APP, OLD_APP, REMOVED_FILE, VERSION};

/// J-01: no journal exists, so recovery must not touch the install root.
#[test]
fn no_journal_leaves_root_untouched() {
    let fixture = fixture();
    let root = fixture.root();

    // A PREPARED plan from before `begin_commit` may still exist; either way
    // the root must not be touched.
    match fixture.engine.recover_and_cleanup().unwrap() {
        StartupOutcome::Clean | StartupOutcome::ReusedPrepared { .. } => {}
        other => panic!("unexpected outcome: {other:?}"),
    }

    assert_eq!(std::fs::read(root.join("App.exe")).unwrap(), OLD_APP);
    assert_eq!(
        std::fs::read(root.join("remove.txt")).unwrap(),
        REMOVED_FILE
    );
    assert!(!root.join("added.txt").exists());
    assert!(!fixture.journal().join("commit.journal").exists());
}

/// J-02: the journal is durable before any op runs; recovery replays all ops.
#[test]
fn journal_before_first_op_replays_forward() {
    let (fixture, _config, journal) = journal_fixture();

    match fixture.engine.recover_and_cleanup().unwrap() {
        StartupOutcome::Recovered {
            ops_completed,
            version,
        } => {
            assert_eq!(ops_completed, journal.ops.len());
            assert_eq!(version, VERSION);
        }
        other => panic!("unexpected outcome: {other:?}"),
    }

    assert_eq!(
        std::fs::read(fixture.root().join("App.exe")).unwrap(),
        NEW_APP
    );
    assert_eq!(
        std::fs::read(fixture.root().join("added.txt")).unwrap(),
        ADDED_FILE
    );
    assert!(!fixture.root().join("remove.txt").exists());
}

/// J-03: the first backup completed before the crash; replay skips it.
#[test]
fn completed_backup_is_skipped_on_replay() {
    let (fixture, config, journal) = journal_fixture();
    let root = fixture.root();

    assert_eq!(
        replay_prefix(&config, &journal, 1),
        JournalOutcome::Replayed { ops_completed: 1 }
    );
    assert!(!root.join("App.exe").exists());
    assert_eq!(
        std::fs::read(root.join(".old").join(&journal.commit_id).join("App.exe")).unwrap(),
        OLD_APP
    );

    assert_eq!(
        replay_with(&config, &journal),
        JournalOutcome::Replayed {
            ops_completed: journal.ops.len()
        }
    );
    assert_eq!(std::fs::read(root.join("App.exe")).unwrap(), NEW_APP);
    assert_eq!(std::fs::read(root.join("added.txt")).unwrap(), ADDED_FILE);
    assert!(!root.join("remove.txt").exists());
}

/// J-04: an op whose rename landed before the crash is detected by hash and
/// skipped even though its staging source is already gone.
#[test]
fn landed_op_is_skipped_on_replay() {
    let (fixture, config, journal) = journal_fixture();
    let root = fixture.root();

    assert_eq!(
        replay_prefix(&config, &journal, 2),
        JournalOutcome::Replayed { ops_completed: 2 }
    );
    // Op index 2 is `Place added.txt`; simulate it landing before the crash.
    std::fs::rename(
        fixture.staging().join("files/added.txt"),
        root.join("added.txt"),
    )
    .unwrap();

    assert_eq!(
        replay_with(&config, &journal),
        JournalOutcome::Replayed {
            ops_completed: journal.ops.len()
        }
    );
    assert_eq!(std::fs::read(root.join("added.txt")).unwrap(), ADDED_FILE);
    assert_eq!(std::fs::read(root.join("App.exe")).unwrap(), NEW_APP);
    assert!(!root.join("remove.txt").exists());
}

/// J-05: all ops landed but `commit.status` was never written; recovery
/// replays idempotently and marks the commit completed.
#[test]
fn all_ops_without_status_are_replayed() {
    let (fixture, config, journal) = journal_fixture();
    let journal_root = fixture.journal();

    assert_eq!(
        replay_with(&config, &journal),
        JournalOutcome::Replayed {
            ops_completed: journal.ops.len()
        }
    );
    assert!(!journal_root.join("commit.status").exists());

    match fixture.engine.recover_and_cleanup().unwrap() {
        StartupOutcome::Recovered { ops_completed, .. } => {
            assert_eq!(ops_completed, journal.ops.len())
        }
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert!(matches!(
        state::read_commit_status(&journal_root).unwrap(),
        Some(delta_update_engine::CommitStatus::Completed { .. })
    ));
    assert!(journal_root.join("commit.journal").exists());
    assert_eq!(
        std::fs::read(fixture.root().join("App.exe")).unwrap(),
        NEW_APP
    );
}

/// J-06: an interrupted atomic status write only leaves a temp file; recovery
/// treats the commit as unfinished and rewrites a complete status.
#[test]
fn status_temp_file_is_ignored() {
    let (fixture, config, journal) = journal_fixture();
    let journal_root = fixture.journal();

    assert_eq!(
        replay_with(&config, &journal),
        JournalOutcome::Replayed {
            ops_completed: journal.ops.len()
        }
    );
    std::fs::write(journal_root.join("commit.status.tmp-stale"), b"partial").unwrap();

    match fixture.engine.recover_and_cleanup().unwrap() {
        StartupOutcome::Recovered { .. } => {}
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert!(matches!(
        state::read_commit_status(&journal_root).unwrap(),
        Some(delta_update_engine::CommitStatus::Completed { .. })
    ));
}

/// J-07: a completed commit is not replayed and its journal is retained until
/// health confirmation.
#[test]
fn completed_commit_waits_for_confirmation() {
    let fixture = fixture();
    let journal_root = fixture.journal();
    apply(&fixture);

    match fixture.engine.recover_and_cleanup().unwrap() {
        StartupOutcome::Committed { version } => assert_eq!(version, VERSION),
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert!(journal_root.join("commit.journal").exists());
    assert!(matches!(
        state::read_commit_status(&journal_root).unwrap(),
        Some(delta_update_engine::CommitStatus::Completed { .. })
    ));

    fixture.engine.confirm_healthy().unwrap();
    assert!(!journal_root.join("commit.journal").exists());
    assert!(!journal_root.join("commit.status").exists());
    assert!(!fixture.staging().exists());
}

/// J-08: confirmation removes the journal first. An interruption after that
/// unlink must still report the committed version instead of offering a stale
/// PREPARED plan.
#[test]
fn journal_removed_after_completion_reports_committed() {
    let fixture = fixture();
    let journal_root = fixture.journal();
    apply(&fixture);
    std::fs::remove_file(journal_root.join("commit.journal")).unwrap();

    match fixture.engine.recover_and_cleanup().unwrap() {
        StartupOutcome::Committed { version } => assert_eq!(version, VERSION),
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert_eq!(
        std::fs::read(fixture.root().join("App.exe")).unwrap(),
        NEW_APP
    );

    fixture.engine.confirm_healthy().unwrap();
    assert!(!journal_root.join("commit.status").exists());
    assert!(!journal_root.join("prepared.json").exists());
}

/// J-09: a journal that fails its checksum is never replayed.
#[test]
fn corrupt_journal_is_not_replayed() {
    let fixture = fixture();
    let journal_root = fixture.journal();
    fixture.engine.begin_commit(&fixture.prepared).unwrap();
    std::fs::write(journal_root.join("commit.journal"), b"TDJ1 broken").unwrap();

    match fixture.engine.recover_and_cleanup().unwrap() {
        StartupOutcome::NeedsAttention { error } => {
            assert_eq!(error.code(), "E_JOURNAL_CORRUPT")
        }
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert_eq!(
        std::fs::read(fixture.root().join("App.exe")).unwrap(),
        OLD_APP
    );
    assert!(!fixture.root().join(".old").exists());
}

/// J-09b: a journal written by a newer client is reported as an upgrade
/// requirement (`E_JOURNAL_FORMAT_TOO_NEW`), not as corruption, and the root is
/// left untouched.
#[test]
fn future_journal_format_requires_a_client_update() {
    let fixture = fixture();
    let journal_root = fixture.journal();
    fixture.engine.begin_commit(&fixture.prepared).unwrap();
    let path = journal_root.join("commit.journal");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[4..6].copy_from_slice(&2u16.to_le_bytes());
    std::fs::write(&path, bytes).unwrap();

    match fixture.engine.recover_and_cleanup().unwrap() {
        StartupOutcome::NeedsAttention { error } => {
            assert_eq!(error.code(), "E_JOURNAL_FORMAT_TOO_NEW");
            assert_eq!(error.action(), ErrorAction::UpdateClient);
        }
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert_eq!(
        std::fs::read(fixture.root().join("App.exe")).unwrap(),
        OLD_APP
    );
    assert!(!fixture.root().join(".old").exists());
}

/// J-10: a replay that fails because a staged source disappeared can be
/// retried once the source is available again.
#[test]
fn replay_failure_can_be_retried() {
    let (fixture, config, journal) = journal_fixture();
    let root = fixture.root();
    let staged_app = fixture.staging().join("files/App.exe");

    assert_eq!(
        replay_prefix(&config, &journal, 1),
        JournalOutcome::Replayed { ops_completed: 1 }
    );
    std::fs::remove_file(&staged_app).unwrap();
    match replay_with(&config, &journal) {
        JournalOutcome::ReplayFailed { op_index, .. } => assert_eq!(op_index, 1),
        other => panic!("unexpected outcome: {other:?}"),
    }

    std::fs::write(&staged_app, NEW_APP).unwrap();
    assert_eq!(
        replay_with(&config, &journal),
        JournalOutcome::Replayed {
            ops_completed: journal.ops.len()
        }
    );
    assert_eq!(std::fs::read(root.join("App.exe")).unwrap(), NEW_APP);
}

/// J-11: replaying the same journal three times yields the same result.
#[test]
fn repeated_replay_is_idempotent() {
    let (fixture, config, journal) = journal_fixture();
    let root = fixture.root();

    for _ in 0..3 {
        assert_eq!(
            replay_with(&config, &journal),
            JournalOutcome::Replayed {
                ops_completed: journal.ops.len()
            }
        );
        assert_eq!(std::fs::read(root.join("App.exe")).unwrap(), NEW_APP);
        assert_eq!(
            std::fs::read(root.join(".old").join(&journal.commit_id).join("App.exe")).unwrap(),
            OLD_APP
        );
    }
}

/// J-12: an external edit after the backup does not win; the staged target is
/// re-landed and verified.
#[test]
fn staging_overwrites_external_edit_after_backup() {
    let (fixture, config, journal) = journal_fixture();
    let root = fixture.root();

    assert_eq!(
        replay_prefix(&config, &journal, 1),
        JournalOutcome::Replayed { ops_completed: 1 }
    );
    std::fs::write(root.join("App.exe"), b"external-edit").unwrap();

    assert_eq!(
        replay_with(&config, &journal),
        JournalOutcome::Replayed {
            ops_completed: journal.ops.len()
        }
    );
    assert_eq!(std::fs::read(root.join("App.exe")).unwrap(), NEW_APP);
    assert_eq!(
        std::fs::read(root.join(".old").join(&journal.commit_id).join("App.exe")).unwrap(),
        OLD_APP
    );
}

/// J-10b: a staged file modified after `begin_commit` is rejected by replay
/// itself, not only by the pre-commit check that ran in the original process.
#[test]
fn tampered_staged_source_blocks_replay() {
    let (fixture, config, journal) = journal_fixture();
    std::fs::write(fixture.staging().join("files/App.exe"), b"tampered").unwrap();

    match replay_with(&config, &journal) {
        JournalOutcome::ReplayFailed { op_index, reason } => {
            assert_eq!(op_index, 1);
            assert!(reason.contains("hash mismatch"), "{reason}");
        }
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert!(!fixture.root().join("added.txt").exists());
}

/// J-13: a lock left behind by a process killed mid-update does not block the
/// next startup; recovery reacquires it and finishes the journal.
#[test]
fn stale_lock_after_kill_does_not_block_recovery() {
    let (fixture, _config, journal) = journal_fixture();
    std::fs::write(
        fixture.journal().join("update.lock"),
        br#"{"pid":424242,"started_at_unix":1,"boot_id":1}"#,
    )
    .unwrap();

    match fixture.engine.recover_and_cleanup().unwrap() {
        StartupOutcome::Recovered { ops_completed, .. } => {
            assert_eq!(ops_completed, journal.ops.len())
        }
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert_eq!(
        std::fs::read(fixture.root().join("App.exe")).unwrap(),
        NEW_APP
    );
}

fn journal_fixture() -> (common::Fixture, ApplyConfig, Journal) {
    let fixture = fixture();
    let pending = fixture.engine.begin_commit(&fixture.prepared).unwrap();
    let config = state::read_apply_config(&fixture.journal())
        .unwrap()
        .unwrap();
    let journal = Journal::decode(&std::fs::read(&pending.journal_path).unwrap()).unwrap();
    (fixture, config, journal)
}

fn replay_with(config: &ApplyConfig, journal: &Journal) -> JournalOutcome {
    let staging = config.staging_root.join(&journal.version);
    let context = ReplayContext {
        install_root: &config.install_root,
        staging_root: &staging,
        journal_root: &config.journal_root,
        commit_id: &journal.commit_id,
    };
    replay(&context, journal).unwrap()
}

fn replay_prefix(config: &ApplyConfig, journal: &Journal, count: usize) -> JournalOutcome {
    let partial = Journal::new(
        &journal.commit_id,
        &journal.version,
        &journal.from_version,
        journal.ops[..count].to_vec(),
    );
    replay_with(config, &partial)
}
