use std::io;
use std::path::Path;

/// Write a file through a temporary sibling and an atomic rename.
pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    std::io::Write::write_all(&mut temp, data)?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .map_err(|err| io::Error::other(err.error))?;
    Ok(())
}

/// Best-effort directory sync. Windows does not support opening directories
/// as files, so this is a no-op there.
pub fn sync_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let dir = std::fs::File::open(path)?;
        dir.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}
