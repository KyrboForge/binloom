use super::install::{InstallOutput, ensure_tool_installed, ensure_unreserved_tool_name};
use crate::{
    common::{LOCKFILE, MANIFEST, TOOLS_DIR, project_root},
    domain::{lockfile::Lockfile, manifest::Manifest, platform::Platform},
    download,
};
use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

pub(crate) fn exec(arguments: &[OsString]) -> Result<()> {
    let (program, arguments) = arguments
        .split_first()
        .context("missing command to execute")?;
    let name = program.to_str();
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
                        matching_tool_name(&directory.join(LOCKFILE), name).is_some()
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

// Tool table keys still reserve names when an entry fails schema or version validation.
fn matching_tool_name(path: &Path, name: &str) -> Option<String> {
    let contents = fs::read_to_string(path).ok()?;
    let document = contents.parse::<toml::Table>().ok()?;
    let tools = document.get("tools")?.as_table()?;
    if tools.contains_key(name) {
        return Some(name.to_owned());
    }
    tools
        .keys()
        .find(|key| key.eq_ignore_ascii_case(name))
        .cloned()
}

fn ensure_managed_tool(root: &Path, name: &str) -> Result<Option<PathBuf>> {
    let matches = [MANIFEST, LOCKFILE].map(|file| matching_tool_name(&root.join(file), name));
    if !matches.iter().any(|key| key.as_deref() == Some(name)) {
        if let Some(canonical) = matches.iter().flatten().next() {
            bail!("`{name}` matches managed tool `{canonical}`; run `binloomw exec {canonical}`");
        }
        return Ok(None);
    }

    let manifest = Manifest::try_from(root.join(MANIFEST).as_path()).with_context(|| {
        format!("cannot run tool {name}; restore {MANIFEST} and run `binloom update`")
    })?;
    let lockfile = Lockfile::try_from(root.join(LOCKFILE).as_path()).with_context(|| {
        format!("cannot run tool {name}; check {LOCKFILE} and run `binloom update`")
    })?;
    manifest.ensure_matches_lockfile(&lockfile)?;
    ensure_unreserved_tool_name(name)?;
    let tool = lockfile
        .tools
        .get(name)
        .with_context(|| format!("missing lock entry for {name}; run `binloom update`"))?;

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
