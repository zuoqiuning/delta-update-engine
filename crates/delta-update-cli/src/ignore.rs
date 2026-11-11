use std::path::Path;

pub const DELTAIGNORE_FILE: &str = ".deltaignore";

const DEFAULT_PATTERNS: &[&str] = &[
    "*.pdb",
    "*.dSYM/",
    "*.map",
    "*.tmp",
    "*.log",
    ".DS_Store",
    "Thumbs.db",
    ".deltaignore",
];

#[derive(Debug, Clone)]
struct Rule {
    negated: bool,
    dir_only: bool,
    anchored: bool,
    segments: Vec<String>,
}

impl Rule {
    fn parse(line: &str) -> Option<Self> {
        let (negated, pattern) = match line.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, line),
        };
        let dir_only = pattern.ends_with('/');
        let pattern = pattern.trim_end_matches('/');
        let anchored = pattern.starts_with('/');
        let pattern = pattern.trim_start_matches('/');
        if pattern.is_empty() {
            return None;
        }
        Some(Self {
            negated,
            dir_only,
            anchored,
            segments: pattern.split('/').map(str::to_string).collect(),
        })
    }

    fn matches(&self, path: &str) -> bool {
        let segments: Vec<&str> = path.split('/').collect();
        if self.dir_only {
            let dirs = &segments[..segments.len().saturating_sub(1)];
            return self.matches_from(dirs, true);
        }
        self.matches_from(&segments, false)
    }

    fn matches_from(&self, segments: &[&str], allow_prefix: bool) -> bool {
        if self.segments.len() == 1 && !self.anchored {
            return segments
                .iter()
                .any(|segment| wildcard_eq(&self.segments[0], segment));
        }
        if self.segments.len() > segments.len() {
            return false;
        }
        if !self
            .segments
            .iter()
            .zip(segments)
            .all(|(pattern, segment)| wildcard_eq(pattern, segment))
        {
            return false;
        }
        allow_prefix || self.segments.len() == segments.len()
    }
}

/// `.deltaignore`-compatible rule set: `*` within a segment, trailing `/` for
/// directories, leading `/` to anchor at the release root, and `!` to
/// re-include. Later rules win.
#[derive(Debug, Clone)]
pub struct DeltaIgnore {
    rules: Vec<Rule>,
}

impl Default for DeltaIgnore {
    fn default() -> Self {
        Self::parse("")
    }
}

impl DeltaIgnore {
    pub fn parse(text: &str) -> Self {
        let mut rules: Vec<Rule> = DEFAULT_PATTERNS
            .iter()
            .filter_map(|pattern| Rule::parse(pattern))
            .collect();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rule) = Rule::parse(line) {
                rules.push(rule);
            }
        }
        Self { rules }
    }

    /// Load `<root>/.deltaignore`; a missing file means defaults only.
    pub fn from_root(root: &Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(root.join(DELTAIGNORE_FILE)) {
            Ok(text) => Ok(Self::parse(&text)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err),
        }
    }

    pub fn is_ignored(&self, path: &str) -> bool {
        let mut ignored = false;
        for rule in &self.rules {
            if rule.matches(path) {
                ignored = !rule.negated;
            }
        }
        ignored
    }
}

fn wildcard_eq(pattern: &str, text: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == text;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut rest = text;
    for (index, part) in parts.iter().enumerate() {
        if index == 0 {
            let Some(tail) = rest.strip_prefix(part) else {
                return false;
            };
            rest = tail;
        } else if index == parts.len() - 1 {
            return rest.ends_with(part);
        } else if !part.is_empty() {
            let Some(position) = rest.find(part) else {
                return false;
            };
            rest = &rest[position + part.len()..];
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_exclude_debug_symbols_at_any_depth() {
        let ignore = DeltaIgnore::default();
        assert!(ignore.is_ignored("App.pdb"));
        assert!(ignore.is_ignored("bin/App.pdb"));
        assert!(ignore.is_ignored("logs/app.log"));
        assert!(ignore.is_ignored("bundle.dSYM/Contents/Info.plist"));
        assert!(!ignore.is_ignored("App.exe"));
    }

    #[test]
    fn wildcard_does_not_cross_segments() {
        let ignore = DeltaIgnore::parse("build/*.bin");
        assert!(ignore.is_ignored("build/App.bin"));
        assert!(!ignore.is_ignored("build/nested/App.bin"));
    }

    #[test]
    fn anchored_directory_and_negation_follow_last_match() {
        let ignore = DeltaIgnore::parse("/tmp/\n*.log\n!keep.log");
        assert!(ignore.is_ignored("tmp/cache.bin"));
        assert!(!ignore.is_ignored("nested/tmp/cache.bin"));
        assert!(!ignore.is_ignored("a/keep.log"));
        assert!(ignore.is_ignored("a/drop.log"));
    }
}
