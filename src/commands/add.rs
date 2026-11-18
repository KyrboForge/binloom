use std::{fmt::Write, fs, io, path::Path};

use anyhow::{Context, Result, ensure};

use super::{install, update};
use crate::{
    common::{
        LOCKFILE, MANIFEST, project_root, validate_tool_name, validate_version, write_atomically,
    },
    domain::{
        lockfile::Lockfile,
        manifest::Manifest,
        sources::{Source, bare_version},
    },
};

pub(crate) fn add(name: &str, source: &str, version: &str, asset: Option<&str>) -> Result<()> {
    let root = project_root()?;

    add_with(
        &root,
        name,
        source,
        version,
        asset,
        |manifest| update::lock_added_tool(&root, manifest, name),
        install::install,
    )
}

fn add_with(
    root: &Path,
    name: &str,
    source: &str,
    version: &str,
    asset: Option<&str>,
    resolve: impl FnOnce(&Manifest) -> Result<Lockfile>,
    install: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let manifest_path = root.join(MANIFEST);
    let lock_path = root.join(LOCKFILE);

    let manifest = fs::read_to_string(&manifest_path).context("failed to read binloom.toml")?;
    let lockfile = match fs::read(&lock_path) {
        Ok(content) => Some(content),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("failed to read binloom.lock"),
    };

    let content = render_tool(&manifest_path, &manifest, name, source, version, asset)?;
    let updated = Manifest::parse(&content, &manifest_path)?;
    let updated_lock = resolve(&updated).with_context(|| format!("failed to resolve {name}"))?;
    updated_lock.write_with_manifest(root, &content)?;
    println!("Updated {}", lock_path.display());

    if let Err(error) = install() {
        if let Err(restore_error) =
            restore(&manifest_path, manifest.as_bytes(), &lock_path, lockfile)
        {
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
) -> Result<()> {
    write_atomically(manifest_path, manifest)?;

    match lockfile {
        Some(content) => write_atomically(lock_path, &content),
        None => match fs::remove_file(lock_path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        },
    }
}

fn render_tool(
    path: &Path,
    content: &str,
    name: &str,
    source: &str,
    version: &str,
    asset: Option<&str>,
) -> Result<String> {
    validate_tool_name(name)?;
    let version = bare_version(name, version);
    validate_version(version)?;
    let manifest = Manifest::parse(content, path)?;
    ensure!(
        !manifest.tools.contains_key(name),
        "tool {name} is already configured"
    );

    let source = Source::try_from(source.to_owned()).map_err(anyhow::Error::msg)?;

    let mut content = content.to_owned();

    if !content.ends_with('\n') {
        content.push('\n');
    }

    if !content.ends_with("\n\n") {
        content.push('\n');
    }

    writeln!(content, "[tools.{name}]")?;
    writeln!(
        content,
        "version = {}",
        toml::Value::String(version.to_owned())
    )?;
    writeln!(
        content,
        "source = {}",
        toml::Value::String(source.to_string())
    )?;
    if let Some(asset) = asset {
        writeln!(content, "asset = {}", toml::Value::String(asset.to_owned()))?;
    }
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::{lockfile::LockedTool, platform::Platform},
        download,
    };

    fn write_tool(
        path: &Path,
        name: &str,
        source: &str,
        version: &str,
        asset: Option<&str>,
    ) -> Result<()> {
        let content = fs::read_to_string(path)?;
        fs::write(
            path,
            render_tool(path, &content, name, source, version, asset)?,
        )?;
        Ok(())
    }

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
            |updated_manifest| {
                assert!(updated_manifest.tools.contains_key("example"));
                assert_eq!(fs::read(&manifest_path).unwrap(), manifest);
                assert_eq!(fs::read(&lock_path).unwrap(), lockfile);
                let mut updated = Lockfile::default();
                updated.tools.insert(
                    "example".to_owned(),
                    LockedTool {
                        version: "1.2.3".to_owned(),
                        source: "github:owner/example".to_owned(),
                        tag: Some("v1.2.3".to_owned()),
                        sha256: None,
                        artifacts: Default::default(),
                        ..LockedTool::default()
                    },
                );
                Ok(updated)
            },
            || {
                assert_ne!(fs::read(&manifest_path).unwrap(), manifest);
                assert_ne!(fs::read(&lock_path).unwrap(), lockfile);

                // Missing artifacts fail installation before any download.
                install::install_from(directory.path(), &download::client(), Platform::LinuxX86_64)
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
    fn stores_bare_version_for_release_tag_input() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("binloom.toml");

        fs::write(
            &path,
            "manifest-version = 1\n\n[binloom]\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        write_tool(&path, "yq", "github:mikefarah/yq", "v4.54.3", None).unwrap();
        write_tool(&path, "jq", "github:jqlang/jq", "jq-1.8.2", None).unwrap();
        let manifest = Manifest::try_from(path.as_path()).unwrap();

        assert_eq!(manifest.tools["yq"].version, "4.54.3");
        assert_eq!(manifest.tools["jq"].version, "1.8.2");
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
