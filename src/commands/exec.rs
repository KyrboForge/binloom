use super::install::{InstallOutput, ensure_tool_installed, ensure_unreserved_tool_name};
use crate::{
    common::{LOCKFILE, MANIFEST, TOOLS_DIR, project_root, validate_tool_name},
    domain::{lockfile::Lockfile, manifest::Manifest, platform::Platform},
    download,
};
use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

pub(crate) fn exec(arguments: &[OsString]) -> Result<()> {
    let (program, arguments) = arguments
        .split_first()
        .context("missing command to execute")?;
    let name = program
        .to_str()
        .filter(|name| validate_tool_name(name).is_ok());
    // A lock-only project can report how to restore the manifest for a declared tool.
    let root = match project_root() {
        Ok(root) => root,
        Err(error) => name
            .and_then(|name| {
                env::current_dir()
                    .ok()?
                    .ancestors()
                    .find(|directory| directory.join(LOCKFILE).is_file())
                    .filter(|directory| {
                        Lockfile::try_from(directory.join(LOCKFILE).as_path())
                            .is_ok_and(|lockfile| lockfile.tools.contains_key(name))
                    })
                    .map(Path::to_path_buf)
            })
            .ok_or(error)?,
    };

    let mut paths = vec![root.join(TOOLS_DIR).join(".bin")];
    if let Some(existing) = env::var_os("PATH") {
        paths.extend(env::split_paths(&existing));
    }
    let path = env::join_paths(paths).context("failed to construct PATH")?;

    let installed = match name {
        Some(name) => ensure_managed_tool(&root, name)?,
        None => None,
    };
    let mut command = match &installed {
        Some(executable) => Command::new(executable),
        None => Command::new(program),
    };
    #[cfg(unix)]
    if installed.is_some() {
        command.arg0(program);
    }
    command.args(arguments).env("PATH", path);

    #[cfg(unix)]
    {
        let error = command.exec();

        Err(error).with_context(|| format!("failed to execute {}", program.to_string_lossy()))
    }

    #[cfg(not(unix))]
    {
        let status = command
            .status()
            .with_context(|| format!("failed to execute {}", program.to_string_lossy()))?;

        anyhow::ensure!(status.success(), "command exited with {status}");
        Ok(())
    }
}

fn ensure_managed_tool(root: &Path, name: &str) -> Result<Option<PathBuf>> {
    let manifest = Manifest::try_from(root.join(MANIFEST).as_path());
    let lockfile = Lockfile::try_from(root.join(LOCKFILE).as_path());
    let managed = manifest
        .as_ref()
        .is_ok_and(|manifest| manifest.tools.contains_key(name))
        || lockfile
            .as_ref()
            .is_ok_and(|lockfile| lockfile.tools.contains_key(name));
    if !managed {
        return Ok(None);
    }

    let manifest = manifest.with_context(|| {
        format!("cannot run tool {name}; restore {MANIFEST} and run `binloom update`")
    })?;
    let lockfile = lockfile.with_context(|| {
        format!("cannot run tool {name}; check {LOCKFILE} and run `binloom update`")
    })?;
    manifest.ensure_matches_lockfile(&lockfile)?;
    ensure_unreserved_tool_name(name)?;
    let tool = &lockfile.tools[name];

    ensure_tool_installed(
        root,
        name,
        tool,
        &Platform::current()?.to_string(),
        &download::client(),
        InstallOutput::Stderr,
    )?;

    Ok(Some(
        root.join(TOOLS_DIR)
            .join(name)
            .join(&tool.version)
            .join(name),
    ))
}
