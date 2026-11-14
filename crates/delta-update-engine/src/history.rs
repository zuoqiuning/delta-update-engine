//! Persistent, machine-readable update outcomes.
//!
//! `worker.log` is for humans; this JSONL file is for the application (and its
//! monitoring pipeline) to answer "how are updates doing on this machine".
//! Nothing here is uploaded anywhere: the host app decides what to report.

use std::path::Path;

use serde::{Deserialize, Serialize};

pub const HISTORY_FILE: &str = "update_history.jsonl";
const COMMIT_STARTED: &str = "commit_started.json";
const MAX_BYTES: u64 = 256 * 1024;
const KEEP_LINES: usize = 200;

const RECORD_VERSION: u32 = 1;

/// One terminal update outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRecord {
    pub v: u32,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_unix: Option<u64>,
    pub finished_at_unix: u64,
    pub result: UpdateResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateResult {
    Completed,
    RolledBack,
    Failed,
}

/// Written before the journal, consumed by the first terminal outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StartedUpdate {
    pub version: String,
    pub from_version: String,
    pub started_at_unix: u64,
}

/// Read the newest `limit` records (0 means "none"). Malformed lines are
/// skipped so one corrupt append cannot hide the rest.
pub fn read(journal_root: &Path, limit: usize) -> Vec<UpdateRecord> {
    if limit == 0 {
        return Vec::new();
    }
    let Ok(text) = std::fs::read_to_string(journal_root.join(HISTORY_FILE)) else {
        return Vec::new();
    };
    let mut records: Vec<UpdateRecord> = text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    if records.len() > limit {
        records.drain(..records.len() - limit);
    }
    records
}

pub(crate) fn mark_started(journal_root: &Path, version: &str, from_version: &str) {
    let started = StartedUpdate {
        version: version.to_string(),
        from_version: from_version.to_string(),
        started_at_unix: now_unix(),
    };
    let Ok(bytes) = serde_json::to_vec(&started) else {
        return;
    };
    let _ = crate::fs::atomic::write_atomic(&journal_root.join(COMMIT_STARTED), &bytes);
}

pub(crate) fn clear_started(journal_root: &Path) {
    let _ = std::fs::remove_file(journal_root.join(COMMIT_STARTED));
}

pub(crate) fn take_started(journal_root: &Path) -> Option<StartedUpdate> {
    let path = journal_root.join(COMMIT_STARTED);
    let started = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let _ = std::fs::remove_file(&path);
    started
}

pub(crate) fn record_completed(journal_root: &Path, version: &str, from_version: Option<&str>) {
    let started = take_started(journal_root);
    append(
        journal_root,
        build(
            Some(version),
            started
                .as_ref()
                .map(|value| value.from_version.as_str())
                .or(from_version),
            started.as_ref(),
            UpdateResult::Completed,
            None,
            None,
        ),
    );
}

pub(crate) fn record_rolled_back(journal_root: &Path, reason: &str) {
    let started = take_started(journal_root);
    append(
        journal_root,
        build(
            started.as_ref().map(|value| value.version.as_str()),
            started.as_ref().map(|value| value.from_version.as_str()),
            started.as_ref(),
            UpdateResult::RolledBack,
            Some(reason),
            None,
        ),
    );
}

pub(crate) fn record_failed(journal_root: &Path, code: &str) {
    let started = take_started(journal_root);
    append(
        journal_root,
        build(
            started.as_ref().map(|value| value.version.as_str()),
            started.as_ref().map(|value| value.from_version.as_str()),
            started.as_ref(),
            UpdateResult::Failed,
            None,
            Some(code),
        ),
    );
}

fn build(
    version: Option<&str>,
    from_version: Option<&str>,
    started: Option<&StartedUpdate>,
    result: UpdateResult,
    reason: Option<&str>,
    error_code: Option<&str>,
) -> UpdateRecord {
    let finished_at_unix = now_unix();
    UpdateRecord {
        v: RECORD_VERSION,
        version: version.unwrap_or("unknown").to_string(),
        from_version: from_version.map(str::to_string),
        started_at_unix: started.map(|value| value.started_at_unix),
        finished_at_unix,
        result,
        duration_ms: started.map(|value| {
            finished_at_unix
                .saturating_sub(value.started_at_unix)
                .saturating_mul(1000)
        }),
        reason: reason.map(str::to_string),
        error_code: error_code.map(str::to_string),
    }
}

fn append(journal_root: &Path, record: UpdateRecord) {
    let path = journal_root.join(HISTORY_FILE);
    rotate(&path);
    let Ok(line) = serde_json::to_string(&record) else {
        return;
    };
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    use std::io::Write;
    let _ = writeln!(file, "{line}");
}

/// Keep the file bounded without touching recent history.
fn rotate(path: &Path) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    if metadata.len() <= MAX_BYTES {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = text.lines().collect();
    let mut kept = lines[lines.len().saturating_sub(KEEP_LINES)..].join("\n");
    kept.push('\n');
    let _ = crate::fs::atomic::write_atomic(path, kept.as_bytes());
}

fn now_unix() -> u64 {
    time::OffsetDateTime::now_utc().unix_timestamp().max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip_and_honor_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        for version in ["2.0.0", "2.1.0", "2.2.0"] {
            append(
                dir.path(),
                build(
                    Some(version),
                    Some("1.0.0"),
                    None,
                    UpdateResult::Completed,
                    None,
                    None,
                ),
            );
        }
        let all = read(dir.path(), 10);
        assert_eq!(all.len(), 3);
        assert_eq!(all[2].version, "2.2.0");
        let last = read(dir.path(), 1);
        assert_eq!(last[0].version, "2.2.0");
        assert!(read(dir.path(), 0).is_empty());
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(HISTORY_FILE),
            "not json\n{\"v\":1,\"version\":\"2.0.0\",\"finished_at_unix\":1,\"result\":\"completed\"}\n",
        )
        .unwrap();
        let records = read(dir.path(), 10);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].version, "2.0.0");
    }

    #[test]
    fn started_state_is_consumed_once() {
        let dir = tempfile::tempdir().unwrap();
        mark_started(dir.path(), "2.0.0", "1.9.0");
        let started = take_started(dir.path()).expect("started");
        assert_eq!(started.version, "2.0.0");
        assert!(take_started(dir.path()).is_none());
    }
}
