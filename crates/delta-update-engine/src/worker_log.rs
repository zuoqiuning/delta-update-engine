//! Append-only log next to the journal.
//!
//! The replacement worker runs with null stdio, so this file is the only place
//! its outcome survives. `prepare` also records per-file patch fallbacks here,
//! so a support bundle can answer "why was this update slow" without guessing.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const FILE: &str = "worker.log";

/// A permanently failing update would grow the file without bound. Keep the
/// newest lines once it crosses 1 MiB; the bundle export only reads the tail.
const MAX_BYTES: u64 = 1024 * 1024;
const KEEP_BYTES: u64 = 256 * 1024;

/// Append one line. Best effort: logging must never fail an update.
pub(crate) fn append(journal_root: &Path, message: &str) {
    let path = journal_root.join(FILE);
    trim(&path);
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    let _ = writeln!(file, "{}", oneline(message));
}

/// One log entry must stay on one line. `EngineError` messages can embed
/// newlines (network response bodies, OS error chains), which would otherwise
/// split a single event into several lines that look like separate entries.
pub(crate) fn oneline(value: &str) -> String {
    value.replace(['\n', '\r'], " ")
}

fn trim(path: &Path) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    if metadata.len() <= MAX_BYTES {
        return;
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return;
    };
    let start = metadata.len().saturating_sub(KEEP_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return;
    }
    let mut tail = Vec::new();
    if file.read_to_end(&mut tail).is_err() {
        return;
    }
    // Release the handle before replacing the file; Windows rejects the rename
    // while another handle is still open in some configurations.
    drop(file);
    // Drop the partial first line so the trimmed file stays readable.
    if let Some(position) = tail.iter().position(|byte| *byte == b'\n') {
        tail.drain(..=position);
    }
    // Relies on `std::fs::rename` replacing an existing file on Windows
    // (`MoveFileExW(MOVEFILE_REPLACE_EXISTING)`), which `write_atomic` uses
    // through `NamedTempFile::persist`. If std ever changes that, switch to an
    // explicit remove + rename; a missing worker.log is acceptable for logging.
    let _ = crate::fs::atomic::write_atomic(path, &tail);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_stay_on_one_line() {
        assert_eq!(
            oneline("denied\r\nsecond line\nthird"),
            "denied  second line third"
        );
        assert_eq!(oneline("plain"), "plain");
    }

    #[test]
    fn oversized_logs_are_trimmed_to_the_newest_lines() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(FILE);
        let line = "apply commit abc failed: file is locked\n";
        let mut content = String::new();
        while content.len() <= MAX_BYTES as usize + line.len() {
            content.push_str(line);
        }
        std::fs::write(&path, content).unwrap();

        trim(&path);
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(size <= KEEP_BYTES, "trimmed size: {size}");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("apply commit"));
        assert!(text.trim_end().ends_with("locked"));
    }
}
