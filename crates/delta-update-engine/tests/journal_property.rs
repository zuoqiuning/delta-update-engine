//! Replay must reach the same tree from any interruption point, and replaying
//! an already finished journal must change nothing.

use std::collections::BTreeMap;
use std::path::Path;

use delta_update_engine::crypto::{sha256_bytes, sha256_file};
use delta_update_engine::journal::format::{FileOp, Journal};
use delta_update_engine::journal::{replay, JournalOutcome, ReplayContext};
use proptest::prelude::*;

const COMMIT: &str = "commit-property";
const REMOVED: &[u8] = b"removed by the update";

fn operations(new: &[u8]) -> Vec<FileOp> {
    vec![
        FileOp::Backup {
            src: "app.exe".into(),
            dst: format!(".old/{COMMIT}/app.exe"),
        },
        FileOp::Place {
            staging_src: "app.exe".into(),
            dst: "app.exe".into(),
            sha256: sha256_bytes(new),
            size: new.len() as u64,
        },
        FileOp::Trash {
            src: "remove.txt".into(),
            trash_dst: "remove.txt".into(),
        },
        FileOp::RemoveTrash {
            trash_path: "remove.txt".into(),
        },
    ]
}

fn snapshot(root: &Path) -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry.unwrap();
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        files.insert(rel, sha256_file(entry.path()).unwrap());
    }
    files
}

/// Build a tree, optionally replay only `split` operations, then replay the
/// full journal twice. Returns the tree after the last replay.
fn run_case(old: &[u8], new: &[u8], keep: &[u8], split: Option<usize>) -> BTreeMap<String, String> {
    let dir = tempfile::tempdir().unwrap();
    let install = dir.path().join("install");
    let staging = dir.path().join("staging");
    std::fs::create_dir_all(&install).unwrap();
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::write(install.join("app.exe"), old).unwrap();
    std::fs::write(install.join("keep.txt"), keep).unwrap();
    std::fs::write(install.join("remove.txt"), REMOVED).unwrap();
    std::fs::write(staging.join("app.exe"), new).unwrap();

    let ops = operations(new);
    let context = ReplayContext {
        install_root: &install,
        staging_root: &staging,
        journal_root: &dir.path().join("journal"),
        commit_id: COMMIT,
    };

    if let Some(split) = split {
        let prefix = Journal::new(COMMIT, "2.0.0", "1.0.0", ops[..split].to_vec());
        replay(&context, &prefix).unwrap();
    }

    let full = Journal::new(COMMIT, "2.0.0", "1.0.0", ops);
    let first = replay(&context, &full).unwrap();
    assert!(matches!(
        first,
        JournalOutcome::Replayed { .. } | JournalOutcome::AlreadyCompleted
    ));

    let before = snapshot(&install);
    // Idempotence: every operation is already done, so nothing may change.
    let second = replay(&context, &full).unwrap();
    assert!(matches!(
        second,
        JournalOutcome::Replayed { .. } | JournalOutcome::AlreadyCompleted
    ));
    assert_eq!(
        snapshot(&install),
        before,
        "a second replay changed the tree"
    );

    assert_eq!(std::fs::read(install.join("app.exe")).unwrap(), new);
    assert_eq!(std::fs::read(install.join("keep.txt")).unwrap(), keep);
    assert!(!install.join("remove.txt").exists());
    assert!(install.join(".old").join(COMMIT).join("app.exe").exists());
    assert!(!install
        .join(".trash")
        .join(COMMIT)
        .join("remove.txt")
        .exists());
    before
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    #[test]
    fn replay_converges_from_every_interruption_point(
        old in prop::collection::vec(any::<u8>(), 1..48),
        new in prop::collection::vec(any::<u8>(), 1..48),
        keep in prop::collection::vec(any::<u8>(), 0..48),
        split in 0usize..=4,
    ) {
        prop_assume!(old != new);
        let expected = run_case(&old, &new, &keep, None);
        let actual = run_case(&old, &new, &keep, Some(split));
        prop_assert_eq!(actual, expected);
    }
}
