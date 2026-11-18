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
