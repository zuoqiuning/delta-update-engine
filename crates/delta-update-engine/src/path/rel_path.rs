use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::EngineError;

/// A manifest-relative path that has passed structural validation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RelPath(String);

impl RelPath {
    pub(crate) fn from_validated(value: String) -> Self {
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn to_path_buf(&self) -> std::path::PathBuf {
        self.0.split('/').collect()
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<&str> for RelPath {
    type Error = EngineError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        validate(value).map(Self::from_validated)
    }
}

pub(crate) fn validate(value: &str) -> Result<String, EngineError> {
    if value.is_empty() {
        return Err(EngineError::PathInvalid("path is empty".into()));
    }
    if value.len() > 1024 {
        return Err(EngineError::PathInvalid("path is too long".into()));
    }
    if value.starts_with('/') || value.starts_with('\\') {
        return Err(EngineError::PathInvalid(
            "path must be relative and must not start with a separator".into(),
        ));
    }
    if value.contains('\\') {
        return Err(EngineError::PathInvalid(
            "manifest paths must use '/' separators".into(),
        ));
    }
    if value.ends_with('/') {
        return Err(EngineError::PathInvalid(
            "path must not end with a separator".into(),
        ));
    }

    let segments: Vec<&str> = value.split('/').collect();
    if segments.len() > 64 {
        return Err(EngineError::PathInvalid(
            "path has too many segments".into(),
        ));
    }
    for segment in segments {
        if segment.is_empty() {
            return Err(EngineError::PathInvalid("path has an empty segment".into()));
        }
        if segment == "." || segment == ".." {
            return Err(EngineError::PathEscape(format!(
                "path contains a traversal segment: {value}"
            )));
        }
        if segment
            .chars()
            .any(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        {
            return Err(EngineError::PathInvalid(
                "path contains characters that are invalid on Windows".into(),
            ));
        }
        if segment.ends_with(' ') || segment.ends_with('.') {
            return Err(EngineError::PathInvalid(
                "path segment must not end with a space or dot".into(),
            ));
        }
        if is_reserved_name(segment) {
            return Err(EngineError::PathInvalid(format!(
                "path uses a reserved Windows device name: {segment}"
            )));
        }
    }

    Ok(value.to_string())
}

fn is_reserved_name(segment: &str) -> bool {
    let stem = segment.split('.').next().unwrap_or(segment);
    let upper = stem.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}
