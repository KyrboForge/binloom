use std::{
    env,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

pub(crate) const MANIFEST: &str = "binloom.toml";
pub(crate) const LOCKFILE: &str = "binloom.lock";
pub(crate) const TOOLS_DIR: &str = ".tools";

pub(crate) fn project_root() -> Result<PathBuf> {
    let current = env::current_dir().context("failed to determine current directory")?;

    find_project_root(&current).with_context(|| {
        format!(
            "{MANIFEST} not found in {} or any parent directory",
            current.display()
        )
    })
}

fn find_project_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|directory| directory.join(MANIFEST).is_file())
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn finds_project_root_from_nested_directory() {
        let directory = tempfile::tempdir().unwrap();
        let nested = directory.path().join("src/nested");

        fs::create_dir_all(&nested).unwrap();
        fs::write(directory.path().join(MANIFEST), "").unwrap();

        assert_eq!(
            find_project_root(&nested),
            Some(directory.path().to_path_buf())
        );
    }

    #[test]
    fn returns_none_outside_project() {
        let directory = tempfile::tempdir().unwrap();

        assert_eq!(find_project_root(directory.path()), None);
    }
}
