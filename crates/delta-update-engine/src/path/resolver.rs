use std::path::{Path, PathBuf};

use crate::error::EngineError;

/// Resolve the install root.
///
/// The running executable is the authoritative source. A configured override
/// is accepted only when it is absolute, exists, and contains the executable.
pub fn resolve_install_root(
    configured: Option<&Path>,
    current_exe: &Path,
) -> Result<PathBuf, EngineError> {
    let exe = dunce::canonicalize(current_exe).map_err(|err| {
        EngineError::ConfigInvalid(format!("cannot resolve current executable: {err}"))
    })?;
    let exe_parent = exe.parent().ok_or_else(|| {
        EngineError::ConfigInvalid("current executable has no parent directory".into())
    })?;

    if let Some(root) = configured {
        if !root.is_absolute() {
            return Err(EngineError::ConfigInvalid(
                "install_dir must be an absolute path".into(),
            ));
        }
        let root = dunce::canonicalize(root).map_err(|err| {
            EngineError::ConfigInvalid(format!("install_dir does not exist: {err}"))
        })?;
        if !exe.starts_with(&root) {
            return Err(EngineError::ConfigInvalid(
                "install_dir does not contain the running executable".into(),
            ));
        }
        return Ok(root);
    }

    Ok(exe_parent.to_path_buf())
}

/// Installation form inferred from where the executable lives, for diagnostics.
///
/// Only used in logs: a wrong answer changes no behavior, while knowing whether
/// a failed update ran from `%ProgramFiles%` (per-machine, needs elevation) or
/// `%LocalAppData%\Programs` (per-user) is what makes a support bundle readable.
pub fn install_form(exe: &Path) -> &'static str {
    let bases: Vec<String> = ["ProgramFiles", "ProgramFiles(x86)"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .collect();
    let local_programs = std::env::var("LOCALAPPDATA")
        .ok()
        .map(|base| format!("{base}\\Programs"));
    classify_install_form(
        exe.parent().unwrap_or(exe),
        &bases,
        local_programs.as_deref(),
    )
}

fn classify_install_form(
    directory: &Path,
    program_files: &[String],
    local_programs: Option<&str>,
) -> &'static str {
    let directory = normalize(directory);
    let under = |base: &str| {
        let base = normalize(Path::new(base));
        directory == base || directory.starts_with(&format!("{base}\\"))
    };
    if program_files.iter().any(|base| under(base)) {
        "per-machine"
    } else if local_programs.is_some_and(under) {
        "per-user"
    } else {
        "portable"
    }
}

fn normalize(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase()
        .trim_end_matches('\\')
        .to_string()
}

/// Canonical form of `path` even when it does not exist yet: resolve the
/// nearest existing ancestor and rejoin the missing tail.
///
/// Two spellings of the same directory (`C:\Users\RUNNER~1\…` versus
/// `C:\Users\runneradmin\…`, a junction, or a `\\?\` prefix) must compare equal
/// before any containment or equality check. `dunce::canonicalize` alone fails
/// such a check open whenever one side is spelled differently.
///
/// Residual boundary: a tail that does not exist yet keeps its original
/// spelling, so a short name *below* the last existing directory is not
/// expanded, and a `..` component there is preserved rather than resolved.
/// Callers that compare this result must reject `..` themselves
/// ([`crate::apply::applier`] does, and both sides of every comparison here are
/// roots the caller already validated); a leftover `..` makes the comparison
/// fail closed instead of matching a different directory.
pub fn long_path(path: &Path) -> PathBuf {
    if let Ok(resolved) = dunce::canonicalize(path) {
        return resolved;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => {
            long_path(parent).join(name)
        }
        _ => path.to_path_buf(),
    }
}

/// Whether `path` contains a `..` component.
///
/// [`long_path`] keeps such components as written, so a comparison of two
/// spellings would fail with a misleading "does not match". Callers that
/// compare roots refuse the unresolved form instead of guessing which side is
/// meant.
pub fn has_parent_component(path: &Path) -> bool {
    path.components()
        .any(|part| part == std::path::Component::ParentDir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_parent_component_sees_unresolved_segments() {
        assert!(has_parent_component(Path::new(r"C:\app\sub\..\bin")));
        assert!(!has_parent_component(Path::new(r"C:\app\bin")));
    }

    #[test]
    fn install_form_follows_the_containing_base() {
        let program_files = vec![r"C:\Program Files".to_string()];
        assert_eq!(
            classify_install_form(Path::new(r"c:\program files\MyApp"), &program_files, None),
            "per-machine"
        );
        assert_eq!(
            classify_install_form(
                Path::new(r"C:\Users\u\AppData\Local\Programs\MyApp"),
                &program_files,
                Some(r"C:\Users\u\AppData\Local\Programs")
            ),
            "per-user"
        );
        // A sibling that merely shares the prefix is not inside Program Files.
        assert_eq!(
            classify_install_form(
                Path::new(r"C:\Program Files Extra\MyApp"),
                &program_files,
                None
            ),
            "portable"
        );
        assert_eq!(
            classify_install_form(Path::new(r"D:\Tools\MyApp"), &program_files, None),
            "portable"
        );
    }

    #[test]
    fn long_path_strips_a_verbatim_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let verbatim = PathBuf::from(format!("\\\\?\\{}", dir.path().display()));
        let resolved = long_path(&verbatim);
        assert_eq!(resolved, dunce::canonicalize(dir.path()).unwrap());
        assert!(!resolved.to_string_lossy().starts_with(r"\\?\"));
    }

    #[test]
    fn long_path_resolves_a_tail_that_does_not_exist_yet() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("staging");
        assert_eq!(
            long_path(&missing),
            dunce::canonicalize(dir.path()).unwrap().join("staging")
        );
    }
}
