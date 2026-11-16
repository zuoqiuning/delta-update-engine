//! Append-only log next to the journal.
//!
//! The replacement worker runs with null stdio, so this file is the only place
//! its outcome survives. `prepare` also records per-file patch fallbacks here,
//! so a support bundle can answer "why was this update slow" without guessing.

use std::collections::BTreeMap;
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

/// Append one structured event: `event=<name> ts=<unix> key=value ...`.
///
/// Values are quoted only when they would otherwise split the line, so the
/// common case stays readable (`event=apply_ok commit=abc ms=12 retries=0`)
/// while paths with spaces still survive a round trip through [`parse`]. The
/// timestamp is what lets a bundle be lined up with the Windows event log or an
/// antivirus quarantine entry.
pub(crate) fn event(journal_root: &Path, name: &str, fields: &[(&str, &str)]) {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let mut line = format!("event={name} ts={seconds}");
    for (key, value) in fields {
        line.push(' ');
        line.push_str(key);
        line.push('=');
        line.push_str(&token(value));
    }
    append(journal_root, &line);
}

/// A parsed structured line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub name: String,
    fields: BTreeMap<String, String>,
}

impl Event {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(String::as_str)
    }
}

/// Parse a structured line written by the worker.
///
/// Returns `None` for prose lines, including the pre-`event=` format, so a log
/// that spans versions keeps working for whoever only knows one of the two.
pub fn parse(line: &str) -> Option<Event> {
    let mut fields = parse_fields(line)?;
    let name = fields.remove("event")?;
    Some(Event { name, fields })
}

fn parse_fields(line: &str) -> Option<BTreeMap<String, String>> {
    let mut fields = BTreeMap::new();
    let mut rest = line.trim_start();
    while !rest.is_empty() {
        let (key, after) = rest.split_once('=')?;
        if key.is_empty() || key.contains(char::is_whitespace) {
            return None;
        }
        let (value, remaining) = read_value(after)?;
        fields.insert(key.to_string(), value);
        rest = remaining.trim_start();
    }
    Some(fields)
}

fn read_value(input: &str) -> Option<(String, &str)> {
    let Some(stripped) = input.strip_prefix('"') else {
        let end = input.find(char::is_whitespace).unwrap_or(input.len());
        return Some((input[..end].to_string(), &input[end..]));
    };
    let mut value = String::new();
    let mut chars = stripped.char_indices();
    while let Some((index, ch)) = chars.next() {
        match ch {
            '\\' => value.push(chars.next()?.1),
            '"' => return Some((value, &stripped[index + 1..])),
            _ => value.push(ch),
        }
    }
    None
}

fn token(value: &str) -> String {
    let needs_quotes = value.is_empty()
        || value
            .chars()
            .any(|ch| ch.is_whitespace() || ch == '"' || ch == '\\');
    if !needs_quotes {
        return value.to_string();
    }
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for ch in value.chars() {
        if ch == '"' || ch == '\\' {
            quoted.push('\\');
        }
        quoted.push(ch);
    }
    quoted.push('"');
    quoted
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
    fn events_round_trip_through_parse() {
        let temp = tempfile::tempdir().unwrap();
        event(
            temp.path(),
            "patch_fallback",
            &[
                ("path", "dir with space/App.exe"),
                ("code", "E_PATCH_FORMAT_MISMATCH"),
            ],
        );
        event(
            temp.path(),
            "apply_ok",
            &[("commit", "abc"), ("retries", "0")],
        );
        let text = std::fs::read_to_string(temp.path().join(FILE)).unwrap();
        let mut lines = text.lines();
        let fallback = parse(lines.next().unwrap()).unwrap();
        assert_eq!(fallback.name, "patch_fallback");
        assert_eq!(fallback.get("path"), Some("dir with space/App.exe"));
        assert_eq!(fallback.get("code"), Some("E_PATCH_FORMAT_MISMATCH"));
        let ok = parse(lines.next().unwrap()).unwrap();
        assert_eq!(ok.name, "apply_ok");
        assert_eq!(ok.get("commit"), Some("abc"));
        let ts: u64 = ok
            .get("ts")
            .expect("structured events carry a timestamp")
            .parse()
            .expect("ts is a unix second count");
        assert!(ts > 1_700_000_000, "ts looks like a real clock: {ts}");
    }

    #[test]
    fn quotes_and_backslashes_survive_encoding() {
        let temp = tempfile::tempdir().unwrap();
        let value = r#".old/C:\temp\a"b/app.exe"#;
        event(temp.path(), "apply_failed", &[("msg", value)]);
        let text = std::fs::read_to_string(temp.path().join(FILE)).unwrap();
        let parsed = parse(text.trim_end()).unwrap();
        assert_eq!(parsed.get("msg"), Some(value));
    }

    #[test]
    fn prose_lines_are_not_events() {
        // Lines from the pre-`event=` format must not be mistaken for events.
        assert!(parse("apply commit 1.0 ok in 12 ms (retries 0)").is_none());
        assert!(parse("patch fallback: App.exe: E_PATCH_FORMAT_MISMATCH").is_none());
        assert!(parse("key=value").is_none());
        assert!(parse("").is_none());
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
