use super::env::{Shell, path_text};
use crate::common::{MANIFEST, TOOLS_DIR, project_root};
use anyhow::{Context, Result, ensure};
use clap::Subcommand;
use std::{
    env, fs,
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Print the directory-change hook for your shell's rc file
    Init {
        #[arg(long, value_enum)]
        shell: Shell,
    },
    /// Allow automatic activation of the nearest project at path (default: cwd)
    Trust { path: Option<PathBuf> },
    /// Revoke automatic activation of a project
    Untrust { path: Option<PathBuf> },
    /// Show whether the current project is trusted
    Status,
    #[command(hide = true)]
    HookEnv {
        #[arg(long, value_enum)]
        shell: Shell,
        #[arg(long)]
        interactive: bool,
    },
}

pub(crate) fn run(command: &Command) -> Result<()> {
    match command {
        Command::Init { shell } => init(*shell),
        Command::Trust { path } | Command::Untrust { path } => {
            let root = root_at(path.as_deref())?;
            let trust = matches!(command, Command::Trust { .. });
            set_trust(&config_dir()?, &root, trust)?;
            println!(
                "{}: {}",
                if trust { "Trusted" } else { "Untrusted" },
                root.display()
            );
            Ok(())
        }
        Command::Status => {
            let root = root_at(None)?;
            let trusted = read_trusted(&config_dir()?)?
                .iter()
                .any(|entry| entry == &root);
            println!(
                "{}: {}",
                if trusted {
                    "Trusted"
                } else {
                    "Project is not trusted"
                },
                root.display()
            );
            Ok(())
        }
        Command::HookEnv { shell, interactive } => hook_env(*shell, *interactive),
    }
}

fn root_at(path: Option<&Path>) -> Result<PathBuf> {
    let root = match path {
        Some(path) => {
            let path = path.canonicalize().context("cannot resolve project path")?;
            path.ancestors()
                .find(|root| root.join(MANIFEST).is_file())
                .context("binloom.toml not found at path or any parent directory")?
                .to_path_buf()
        }
        None => project_root()?,
    };
    let root = root.canonicalize()?;
    path_text(&root)?;
    Ok(root)
}

fn config_dir() -> Result<PathBuf> {
    let base = match env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        Some(value) => PathBuf::from(value),
        None => PathBuf::from(env::var_os("HOME").context("HOME is not set")?).join(".config"),
    };
    ensure!(base.is_absolute(), "HOME/XDG_CONFIG_HOME must be absolute");
    Ok(base.join("binloom"))
}

fn reject_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => ensure!(
            !metadata.is_symlink(),
            "refusing symlink: {}",
            path.display()
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn read_optional(path: &Path) -> Result<String> {
    reject_symlink(path)?;
    match fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
    }
}

fn read_trusted(config: &Path) -> Result<Vec<PathBuf>> {
    reject_symlink(config)?;
    Ok(read_optional(&config.join("trusted"))?
        .lines()
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect())
}

fn prepare_config(config: &Path) -> Result<()> {
    reject_symlink(config)?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(config)?;
    fs::set_permissions(config, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn write_private(path: &Path, content: &str) -> Result<()> {
    reject_symlink(path)?;
    let mut temporary = tempfile::NamedTempFile::new_in(path.parent().context("missing parent")?)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temporary.write_all(content.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn set_trust(config: &Path, root: &Path, trust: bool) -> Result<()> {
    let mut roots = read_trusted(config)?;
    roots.retain(|entry| entry != root);
    if trust {
        roots.push(root.to_path_buf());
    }
    let mut content = String::new();
    for root in roots {
        content.push_str(path_text(&root)?);
        content.push('\n');
    }
    prepare_config(config)?;
    write_private(&config.join("trusted"), &content)
}

fn hook_env(shell: Shell, interactive: bool) -> Result<()> {
    let mut paths = env::split_paths(&env::var_os("PATH").unwrap_or_default()).collect::<Vec<_>>();
    // Remove only the occurrence this hook inserted; keep unrelated PATH edits.
    if env::var("BINLOOM_PATH_ADDED").as_deref() == Ok("1")
        && let Some(previous) = env::var_os("BINLOOM_ACTIVE_ROOT")
    {
        let bin = PathBuf::from(previous).join(TOOLS_DIR).join(".bin");
        if let Some(index) = paths.iter().position(|path| path == &bin) {
            paths.remove(index);
        }
    }

    let mut warned = env::var("BINLOOM_WARNED_ROOTS").unwrap_or_default();
    let mut active = None;
    if let Ok(root) = root_at(None) {
        // A missing/unreadable trust store fails closed, but must still deactivate.
        let trusted = config_dir()
            .and_then(|config| read_trusted(&config))
            .unwrap_or_default();
        if trusted.contains(&root) {
            let bin = root.join(TOOLS_DIR).join(".bin");
            if let Ok(resolved) = bin.canonicalize()
                && resolved != root
                && resolved.starts_with(&root)
                && resolved.is_dir()
            {
                active = Some(root);
            }
        } else if interactive && !warned.lines().any(|line| Path::new(line) == root) {
            eprintln!(
                "binloom: project not trusted; run binloom shell trust ({})",
                root.display()
            );
            warned.push_str(path_text(&root)?);
            warned.push('\n');
        }
    }

    let mut added = false;
    if let Some(root) = &active {
        let bin = root.join(TOOLS_DIR).join(".bin");
        if paths.first() != Some(&bin) {
            paths.insert(0, bin);
            added = true;
        }
    }
    let joined = env::join_paths(&paths).context("cannot construct shell PATH")?;
    let joined = joined
        .to_str()
        .context("shell activation requires a UTF-8 PATH")?;
    let mut output = String::new();
    match shell {
        Shell::Bash | Shell::Zsh => {
            output.push_str(&format!("export PATH={}\n", shell.quote(joined)))
        }
        Shell::Fish => {
            output.push_str("set -gx PATH");
            for path in &paths {
                output.push(' ');
                output.push_str(&shell.quote(path.to_str().context("non-UTF-8 PATH")?));
            }
            output.push('\n');
        }
    }
    for (name, value) in [
        (
            "BINLOOM_ACTIVE_ROOT",
            active.as_deref().map(path_text).transpose()?,
        ),
        (
            "BINLOOM_PATH_ADDED",
            active.as_ref().map(|_| if added { "1" } else { "0" }),
        ),
        ("BINLOOM_WARNED_ROOTS", Some(warned.as_str())),
    ] {
        match (shell, value) {
            (Shell::Fish, Some(value)) => {
                output.push_str(&format!("set -gx {name} {}\n", shell.quote(value)))
            }
            (Shell::Fish, None) => output.push_str(&format!("set -e {name}\n")),
            (_, Some(value)) => output.push_str(&format!("export {name}={}\n", shell.quote(value))),
            (_, None) => output.push_str(&format!("unset {name}\n")),
        }
    }
    print!("{output}");
    Ok(())
}

fn init(shell: Shell) -> Result<()> {
    // Keep using the explicitly invoked executable, never one discovered in an
    // untrusted project's PATH or wrapper while changing directories.
    let executable = env::current_exe().context("cannot locate Binloom executable")?;
    let quoted = shell.quote(path_text(&executable)?);
    match shell {
        Shell::Bash | Shell::Zsh => {
            println!("_BINLOOM_EXE={quoted}\nexport BINLOOM_SHELL_HOOK=1");
            print!("{}", POSIX_HOOK.replace("@SHELL@", shell.name()));
            print!(
                "{}",
                match shell {
                    Shell::Bash => BASH_REGISTER,
                    _ => ZSH_REGISTER,
                }
            );
        }
        Shell::Fish => {
            println!("set -g _BINLOOM_EXE {quoted}\nset -gx BINLOOM_SHELL_HOOK 1");
            print!("{FISH_HOOK}");
        }
    }
    Ok(())
}

const POSIX_HOOK: &str = r#"
_binloom_hook() {
    if [ "${_BINLOOM_LAST_PWD+x}" = x ] && [ "$_BINLOOM_LAST_PWD" = "$PWD" ]; then
        return 0
    fi
    _BINLOOM_LAST_PWD=$PWD
    case $- in
        *i*) _binloom_env=$("$_BINLOOM_EXE" shell hook-env --shell @SHELL@ --interactive) ;;
        *) _binloom_env=$("$_BINLOOM_EXE" shell hook-env --shell @SHELL@) ;;
    esac
    eval "$_binloom_env"
}
"#;

const BASH_REGISTER: &str = r#"
case "$(declare -p PROMPT_COMMAND 2>/dev/null)" in
    'declare -a'*)
        case " ${PROMPT_COMMAND[*]} " in
            *' _binloom_hook '*) ;;
            *) PROMPT_COMMAND+=(_binloom_hook) ;;
        esac ;;
    *)
        case ";${PROMPT_COMMAND-};" in
            *';_binloom_hook;'*) ;;
            # This branch handles scalar PROMPT_COMMAND; the branch above handles arrays.
            # shellcheck disable=SC2178,SC2128
            *) PROMPT_COMMAND="_binloom_hook${PROMPT_COMMAND:+; $PROMPT_COMMAND}" ;;
        esac ;;
esac
_binloom_hook
"#;

const ZSH_REGISTER: &str = r#"
autoload -Uz add-zsh-hook
add-zsh-hook precmd _binloom_hook
add-zsh-hook chpwd _binloom_hook
_binloom_hook
"#;

const FISH_HOOK: &str = r#"
function _binloom_hook --on-variable PWD
    if set -q _BINLOOM_LAST_PWD; and test "$_BINLOOM_LAST_PWD" = "$PWD"
        return
    end
    set -g _BINLOOM_LAST_PWD "$PWD"
    set -l flags
    if status is-interactive
        set flags --interactive
    end
    "$_BINLOOM_EXE" shell hook-env --shell fish $flags | source
end
_binloom_hook
"#;
