use std::path::Path;

/// Generated deliverables that are not documentation and must not be scanned.
const GENERATED_MARKDOWN: &[&str] = &["项目完整源码.md"];

/// Check that relative Markdown links point at existing files.
///
/// Covers every Markdown file in the repository (docs, crate READMEs,
/// example README, root policies). External URLs and pure anchors are skipped.
pub fn run() -> Result<(), String> {
    let mut errors = Vec::new();
    visit(Path::new("."), &mut errors)?;

    if errors.is_empty() {
        println!("docs links: ok");
        Ok(())
    } else {
        for error in &errors {
            eprintln!("{error}");
        }
        Err(format!("{} broken documentation links", errors.len()))
    }
}

fn visit(dir: &Path, errors: &mut Vec<String>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|err| format!("read {}: {err}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|err| err.to_string())?;
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            if matches!(
                name.to_string_lossy().as_ref(),
                "target" | "node_modules" | ".git" | "dist"
            ) {
                continue;
            }
            visit(&path, errors)?;
        } else if path.extension().is_some_and(|ext| ext == "md") {
            if GENERATED_MARKDOWN
                .iter()
                .any(|generated| name.to_string_lossy() == *generated)
            {
                continue;
            }
            visit_file(&path, errors)?;
        }
    }
    Ok(())
}

fn visit_file(path: &Path, errors: &mut Vec<String>) -> Result<(), String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => return Ok(()),
    };
    let base = path.parent().unwrap_or_else(|| Path::new("."));

    for target in markdown_targets(&text) {
        if target.starts_with("http://")
            || target.starts_with("https://")
            || target.starts_with('#')
            || target.starts_with("mailto:")
            || target.starts_with("computer://")
        {
            continue;
        }
        let target = target.split('#').next().unwrap_or("");
        if target.is_empty() {
            continue;
        }
        let resolved = base.join(target);
        if !resolved.exists() {
            errors.push(format!("{}: missing link target {target}", path.display()));
        }
    }
    Ok(())
}

fn markdown_targets(text: &str) -> Vec<&str> {
    let mut targets = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("](") {
        rest = &rest[start + 2..];
        if let Some(end) = rest.find(')') {
            targets.push(rest[..end].trim());
            rest = &rest[end + 1..];
        } else {
            break;
        }
    }
    targets
}
