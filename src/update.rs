use std::{collections::BTreeMap, path::Path};

use crate::resolve::{resolve_binloom, resolve_tool};
use crate::{
    common::{LOCKFILE, MANIFEST, project_root},
    download,
    lockfile::Lockfile,
    manifest::{self, Manifest, Tool},
    sources::Source,
};
use anyhow::{Context, Result};

pub fn update(tool_name: Option<&str>) -> Result<()> {
    let root = project_root()?;

    update_tools(&root, tool_name, true)
}

pub(crate) fn lock() -> Result<()> {
    let root = project_root()?;

    update_tools(&root, None, false)
}

pub fn lock_added_tool(tool_name: &str) -> Result<()> {
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

        let locked = resolve_tool(name, tool, version, minimum_age, &client)?;

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

        let (locked, wrapper) = resolve_binloom(&source, version, minimum_age, &client)?;

        let version = locked.version.clone();

        lockfile.binloom = Some(locked);
        lockfile.wrapper = Some(wrapper);

        latest.then_some(version)
    } else {
        None
    };

    lockfile.write(lock_path.as_path())?;

    if latest {
        manifest::update_versions(
            manifest_path.as_path(),
            binloom_version.as_deref(),
            versions
                .iter()
                .map(|(name, version)| (name.as_str(), version.as_str())),
        )?;
    }

    println!("Updated {}", lock_path.display());

    Ok(())
}

pub fn update_binloom() -> Result<()> {
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
    lockfile.write(lock_path.as_path())?;

    manifest::update_versions(manifest_path.as_path(), Some(&version), [])?;

    println!("Updated Binloom in {}", lock_path.display());

    Ok(())
}

fn lock_target<'a>(existing: Option<&Lockfile>, tool_name: &'a str) -> Option<&'a str> {
    existing
        .is_some_and(|lockfile| lockfile.binloom.is_some() && lockfile.wrapper.is_some())
        .then_some(tool_name)
}

fn binloom_source() -> Source {
    Source::try_from("github:KyrboForge/binloom".to_owned())
        .expect("hardcoded Binloom source must be valid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lockfile::{ChecksumSource, LockedTool, LockedWrapper};

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
                tag: "v0.1.0".to_owned(),
                artifacts: BTreeMap::new(),
            }),
            ..Lockfile::default()
        };

        assert_eq!(lock_target(Some(&existing), "lefthook"), Some("lefthook"));
        assert_eq!(lock_target(None, "lefthook"), None);
        assert_eq!(lock_target(Some(&Lockfile::default()), "lefthook"), None);
    }
}
