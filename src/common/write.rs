use anyhow::{Context, Result};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use tempfile::NamedTempFile;

use super::{LOCKFILE, MANIFEST};

/// Replaces `path` with `content` via a synced temporary file in the same
/// directory, so readers never observe a partially written file.
pub(crate) fn write_atomically(path: &Path, content: &[u8]) -> Result<()> {
    let path = &resolve_link(path);
    replace(stage(path, content)?, path)?;
    sync_parent(path)
}

/// A symlinked file is updated through the link, so the rename never detaches it.
fn resolve_link(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

pub(crate) fn write_new_atomically(path: &Path, content: &[u8]) -> Result<()> {
    stage(path, content)?
        .persist_noclobber(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to create {}", path.display()))?;
    sync_parent(path)
}

pub(crate) fn write_manifest_and_lock(root: &Path, manifest: &[u8], lock: &[u8]) -> Result<()> {
    let manifest_path = resolve_link(&root.join(MANIFEST));
    let lock_path = resolve_link(&root.join(LOCKFILE));
    let previous = fs::read(&manifest_path).context("failed to read binloom.toml")?;
    if previous == manifest {
        return write_atomically(&lock_path, lock);
    }

    // Prepare both replacements and the rollback before publishing either file.
    let restore = stage(&manifest_path, &previous)?;
    let manifest = stage(&manifest_path, manifest)?;
    let lock = stage(&lock_path, lock)?;
    replace(manifest, &manifest_path)?;
    if let Err(error) = replace(lock, &lock_path) {
        replace(restore, &manifest_path)
            .and_then(|()| sync_parent(&manifest_path))
            .map_err(|restore_error| {
                restore_error.context(format!("{error:#}; failed to restore binloom.toml"))
            })?;
        return Err(error);
    }
    sync_parent(&manifest_path)?;
    // A symlink may place the lock in another directory; its rename needs its own sync.
    if parent(&lock_path) != parent(&manifest_path) {
        sync_parent(&lock_path)?;
    }
    Ok(())
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn stage(path: &Path, content: &[u8]) -> Result<NamedTempFile> {
    let mut temporary = NamedTempFile::new_in(parent(path))
        .with_context(|| format!("failed to create temporary file for {}", path.display()))?;

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

    Ok(temporary)
}

fn replace(temporary: NamedTempFile, path: &Path) -> Result<()> {
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to replace {}", path.display()))?;
    Ok(())
}

fn sync_parent(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let parent = parent(path);
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
    use super::{LOCKFILE, MANIFEST, write_atomically, write_manifest_and_lock};
    use std::{fs, os::unix::fs::PermissionsExt};

    #[test]
    fn restores_manifest_when_lock_cannot_be_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = directory.path().join(MANIFEST);
        let lock = directory.path().join(LOCKFILE);
        fs::write(&manifest, b"original manifest").unwrap();
        fs::create_dir(&lock).unwrap();

        let error =
            write_manifest_and_lock(directory.path(), b"new manifest", b"new lock").unwrap_err();

        assert!(error.to_string().contains("failed to replace"));
        assert_eq!(fs::read(manifest).unwrap(), b"original manifest");
        assert!(lock.is_dir());
    }

    #[test]
    fn writes_through_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.toml");
        let link = directory.path().join(MANIFEST);
        fs::write(&target, b"old").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        write_atomically(&link, b"new").unwrap();

        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(fs::read(target).unwrap(), b"new");
    }

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
