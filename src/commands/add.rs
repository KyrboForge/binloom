use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
};

use anyhow::{Context, Result, ensure};

use super::{install, update};
use crate::{
    common::{LOCKFILE, MANIFEST, project_root, validate_tool_name, validate_version},
    domain::{manifest::Manifest, sources::Source},
};

pub(crate) fn add(name: &str, source: &str, version: &str, asset: Option<&str>) -> Result<()> {
    let root = project_root()?;

    add_with(&root, name, source, version, asset, || {
        update::lock_added_tool(name).and_then(|()| install::install())
    })
}

fn add_with(
    root: &Path,
    name: &str,
    source: &str,
    version: &str,
    asset: Option<&str>,
    lock_and_install: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let manifest_path = root.join(MANIFEST);
    let lock_path = root.join(LOCKFILE);

    let manifest = fs::read(&manifest_path).context("failed to read binloom.toml")?;
    let lockfile = match fs::read(&lock_path) {
        Ok(content) => Some(content),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("failed to read binloom.lock"),
    };

    write_tool(manifest_path.as_path(), name, source, version, asset)?;

    let result = lock_and_install();

    if let Err(error) = result {
        if let Err(restore_error) = restore(&manifest_path, &manifest, &lock_path, lockfile) {
            return Err(error.context(format!(
                "failed to restore binloom.toml and binloom.lock: {restore_error:#}"
            )));
        }

        return Err(error.context(format!("failed to add {name}; binloom.toml was restored")));
    }

    Ok(())
}

fn restore(
    manifest_path: &Path,
    manifest: &[u8],
    lock_path: &Path,
    lockfile: Option<Vec<u8>>,
) -> io::Result<()> {
    fs::write(manifest_path, manifest)?;

    match lockfile {
        Some(content) => fs::write(lock_path, content),
        None => match fs::remove_file(lock_path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        },
    }
}

fn write_tool(
    path: &Path,
    name: &str,
    source: &str,
    version: &str,
    asset: Option<&str>,
) -> Result<()> {
    validate_tool_name(name)?;
    validate_version(version)?;

    let manifest = Manifest::try_from(path)?;
    ensure!(
        !manifest.tools.contains_key(name),
        "tool {name} is already configured"
    );

    let source = Source::try_from(source.to_owned()).map_err(anyhow::Error::msg)?;

    let content = fs::read_to_string(path)?;
    let mut file = OpenOptions::new().append(true).open(path)?;

    if !content.ends_with('\n') {
        writeln!(file)?;
    }

    if !content.ends_with("\n\n") {
        writeln!(file)?;
    }

    writeln!(file, "[tools.{name}]")?;
    writeln!(
        file,
        "version = {}",
        toml::Value::String(version.to_owned())
    )?;
    writeln!(file, "source = {}", toml::Value::String(source.to_string()))?;
    if let Some(asset) = asset {
        writeln!(file, "asset = {}", toml::Value::String(asset.to_owned()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::lockfile::{LockedTool, Lockfile},
        download,
    };

    #[test]
    fn restores_manifest_and_existing_lockfile_when_install_fails() {
        let directory = tempfile::tempdir().unwrap();
        let manifest_path = directory.path().join(MANIFEST);
        let lock_path = directory.path().join(LOCKFILE);
        let manifest =
            b"# project tools\r\nmanifest-version = 1\r\n\r\n[binloom]\r\nversion = \"0.1.0\"";
        let lockfile = b"# preserve this comment\r\nlock-version = 1\r\n";

        fs::write(&manifest_path, manifest).unwrap();
        fs::write(&lock_path, lockfile).unwrap();

        let error = add_with(
            directory.path(),
            "example",
            "github:owner/example",
            "1.2.3",
            None,
            || {
                // Resolve offline, then use the real lockfile writer and installer.
                let mut updated = Lockfile::default();
                updated.tools.insert(
                    "example".to_owned(),
                    LockedTool {
                        version: "1.2.3".to_owned(),
                        source: "github:owner/example".to_owned(),
                        tag: "v1.2.3".to_owned(),
                        artifacts: Default::default(),
                    },
                );
                updated.write(&lock_path)?;
                assert_ne!(fs::read(&manifest_path).unwrap(), manifest);
                assert_ne!(fs::read(&lock_path).unwrap(), lockfile);

                // Missing artifacts fail installation before any download.
                install::install_from(directory.path(), &download::client())
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("binloom.toml was restored"));
        assert!(
            format!("{error:#}").contains("tool example has no artifact for"),
            "{error:#}"
        );
        assert_eq!(fs::read(&manifest_path).unwrap(), manifest);
        assert_eq!(fs::read(&lock_path).unwrap(), lockfile);
    }

    #[test]
    fn appends_tool_without_rewriting_manifest() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("binloom.toml");

        fs::write(
            &path,
            "manifest-version = 1\n\n[binloom]\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        write_tool(
            &path,
            "lefthook",
            "github:evilmartians/lefthook",
            "2.1.10",
            None,
        )
        .unwrap();
        write_tool(
            &path,
            "example",
            "github:owner/example",
            "1.2.3",
            Some("example_{version}_{os}_{arch}.gz"),
        )
        .unwrap();
        let manifest = Manifest::try_from(path.as_path()).unwrap();

        assert_eq!(manifest.tools["lefthook"].version, "2.1.10");
        assert_eq!(
            manifest.tools["example"].asset.as_deref(),
            Some("example_{version}_{os}_{arch}.gz")
        );

        let before = fs::read_to_string(&path).unwrap();
        assert!(
            write_tool(
                &path,
                "lefthook",
                "github:evilmartians/lefthook",
                "2.1.10",
                None
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(path).unwrap(), before);
    }
}
