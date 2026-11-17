use crate::common::{TOOLS_DIR, project_root};
use anyhow::{Context, Result, ensure};
use clap::ValueEnum;
use std::path::Path;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum Shell {
    Bash,
    Zsh,
    Fish,
}

impl Shell {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
        }
    }

    pub(super) fn quote(self, value: &str) -> String {
        let escaped = match self {
            Self::Bash | Self::Zsh => value.replace('\'', "'\\''"),
            Self::Fish => value.replace('\\', "\\\\").replace('\'', "\\'"),
        };
        format!("'{escaped}'")
    }
}

pub(super) fn path_text(path: &Path) -> Result<&str> {
    let value = path
        .to_str()
        .context("shell activation requires UTF-8 paths")?;
    ensure!(
        !value.contains([':', '\n', '\r']),
        "shell activation paths cannot contain colons or newlines"
    );
    Ok(value)
}

pub(crate) fn env(shell: Shell) -> Result<()> {
    let bin = project_root()?.join(TOOLS_DIR).join(".bin");
    let quoted = shell.quote(path_text(&bin)?);
    match shell {
        Shell::Bash | Shell::Zsh => println!(
            "case \"${{PATH-}}\" in\n  {quoted}|{quoted}:*) ;;\n  *) export PATH={quoted}\"${{PATH+:$PATH}}\" ;;\nesac"
        ),
        Shell::Fish => println!(
            "if not set -q PATH[1]; or test \"$PATH[1]\" != {quoted}\n    set -gx PATH {quoted} $PATH\nend"
        ),
    }
    Ok(())
}
