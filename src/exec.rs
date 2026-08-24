use std::{env, ffi::OsString, process::Command};

use anyhow::{Context, Result};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

pub fn exec(arguments: &[OsString]) -> Result<()> {
    let (program, arguments) = arguments
        .split_first()
        .context("missing command to execute")?;

    let mut paths = vec![
        env::current_dir()
            .context("failed to determine current directory")?
            .join(".tools/.bin"),
    ];

    if let Some(existing) = env::var_os("PATH") {
        paths.extend(env::split_paths(&existing));
    }

    let path = env::join_paths(paths).context("failed to construct PATH")?;

    let mut command = Command::new(program);
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
