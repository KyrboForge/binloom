use anyhow::{Context, Result};
use std::{fs, io::Write, path::Path};
use tempfile::NamedTempFile;

/// Replaces `path` with `content` via a synced temporary file in the same
/// directory, so readers never observe a partially written file.
pub(crate) fn write_atomically(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    let mut temporary = NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create temporary file in {}", parent.display()))?;

    temporary
        .write_all(content)
        .with_context(|| format!("failed to write temporary file for {}", path.display()))?;

    // `NamedTempFile` is owner-only; keep the mode of the file being replaced.
    if let Ok(metadata) = fs::metadata(path) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())
            .with_context(|| format!("failed to copy permissions of {}", path.display()))?;
    }

    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("failed to sync temporary file for {}", path.display()))?;

    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to replace {}", path.display()))?;

    #[cfg(unix)]
    {
        let directory = fs::File::open(parent)
            .with_context(|| format!("failed to open {}", parent.display()))?;

        directory
            .sync_all()
            .with_context(|| format!("failed to sync {}", parent.display()))?;
    }

    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::write_atomically;
    use std::{fs, os::unix::fs::PermissionsExt};

    #[test]
    fn keeps_permissions_of_the_replaced_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("binloom.toml");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        write_atomically(&path, b"new").unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }
}
