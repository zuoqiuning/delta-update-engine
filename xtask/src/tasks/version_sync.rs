use std::path::Path;

/// Verify that the workspace version and the npm package version match, and
/// that every workspace crate inherits the version.
pub fn run() -> Result<(), String> {
    let workspace = read_workspace_version(Path::new("Cargo.toml"))?;
    let npm = read_json_version(Path::new("packages/plugin-api/package.json"))?;

    if workspace != npm {
        return Err(format!(
            "version mismatch: workspace {workspace}, npm {npm}"
        ));
    }

    for entry in std::fs::read_dir("crates").map_err(|err| err.to_string())? {
        let entry = entry.map_err(|err| err.to_string())?;
        let manifest = entry.path().join("Cargo.toml");
        if !manifest.exists() {
            continue;
        }
        let text = std::fs::read_to_string(&manifest).map_err(|err| err.to_string())?;
        if !text.contains("version.workspace = true") {
            return Err(format!(
                "{} does not inherit workspace version",
                manifest.display()
            ));
        }
        check_path_dependencies(&manifest, &workspace)?;
    }
    check_path_dependencies(Path::new("e2e/Cargo.toml"), &workspace)?;

    println!("version-sync: {workspace}");
    Ok(())
}

/// Any regular path dependency between workspace crates must also pin the
/// version, otherwise `cargo publish` cannot resolve it from the registry and
/// `cargo-deny` reports it as a wildcard dependency.
///
/// Dev-dependencies are exempt: Cargo removes them from the packaged manifest,
/// so a path-only dev-dependency on the unpublished testkit is both allowed and
/// required for `cargo package` to succeed.
fn check_path_dependencies(manifest: &Path, expected: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(manifest).map_err(|err| err.to_string())?;
    let mut dev_dependencies = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            dev_dependencies = line == "[dev-dependencies]";
            continue;
        }
        if dev_dependencies {
            continue;
        }
        if line.starts_with("delta-update-")
            && line.contains("path =")
            && !line.contains(&format!("version = \"{expected}\""))
        {
            return Err(format!(
                "{}: path dependency must pin version = \"{expected}\": {line}",
                manifest.display()
            ));
        }
    }
    Ok(())
}

fn read_workspace_version(path: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(path).map_err(|err| err.to_string())?;
    let mut in_section = false;
    for line in text.lines() {
        let line = line.trim();
        if line == "[workspace.package]" {
            in_section = true;
            continue;
        }
        if in_section && line.starts_with('[') {
            break;
        }
        if in_section {
            if let Some(value) = line.strip_prefix("version = ") {
                return Ok(value.trim_matches('"').to_string());
            }
        }
    }
    Err("workspace.package.version not found".into())
}

fn read_json_version(path: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(path).map_err(|err| err.to_string())?;
    for line in text.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("\"version\":") {
            return Ok(value
                .trim()
                .trim_end_matches(',')
                .trim_matches('"')
                .to_string());
        }
    }
    Err(format!("version not found in {}", path.display()))
}
