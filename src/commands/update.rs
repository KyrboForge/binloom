use std::{collections::BTreeMap, path::Path};

use crate::{
    common::{LOCKFILE, MANIFEST, project_root, write_atomically},
    domain::{
        lockfile::{LockedPlatforms, Lockfile},
        manifest::{self, Manifest, Tool},
        resolve::{resolve_binloom, resolve_tool},
        sources::Source,
    },
    download,
};
use anyhow::{Context, Result, ensure};

pub(crate) fn update(tool_name: Option<&str>) -> Result<()> {
    let root = project_root()?;

    update_tools(&root, tool_name, true)
}

pub(crate) fn lock() -> Result<()> {
    let root = project_root()?;

    update_tools(&root, None, false)
}

pub(crate) fn lock_added_tool(tool_name: &str) -> Result<()> {
    let root = project_root()?;
    let lock_path = root.join(LOCKFILE);

    let existing = if lock_path
        .try_exists()
        .context("failed to check binloom.lock")?
    {
        Some(Lockfile::try_from(lock_path.as_path())?)
    } else {
        None
    };

    let target = lock_target(existing.as_ref(), tool_name);

    update_tools(&root, target, false)
}

fn update_tools(root: &Path, tool_name: Option<&str>, latest: bool) -> Result<()> {
    let manifest_path = root.join(MANIFEST);
    let lock_path = root.join(LOCKFILE);
    let manifest = Manifest::try_from(manifest_path.as_path())?;

    let minimum_age = manifest.update.minimum_release_age_minutes;

    let (selected, mut lockfile): (Vec<(&str, &Tool)>, Lockfile) = match tool_name {
        Some(name) => {
            let tool = manifest
                .tools
                .get(name)
                .with_context(|| format!("tool {name} is not configured"))?;

            let lockfile = Lockfile::try_from(lock_path.as_path())
                .context("failed to load binloom.lock; run `binloom update` first")?;
            ensure!(
                manifest.project_targets()
                    == lockfile
                        .platforms
                        .as_ref()
                        .map(|platforms| platforms.targets.as_slice()),
                "[platforms] targets changed; run `binloom update` to regenerate binloom.lock"
            );

            (vec![(name, tool)], lockfile)
        }
        None => {
            let selected = manifest
                .tools
                .iter()
                .map(|(name, tool)| (name.as_str(), tool))
                .collect();

            (selected, Lockfile::default())
        }
    };

    let client = download::client();
    let mut versions = BTreeMap::new();

    for (name, tool) in selected {
        let version = if latest {
            None
        } else {
            Some(tool.version.as_str())
        };

        let targets = manifest.tool_targets(tool);
        let mut locked = resolve_tool(name, tool, targets, version, minimum_age, &client)?;
        // Explicit projects snapshot the resolver inputs so drift is detectable.
        if manifest.platforms.is_some() {
            locked.targets = Some(targets.to_vec());
            locked.asset_pattern.clone_from(&tool.asset);
        }

        versions.insert(name.to_owned(), locked.version.clone());
        lockfile.tools.insert(name.to_owned(), locked);
    }

    let binloom_version = if tool_name.is_none() {
        let source = binloom_source();

        let version = if latest {
            None
        } else {
            Some(manifest.binloom.version.as_str())
        };

        let minimum_age = binloom_minimum_age(version, minimum_age);
        let (locked, wrapper) = resolve_binloom(&source, version, minimum_age, &client)?;

        let version = locked.version.clone();

        lockfile.binloom = Some(locked);
        lockfile.wrapper = Some(wrapper);

        latest.then_some(version)
    } else {
        None
    };

    lockfile.platforms = manifest.project_targets().map(|targets| LockedPlatforms {
        targets: targets.to_vec(),
    });

    // Every resolution and manifest edit succeeds before any file is replaced.
    let updated_manifest = latest
        .then(|| {
            manifest::render_versions(
                manifest_path.as_path(),
                binloom_version.as_deref(),
                versions
                    .iter()
                    .map(|(name, version)| (name.as_str(), version.as_str())),
            )
        })
        .transpose()?;

    lockfile.write(lock_path.as_path())?;

    if let Some(content) = updated_manifest {
        write_atomically(manifest_path.as_path(), content.as_bytes())?;
    }

    println!("Updated {}", lock_path.display());

    Ok(())
}

pub(crate) fn update_binloom() -> Result<()> {
    let root = project_root()?;
    let manifest_path = root.join(MANIFEST);
    let lock_path = root.join(LOCKFILE);
    let manifest = Manifest::try_from(manifest_path.as_path())?;

    let source = binloom_source();

    let client = download::client();

    let (locked, wrapper) = resolve_binloom(
        &source,
        None,
        manifest.update.minimum_release_age_minutes,
        &client,
    )?;

    let mut lockfile = if lock_path
        .try_exists()
        .context("failed to check binloom.lock")?
    {
        Lockfile::try_from(lock_path.as_path())?
    } else {
        Lockfile::default()
    };

    let version = locked.version.clone();
    lockfile.binloom = Some(locked);
    lockfile.wrapper = Some(wrapper);
    let updated_manifest = manifest::render_versions(manifest_path.as_path(), Some(&version), [])?;
    lockfile.write(lock_path.as_path())?;

    write_atomically(manifest_path.as_path(), updated_manifest.as_bytes())?;

    println!("Updated Binloom in {}", lock_path.display());

    Ok(())
}

fn lock_target<'a>(existing: Option<&Lockfile>, tool_name: &'a str) -> Option<&'a str> {
    existing
        .is_some_and(|lockfile| lockfile.binloom.is_some() && lockfile.wrapper.is_some())
        .then_some(tool_name)
}

fn binloom_minimum_age(version: Option<&str>, configured: u64) -> u64 {
    // Locking the exact version already running needs no cooling-off period.
    if version == Some(env!("CARGO_PKG_VERSION")) {
        0
    } else {
        configured
    }
}

fn binloom_source() -> Source {
    Source::try_from("github:KyrboForge/binloom".to_owned())
        .expect("hardcoded Binloom source must be valid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::lockfile::{ChecksumSource, LockedTool, LockedWrapper};

    #[test]
    fn exempts_only_the_running_binloom_pin_from_minimum_age() {
        let minimum_age = 24 * 60;

        assert_eq!(
            binloom_minimum_age(Some(env!("CARGO_PKG_VERSION")), minimum_age),
            0
        );
        assert_eq!(binloom_minimum_age(None, minimum_age), minimum_age);
        assert_eq!(
            binloom_minimum_age(Some("999.0.0"), minimum_age),
            minimum_age
        );
    }

    #[test]
    fn selects_single_tool_only_for_complete_lockfile() {
        let existing = Lockfile {
            wrapper: Some(LockedWrapper {
                version: "0.1.1".to_owned(),
                url: "https://example.com/binloomw".to_owned(),
                sha256: "a".repeat(64),
                checksum_source: ChecksumSource::Digest,
            }),
            binloom: Some(LockedTool {
                version: "0.1.0".to_owned(),
                source: "github:KyrboForge/binloom".to_owned(),
                tag: Some("v0.1.0".to_owned()),
                ..LockedTool::default()
            }),
            ..Lockfile::default()
        };

        assert_eq!(lock_target(Some(&existing), "lefthook"), Some("lefthook"));
        assert_eq!(lock_target(None, "lefthook"), None);
        assert_eq!(lock_target(Some(&Lockfile::default()), "lefthook"), None);
    }

    #[test]
    fn single_tool_update_rejects_changed_project_targets_before_writing() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = "manifest-version = 1\n[binloom]\nversion = \"0.1.0\"\n\
            [platforms]\ntargets = [\"linux-x86_64\"]\n\
            [tools.tool]\nversion = \"1.0.0\"\nsource = \"github:owner/tool\"\n";
        let lock = "lock-version = 1\n";
        std::fs::write(directory.path().join(MANIFEST), manifest).unwrap();
        std::fs::write(directory.path().join(LOCKFILE), lock).unwrap();

        let error = update_tools(directory.path(), Some("tool"), false).unwrap_err();

        assert!(error.to_string().contains("[platforms] targets changed"));
        assert_eq!(
            std::fs::read_to_string(directory.path().join(MANIFEST)).unwrap(),
            manifest
        );
        assert_eq!(
            std::fs::read_to_string(directory.path().join(LOCKFILE)).unwrap(),
            lock
        );
    }
}
