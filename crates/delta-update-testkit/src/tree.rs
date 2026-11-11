use std::io;
use std::path::Path;

/// Write a small directory tree. Values are written verbatim as file bytes.
pub fn write_tree(root: &Path, files: &[(&str, &[u8])]) -> io::Result<()> {
    for (path, bytes) in files {
        let full = root.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(full, bytes)?;
    }
    Ok(())
}

/// Assert that every file in the tree matches the expected bytes.
pub fn assert_tree(root: &Path, files: &[(&str, &[u8])]) -> io::Result<()> {
    for (path, expected) in files {
        let actual = std::fs::read(root.join(path))?;
        assert_eq!(&actual, expected, "{path} content mismatch");
    }
    Ok(())
}
