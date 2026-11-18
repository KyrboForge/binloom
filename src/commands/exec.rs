use super::install::{
    InstallOutput, ensure_project_host, ensure_tool_installed, ensure_unreserved_tool_name,
    join_targets, tool_supports,
};
use crate::{
    common::{LOCKFILE, MANIFEST, TOOLS_DIR},
    domain::{lockfile::Lockfile, manifest::Manifest, platform::Platform},
    download,
};
use std::{
    env,
    ffi::OsString,
    fs, io,
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
    let current = env::current_dir().context("failed to determine current directory")?;
    let root = if let Some(root) = find_metadata(&current, MANIFEST)? {
        root
    } else if let Some(root) = find_metadata(&current, LOCKFILE)?
        && matching_tool_name(&root.join(LOCKFILE), name)?.is_some()
    {
        // A lock-only project can report how to restore a declared tool's manifest.
        root
    } else {
        bail!(
            "{MANIFEST} not found in {} or any parent directory",
            current.display()
        );
    };

    let mut paths = vec![root.join(TOOLS_DIR).join(".bin")];
    if let Some(existing) = env::var_os("PATH") {
        paths.extend(env::split_paths(&existing));
    }
    let path = env::join_paths(paths).context("failed to construct PATH")?;

    // The install lock is close-on-exec and held until `execve`, so no Binloom process can
    // replace the verified executable in between. A same-user process that writes the file
    // in place after hashing is not excluded; closing that needs sealing or supervision.
    let installed = ensure_managed_tool(root, name)?;
    let mut command = match &installed {
        Some((executable, _lock)) => Command::new(executable),
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

fn find_metadata<'a>(current: &'a Path, file: &str) -> Result<Option<&'a Path>> {
    for directory in current.ancestors() {
        if metadata_present(&directory.join(file))? {
            return Ok(Some(directory));
        }
    }
    Ok(None)
}

fn metadata_present(path: &Path) -> Result<bool> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() || (metadata.is_symlink() && path.is_file()) => Ok(true),
        Ok(_) => bail!(
            "expected a regular file at {}; restore it and run `binloom update`",
            path.display()
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(error).with_context(|| format!("check {} and run `binloom update`", path.display()))
        }
    }
}

// Tool table keys still reserve names when an entry fails schema or version validation.
fn matching_tool_name(path: &Path, name: Option<&str>) -> Result<Option<String>> {
    if !metadata_present(path)? {
        return Ok(None);
    }
    let diagnostic = || format!("check {} and run `binloom update`", path.display());
    let contents = fs::read_to_string(path).with_context(diagnostic)?;
    let document = contents.parse::<toml::Table>().with_context(diagnostic)?;
    let Some(tools) = document.get("tools") else {
        return Ok(None);
    };
    let tools = tools
        .as_table()
        .context("expected `tools` to be a table")
        .with_context(diagnostic)?;
    let Some(name) = name else { return Ok(None) };
    if tools.contains_key(name) {
        return Ok(Some(name.to_owned()));
    }
    Ok(tools
        .keys()
        .find(|key| key.eq_ignore_ascii_case(name))
        .cloned())
}

fn ensure_managed_tool(root: &Path, name: Option<&str>) -> Result<Option<(PathBuf, fs::File)>> {
    let matches = [
        matching_tool_name(&root.join(MANIFEST), name)?,
        matching_tool_name(&root.join(LOCKFILE), name)?,
    ];
    let Some(name) = name else { return Ok(None) };
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
    // An excluded managed tool fails here instead of falling back to PATH.
    let host = Platform::current()?;
    ensure_project_host(manifest.project_targets(), host)?;
    if !tool_supports(tool, host) {
        bail!(
            "tool {name} is not available on {host}; its targets are {}",
            join_targets(tool.targets.as_deref().unwrap_or_default())
        );
    }

    let lock = ensure_tool_installed(
        root,
        name,
        tool,
        &host.to_string(),
        &download::client(),
        InstallOutput::Stderr,
    )?;

    Ok(Some((
        root.join(TOOLS_DIR)
            .join(name)
            .join(&tool.version)
            .join(name),
        lock,
    )))
}
