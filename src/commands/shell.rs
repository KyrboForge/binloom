use super::env::{Shell, path_text};
use crate::common::{MANIFEST, TOOLS_DIR, project_root};
use anyhow::{Context, Result, ensure};
use clap::Subcommand;
use std::{
    env, fs,
    io::{self, IsTerminal, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
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
        Some(value) => resolve_base(Path::new(&value))?,
        None => {
            let home = PathBuf::from(env::var_os("HOME").context("HOME is not set")?);
            ensure!(home.is_absolute(), "HOME must be absolute");
            let base = home.canonicalize()?.join(".config");
            reject_symlink(&base)?;
            base
        }
    };
    ensure!(base.is_absolute(), "HOME/XDG_CONFIG_HOME must be absolute");
    Ok(base.join("binloom"))
}

/// Resolve the ancestors of a user-supplied directory like HOME; the directory itself and any
/// components that do not exist yet must not be symlinks.
fn resolve_base(path: &Path) -> Result<PathBuf> {
    ensure!(
        path.is_absolute(),
        "directory must be absolute: {}",
        path.display()
    );
    reject_symlink(path)?;
    let mut existing = path;
    let mut missing = Vec::new();
    while fs::symlink_metadata(existing).is_err() {
        missing.push(existing.file_name().context("directory has no root")?);
        existing = existing.parent().context("directory has no root")?;
    }
    let mut resolved = existing
        .canonicalize()
        .with_context(|| format!("cannot resolve {}", existing.display()))?;
    for component in missing.into_iter().rev() {
        resolved.push(component);
        reject_symlink(&resolved)?;
    }
    Ok(resolved)
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
    prepare_config(config)?;
    let lock_path = config.join("trusted.lock");
    reject_symlink(&lock_path)?;
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)?;
    lock.lock()?;
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
            // The wrapper-only setup has no `binloom` on PATH until a project is trusted.
            eprintln!(
                "binloom: project not trusted; run {} shell trust {}",
                shell.quote(path_text(&env::current_exe()?)?),
                shell.quote(path_text(&root)?)
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
_binloom_deactivate() {
    if [ "${BINLOOM_PATH_ADDED-}" = 1 ] && [ -n "${BINLOOM_ACTIVE_ROOT-}" ]; then
        _binloom_rest=${PATH-}
        _binloom_new=
        _binloom_separator=
        _binloom_removed=0
        while :; do
            _binloom_part=${_binloom_rest%%:*}
            if [ "$_binloom_removed" = 0 ] && [ "$_binloom_part" = "$BINLOOM_ACTIVE_ROOT/.tools/.bin" ]; then
                _binloom_removed=1
            else
                _binloom_new=$_binloom_new$_binloom_separator$_binloom_part
                _binloom_separator=:
            fi
            case $_binloom_rest in
                *:*) _binloom_rest=${_binloom_rest#*:} ;;
                *) break ;;
            esac
        done
        export PATH=$_binloom_new
    fi
    unset BINLOOM_ACTIVE_ROOT BINLOOM_PATH_ADDED
}
_binloom_hook() {
    local _binloom_status=$?
    if [ "${_BINLOOM_LAST_PWD+x}" = x ] && [ "$_BINLOOM_LAST_PWD" = "$PWD" ]; then
        return "$_binloom_status"
    fi
    _BINLOOM_LAST_PWD=$PWD
    if [ ! -x "$_BINLOOM_EXE" ]; then
        _binloom_deactivate
        return "$_binloom_status"
    fi
    case $- in
        *i*) _binloom_env=$("$_BINLOOM_EXE" shell hook-env --shell @SHELL@ --interactive) || { _binloom_deactivate; return "$_binloom_status"; } ;;
        *) _binloom_env=$("$_BINLOOM_EXE" shell hook-env --shell @SHELL@) || { _binloom_deactivate; return "$_binloom_status"; } ;;
    esac
    eval "$_binloom_env"
    return "$_binloom_status"
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
        # This branch handles scalar PROMPT_COMMAND; the branch above handles arrays.
        # shellcheck disable=SC2178,SC2128
        # Appended on its own line so earlier commands still see the user's
        # PIPESTATUS, and a trailing `;` in PROMPT_COMMAND stays valid.
        case ";${PROMPT_COMMAND-};" in
            *';_binloom_hook;'* | *"
_binloom_hook;"*) ;;
            *) PROMPT_COMMAND="${PROMPT_COMMAND:+$PROMPT_COMMAND
}_binloom_hook" ;;
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
    if test -x "$_BINLOOM_EXE"
        set -l activation ("$_BINLOOM_EXE" shell hook-env --shell fish $flags)
        if test $status -eq 0
            printf '%s\n' $activation | source
            return
        end
    end
    if test "$BINLOOM_PATH_ADDED" = 1; and set -q BINLOOM_ACTIVE_ROOT
        set -l index (contains --index -- "$BINLOOM_ACTIVE_ROOT/.tools/.bin" $PATH)
        if test -n "$index"
            set -eg PATH[$index]
        end
    end
    set -e BINLOOM_ACTIVE_ROOT
    set -e BINLOOM_PATH_ADDED
end
_binloom_hook
"#;

struct ActivationSetup<'a> {
    home: &'a Path,
    config: &'a Path,
    shell: Shell,
    root: &'a Path,
    executable: &'a Path,
    /// `$ZDOTDIR` for zsh or `$XDG_CONFIG_HOME` for fish, when set.
    profile_dir: Option<&'a Path>,
}

pub(super) fn maybe_prompt(root: &Path, no_prompt: bool) -> Result<()> {
    if no_prompt
        || env::var("BINLOOM_NO_PROMPT").as_deref() == Ok("1")
        || !io::stdin().is_terminal()
        || !io::stderr().is_terminal()
    {
        return Ok(());
    }
    let shell = env::var_os("SHELL").map(PathBuf::from);
    let shell = match shell
        .as_deref()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
    {
        Some("bash") => Shell::Bash,
        Some("zsh") => Shell::Zsh,
        Some("fish") => Shell::Fish,
        _ => return Ok(()),
    };
    // Tools are already installed, so an unusable environment skips the offer
    // instead of failing `install` before the user was ever asked.
    let paths = (|| -> Result<_> {
        let home = PathBuf::from(env::var_os("HOME").context("HOME is not set")?);
        Ok((home, config_dir()?, env::current_exe()?))
    })();
    let (home, config, executable) = match paths {
        Ok(paths) => paths,
        Err(error) => {
            eprintln!("warning: skipping shell activation setup: {error:#}");
            return Ok(());
        }
    };
    let profile_dir = match shell {
        Shell::Bash => None,
        Shell::Zsh => env::var_os("ZDOTDIR"),
        Shell::Fish => env::var_os("XDG_CONFIG_HOME"),
    }
    .filter(|value| !value.is_empty())
    .map(PathBuf::from);
    let setup = ActivationSetup {
        home: &home,
        config: &config,
        shell,
        root,
        executable: &executable,
        profile_dir: profile_dir.as_deref(),
    };
    // An active hook (possibly `shell init` run by hand) never prompts or edits
    // profiles, but it still repairs a setup the user already consented to.
    if env::var("BINLOOM_SHELL_HOOK").as_deref() == Ok("1") {
        restore_consented(&mut io::stderr().lock(), &setup).map(drop)
    } else {
        prompt_setup(&mut io::stdin().lock(), &mut io::stderr().lock(), &setup)
    }
    .context(
        "shell activation setup failed; installed tools are still available through binloom exec",
    )
}

const BLOCK_BEGIN: &str = "# >>> binloom shell activation >>>";
const BLOCK_END: &str = "# <<< binloom shell activation <<<";

fn profile_paths(home: &Path, shell: Shell, profile_dir: Option<&Path>) -> Result<Vec<PathBuf>> {
    ensure!(home.is_absolute(), "HOME must be absolute");
    let home = home.canonicalize().context("cannot resolve HOME")?;
    let (base, relatives) = match (shell, profile_dir) {
        (Shell::Bash, _) => {
            let mut login = ".bash_profile";
            for candidate in [".bash_profile", ".bash_login", ".profile"] {
                let path = home.join(candidate);
                reject_symlink(&path)?;
                if path.try_exists()? {
                    login = candidate;
                    break;
                }
            }
            (home, vec![".bashrc", login])
        }
        (Shell::Zsh, Some(dir)) => (resolve_base(dir)?, vec![".zshrc"]),
        (Shell::Zsh, None) => (home, vec![".zshrc"]),
        (Shell::Fish, Some(dir)) => (resolve_base(dir)?, vec!["fish/config.fish"]),
        (Shell::Fish, None) => (home, vec![".config/fish/config.fish"]),
    };
    relatives
        .into_iter()
        .map(|relative| {
            let mut path = base.clone();
            // Reject symlink redirects at every level, before reading or creating files.
            for component in Path::new(relative).components() {
                path.push(component);
                reject_symlink(&path)?;
            }
            Ok(path)
        })
        .collect()
}

fn rc_block(shell: Shell, executable: &Path) -> Result<String> {
    let pinned = shell.quote(path_text(executable)?);
    let name = shell.name();
    let warning = format!(
        "echo {} {pinned} {} >&2",
        shell.quote("binloom: shell activation disabled:"),
        shell.quote("is missing; run ./binloomw install in a Binloom project to restore it")
    );
    let body = match shell {
        Shell::Bash | Shell::Zsh => {
            let bash_guard = if matches!(shell, Shell::Bash) {
                " && [ -n \"${BASH_VERSION-}\" ]"
            } else {
                ""
            };
            format!(
                "if [ -z \"${{_BINLOOM_EXE-}}\" ]{bash_guard}; then\n  case $- in\n    *i*)\n      if [ -x {pinned} ]; then\n        eval \"$({pinned} shell init --shell {name})\"\n      else\n        {warning}\n      fi\n      ;;\n  esac\nfi"
            )
        }
        Shell::Fish => format!(
            "if status is-interactive; and not set -q _BINLOOM_EXE\n  if test -x {pinned}\n    {pinned} shell init --shell fish | source\n  else\n    {warning}\n  end\nend"
        ),
    };
    Ok(format!("{BLOCK_BEGIN}\n{body}\n{BLOCK_END}\n"))
}

fn install_stable_executable(config: &Path, source: &Path) -> Result<PathBuf> {
    prepare_config(config)?;
    let directory = config.join("bin");
    reject_symlink(&directory)?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    let target = directory.join("binloom");
    reject_symlink(&target)?;
    // Read before replacing: setup may already be running from the stable path.
    let executable = fs::read(source)
        .with_context(|| format!("cannot read Binloom executable {}", source.display()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o700))?;
    temporary.write_all(&executable)?;
    temporary.as_file().sync_all()?;
    temporary.persist(&target).map_err(|error| error.error)?;
    Ok(target)
}

fn read_profiles(setup: &ActivationSetup<'_>) -> Result<Vec<(PathBuf, String)>> {
    profile_paths(setup.home, setup.shell, setup.profile_dir)?
        .into_iter()
        .map(|path| {
            let existing = read_optional(&path)?;
            ensure!(
                !existing.contains(BLOCK_BEGIN) || existing.contains(BLOCK_END),
                "incomplete Binloom rc block in {}",
                path.display()
            );
            Ok((path, existing))
        })
        .collect()
}

fn append_rc(path: &Path, existing: &str, block: &str) -> Result<()> {
    let parent = path.parent().context("rc path has no parent")?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    let metadata = fs::metadata(path).ok();
    if metadata.is_some() {
        let backup = path.with_extension("binloom.bak");
        reject_symlink(&backup)?;
        if !backup.try_exists()? {
            let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
            temporary.write_all(existing.as_bytes())?;
            temporary.as_file().sync_all()?;
            temporary
                .persist_noclobber(&backup)
                .map_err(|error| error.error)?;
        }
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    if let Some(metadata) = metadata {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())?;
    }
    temporary.write_all(existing.as_bytes())?;
    if !existing.is_empty() && !existing.ends_with('\n') {
        writeln!(temporary)?;
    }
    temporary.write_all(block.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// Restores the pinned executable after a saved "yes"; returns whether the
/// user has already answered the prompt.
fn restore_consented(output: &mut impl Write, setup: &ActivationSetup<'_>) -> Result<bool> {
    reject_symlink(setup.config)?;
    let answer = read_optional(&setup.config.join("activation-prompt"))?;
    if answer == "yes\n" {
        let directory = setup.config.join("bin");
        reject_symlink(&directory)?;
        let stable = directory.join("binloom");
        reject_symlink(&stable)?;
        if !stable.try_exists()? {
            install_stable_executable(setup.config, setup.executable)?;
            writeln!(
                output,
                "Binloom shell activation executable restored at {}.",
                stable.display()
            )?;
        }
    }
    Ok(!answer.is_empty())
}

fn prompt_setup(
    input: &mut impl io::BufRead,
    output: &mut impl Write,
    setup: &ActivationSetup<'_>,
) -> Result<()> {
    if restore_consented(output, setup)? {
        return Ok(());
    }
    let marker = setup.config.join("activation-prompt");
    if read_profiles(setup)?
        .iter()
        .all(|(_, existing)| existing.contains(BLOCK_BEGIN))
    {
        return Ok(());
    }
    write!(
        output,
        "Enable automatic project-local commands in your shell? [y/N]"
    )?;
    output.flush()?;
    let mut answer = String::new();
    input.read_line(&mut answer)?;
    writeln!(output)?;
    prepare_config(setup.config)?;
    if !answer.trim().eq_ignore_ascii_case("y") {
        return write_private(&marker, "no\n");
    }
    // Consent can take arbitrarily long: recheck redirects and preserve edits
    // made by the user (or another setup process) while the prompt was open.
    let profiles = read_profiles(setup)?;
    let root = setup.root.canonicalize()?;
    ensure!(
        root.join(MANIFEST).is_file(),
        "binloom.toml not found in project"
    );
    let stable = install_stable_executable(setup.config, setup.executable)?;
    let block = rc_block(setup.shell, &stable)?;
    set_trust(setup.config, &root, true)?;
    for (rc, existing) in &profiles {
        if !existing.contains(BLOCK_BEGIN) {
            append_rc(rc, existing, &block)?;
        }
    }
    write_private(&marker, "yes\n")?;
    writeln!(
        output,
        "Shell activation configured in {}. Open a new shell or source the configured files. Remove the marked Binloom block to disable it.",
        profiles
            .iter()
            .map(|(rc, _)| rc.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn profile_environment_directories() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path().canonicalize().unwrap();
        let zdot = home.join("zsh");
        let xdg = home.join("xdg");
        assert_eq!(
            profile_paths(&home, Shell::Zsh, Some(&zdot)).unwrap(),
            vec![zdot.join(".zshrc")]
        );
        assert_eq!(
            profile_paths(&home, Shell::Fish, Some(&xdg)).unwrap(),
            vec![xdg.join("fish/config.fish")]
        );
        assert_eq!(
            profile_paths(&home, Shell::Fish, None).unwrap(),
            vec![home.join(".config/fish/config.fish")]
        );
        assert!(profile_paths(&home, Shell::Zsh, Some(Path::new("relative"))).is_err());
        std::os::unix::fs::symlink(&xdg, &zdot).unwrap();
        assert!(profile_paths(&home, Shell::Zsh, Some(&zdot)).is_err());
        fs::remove_file(&zdot).unwrap();
        std::os::unix::fs::symlink(&zdot, &xdg).unwrap();
        assert!(profile_paths(&home, Shell::Fish, Some(&xdg)).is_err());
        // Ancestors resolve like HOME; missing tails are created below the real directory.
        fs::create_dir_all(home.join("real/zsh")).unwrap();
        std::os::unix::fs::symlink(home.join("real"), home.join("linked")).unwrap();
        assert_eq!(
            profile_paths(&home, Shell::Zsh, Some(&home.join("linked/zsh"))).unwrap(),
            vec![home.join("real/zsh/.zshrc")]
        );
        assert_eq!(
            profile_paths(&home, Shell::Fish, Some(&home.join("linked/missing/xdg"))).unwrap(),
            vec![home.join("real/missing/xdg/fish/config.fish")]
        );
    }

    #[test]
    fn rc_block_never_uses_path_fallback() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let block = rc_block(shell, Path::new("/missing/binloom")).unwrap();
            assert!(!block.contains("command -v"));
            assert!(!block.contains("command -q"));
        }
    }

    #[test]
    fn consent_keeps_profile_edits_made_while_waiting_for_the_answer() {
        struct EditingReader {
            rc: PathBuf,
            input: Cursor<&'static [u8]>,
        }
        impl io::Read for EditingReader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                self.input.read(buffer)
            }
        }
        impl io::BufRead for EditingReader {
            fn fill_buf(&mut self) -> io::Result<&[u8]> {
                fs::write(&self.rc, "# edited during the prompt\n")?;
                self.input.fill_buf()
            }
            fn consume(&mut self, amount: usize) {
                self.input.consume(amount);
            }
        }
        let home = tempfile::tempdir().unwrap();
        let rc = home.path().join(".bashrc");
        fs::write(&rc, "# original\n").unwrap();
        fs::write(home.path().join(MANIFEST), "").unwrap();
        let config = home.path().join(".config/binloom");
        let setup = ActivationSetup {
            home: home.path(),
            config: &config,
            shell: Shell::Bash,
            root: home.path(),
            executable: &env::current_exe().unwrap(),
            profile_dir: None,
        };
        let mut input = EditingReader {
            rc: rc.clone(),
            input: Cursor::new(b"y\n"),
        };
        prompt_setup(&mut input, &mut Vec::new(), &setup).unwrap();
        assert!(
            fs::read_to_string(rc)
                .unwrap()
                .starts_with("# edited during the prompt\n")
        );
    }

    #[test]
    fn consent_yes_preserves_rc_backs_it_up_trusts_and_appends_once() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let home = tempfile::tempdir().unwrap();
            let home = home.path().canonicalize().unwrap();
            let root = home.join("project ' special");
            fs::create_dir(&root).unwrap();
            fs::write(root.join(MANIFEST), "").unwrap();
            let config = home.join(".config/binloom");
            let rc = home.join(match shell {
                Shell::Bash => ".bashrc",
                Shell::Zsh => ".zshrc",
                Shell::Fish => ".config/fish/config.fish",
            });
            fs::create_dir_all(rc.parent().unwrap()).unwrap();
            fs::write(&rc, "# keep my config\n").unwrap();
            let setup = ActivationSetup {
                home: &home,
                config: &config,
                shell,
                root: &root,
                executable: &env::current_exe().unwrap(),
                profile_dir: None,
            };
            let mut output = Vec::new();
            prompt_setup(&mut Cursor::new(b"y\n"), &mut output, &setup).unwrap();
            let prompt = String::from_utf8(output).unwrap();
            assert!(
                prompt.starts_with("Enable automatic project-local commands in your shell? [y/N]")
            );
            let content = fs::read_to_string(&rc).unwrap();
            assert!(content.starts_with("# keep my config\n"));
            assert_eq!(
                content
                    .matches("# >>> binloom shell activation >>>")
                    .count(),
                1
            );
            assert!(content.contains(&format!("shell init --shell {}", shell.name())));
            assert_eq!(
                fs::read_to_string(rc.with_extension("binloom.bak")).unwrap(),
                "# keep my config\n"
            );
            assert_eq!(
                fs::read_to_string(config.join("activation-prompt")).unwrap(),
                "yes\n"
            );
            assert_eq!(read_trusted(&config).unwrap(), std::slice::from_ref(&root));
            let mut second = Vec::new();
            prompt_setup(&mut Cursor::new(b"y\n"), &mut second, &setup).unwrap();
            assert!(second.is_empty());
            assert_eq!(fs::read_to_string(rc).unwrap(), content);
        }
    }

    #[test]
    fn consent_defaults_to_no_and_remembers_it() {
        for answer in ["", "\n", "n\n"] {
            let home = tempfile::tempdir().unwrap();
            let config = home.path().join(".config/binloom");
            let setup = ActivationSetup {
                home: home.path(),
                config: &config,
                shell: Shell::Bash,
                root: home.path(),
                executable: &env::current_exe().unwrap(),
                profile_dir: None,
            };
            let mut output = Vec::new();
            prompt_setup(&mut Cursor::new(answer), &mut output, &setup).unwrap();
            assert!(!home.path().join(".bashrc").exists());
            assert!(!config.join("trusted").exists());
            assert_eq!(
                fs::read_to_string(config.join("activation-prompt")).unwrap(),
                "no\n"
            );
            output.clear();
            prompt_setup(&mut Cursor::new(b"y\n"), &mut output, &setup).unwrap();
            assert!(output.is_empty());
        }
    }

    #[test]
    fn consent_rejects_rc_symlinks_and_symlinked_fish_parent() {
        for shell in [Shell::Bash, Shell::Fish] {
            let home = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let config = home.path().join(".config/binloom");
            if matches!(shell, Shell::Bash) {
                fs::write(outside.path().join("rc"), "untouched").unwrap();
                std::os::unix::fs::symlink(outside.path().join("rc"), home.path().join(".bashrc"))
                    .unwrap();
            } else {
                fs::create_dir(home.path().join(".config")).unwrap();
                std::os::unix::fs::symlink(outside.path(), home.path().join(".config/fish"))
                    .unwrap();
            }
            let setup = ActivationSetup {
                home: home.path(),
                config: &config,
                shell,
                root: home.path(),
                executable: &env::current_exe().unwrap(),
                profile_dir: None,
            };
            assert!(prompt_setup(&mut Cursor::new(b"y\n"), &mut Vec::new(), &setup).is_err());
            assert!(!config.join("activation-prompt").exists());
            if matches!(shell, Shell::Bash) {
                assert_eq!(
                    fs::read_to_string(outside.path().join("rc")).unwrap(),
                    "untouched"
                );
            } else {
                assert!(!outside.path().join("config.fish").exists());
            }
        }
    }

    struct SetupFixture {
        home: tempfile::TempDir,
        config: PathBuf,
        executable: PathBuf,
    }

    impl SetupFixture {
        fn new() -> Self {
            let home = tempfile::Builder::new()
                .prefix("binloom ' $test")
                .tempdir()
                .unwrap();
            let config = home.path().join(".config/binloom");
            let executable = home.path().join("setup-binloom");
            fs::write(home.path().join(MANIFEST), "").unwrap();
            fs::write(&executable, "#!/bin/sh\nprintf '%s\\n' '_BINLOOM_EXE=/initialized' 'BINLOOM_INIT_COUNT=$((${BINLOOM_INIT_COUNT:-0} + 1))'\n").unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                home,
                config,
                executable,
            }
        }

        fn setup(&self) -> ActivationSetup<'_> {
            ActivationSetup {
                home: self.home.path(),
                config: &self.config,
                shell: Shell::Bash,
                root: self.home.path(),
                executable: &self.executable,
                profile_dir: None,
            }
        }

        fn consent(&self) -> String {
            let mut output = Vec::new();
            prompt_setup(&mut Cursor::new(b"y\n"), &mut output, &self.setup()).unwrap();
            String::from_utf8(output).unwrap()
        }
    }

    #[test]
    fn consent_installs_stable_executable_and_pins_it_after_original_is_deleted() {
        let fixture = SetupFixture::new();
        fixture.consent();
        let stable = fixture.config.join("bin/binloom");
        assert_eq!(
            fs::read(&stable).unwrap(),
            fs::read(&fixture.executable).unwrap()
        );
        for path in [fixture.config.join("bin"), stable.clone()] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        fs::remove_file(&fixture.executable).unwrap();
        for name in [".bashrc", ".bash_profile"] {
            let block = fs::read_to_string(fixture.home.path().join(name)).unwrap();
            assert!(block.contains(&Shell::Bash.quote(path_text(&stable).unwrap())));
            assert!(!block.contains(path_text(&fixture.executable).unwrap()));
        }
        assert!(
            std::process::Command::new(&stable)
                .output()
                .unwrap()
                .status
                .success()
        );
    }

    #[test]
    fn consent_restores_missing_stable_copy_only_for_yes_marker() {
        let fixture = SetupFixture::new();
        fixture.consent();
        let stable = fixture.config.join("bin/binloom");
        fs::remove_file(&stable).unwrap();
        let mut output = Vec::new();
        prompt_setup(&mut Cursor::new(b""), &mut output, &fixture.setup()).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert_eq!(output.lines().count(), 1);
        assert!(output.contains("restored"));
        assert_eq!(
            fs::read(&stable).unwrap(),
            fs::read(&fixture.executable).unwrap()
        );
        fs::remove_file(&stable).unwrap();
        fs::write(fixture.config.join("activation-prompt"), "no\n").unwrap();
        let mut output = Vec::new();
        prompt_setup(&mut Cursor::new(b"y\n"), &mut output, &fixture.setup()).unwrap();
        assert!(output.is_empty());
        assert!(!stable.exists());
    }

    #[test]
    fn active_hook_restores_consented_copy_without_prompting() {
        let fixture = SetupFixture::new();
        let mut output = Vec::new();
        assert!(!restore_consented(&mut output, &fixture.setup()).unwrap());
        assert!(output.is_empty());
        assert!(!fixture.config.exists());
        assert!(!fixture.home.path().join(".bashrc").exists());

        fixture.consent();
        let stable = fixture.config.join("bin/binloom");
        fs::remove_file(&stable).unwrap();
        assert!(restore_consented(&mut output, &fixture.setup()).unwrap());
        assert!(String::from_utf8(output).unwrap().contains("restored"));
        assert_eq!(
            fs::read(&stable).unwrap(),
            fs::read(&fixture.executable).unwrap()
        );
    }

    #[test]
    fn bash_configures_first_login_file_and_bashrc() {
        for existing in [
            vec![],
            vec![".profile"],
            vec![".bash_login", ".profile"],
            vec![".bash_profile", ".profile"],
        ] {
            let fixture = SetupFixture::new();
            for name in &existing {
                fs::write(fixture.home.path().join(name), "# existing\n").unwrap();
            }
            let login = existing.first().copied().unwrap_or(".bash_profile");
            let output = fixture.consent();
            for name in [".bashrc", login] {
                let rc = fixture.home.path().join(name);
                assert!(fs::read_to_string(&rc).unwrap().contains(BLOCK_BEGIN));
                assert!(output.contains(path_text(&rc).unwrap()));
            }
            for name in existing.into_iter().skip(1) {
                assert_eq!(
                    fs::read_to_string(fixture.home.path().join(name)).unwrap(),
                    "# existing\n"
                );
            }
        }
    }

    #[test]
    fn complete_blocks_in_all_profiles_skip_prompt_without_marker() {
        let fixture = SetupFixture::new();
        fixture.consent();
        fs::remove_file(fixture.config.join("activation-prompt")).unwrap();
        let before = [".bashrc", ".bash_profile"]
            .map(|name| fs::read_to_string(fixture.home.path().join(name)).unwrap());
        assert!(fixture.consent().is_empty());
        let after = [".bashrc", ".bash_profile"]
            .map(|name| fs::read_to_string(fixture.home.path().join(name)).unwrap());
        assert_eq!(before, after);
    }

    #[test]
    fn consent_appends_only_missing_blocks_and_rejects_incomplete_login_block() {
        let fixture = SetupFixture::new();
        let rc = fixture.home.path().join(".bashrc");
        let existing = rc_block(Shell::Bash, &fixture.config.join("bin/binloom")).unwrap();
        fs::write(&rc, &existing).unwrap();
        assert!(fixture.consent().contains("Enable automatic"));
        assert_eq!(fs::read_to_string(&rc).unwrap(), existing);
        assert!(!rc.with_extension("binloom.bak").exists());
        fs::remove_file(fixture.config.join("activation-prompt")).unwrap();
        fs::write(fixture.home.path().join(".bash_profile"), BLOCK_BEGIN).unwrap();
        assert!(prompt_setup(&mut Cursor::new(b"y\n"), &mut Vec::new(), &fixture.setup()).is_err());
    }

    #[test]
    fn rc_blocks_guard_initialization_and_warn_when_executable_is_missing() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let block = rc_block(shell, Path::new("/missing ' binloom")).unwrap();
            assert!(block.contains("_BINLOOM_EXE"));
            assert!(block.contains("shell activation disabled:"));
            assert!(block.contains("run ./binloomw install"));
            assert!(block.contains(">&2"));
            match shell {
                Shell::Bash | Shell::Zsh => {
                    assert!(block.contains("${_BINLOOM_EXE-}"));
                    assert!(block.contains("case $- in"));
                    assert!(block.contains("*i*)"));
                    assert_eq!(
                        block.contains("${BASH_VERSION-}"),
                        matches!(shell, Shell::Bash)
                    );
                }
                Shell::Fish => {
                    assert!(block.contains("if status is-interactive; and not set -q _BINLOOM_EXE"))
                }
            }
        }
    }

    #[test]
    fn bash_login_profile_initializes_once_with_bashrc() {
        if std::process::Command::new("bash")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let fixture = SetupFixture::new();
        fixture.consent();
        fs::remove_file(&fixture.executable).unwrap();
        let output = std::process::Command::new("bash")
            .args(["--noprofile", "--norc", "-lic", "set -e; . \"$HOME/.bash_profile\"; . \"$HOME/.bashrc\"; test \"$BINLOOM_INIT_COUNT\" = 1"])
            .env("HOME", fixture.home.path())
            .env("XDG_CONFIG_HOME", fixture.home.path().join(".config"))
            .env_remove("_BINLOOM_EXE")
            .env_remove("BINLOOM_INIT_COUNT")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("binloom:"));
    }

    #[test]
    fn stable_executable_replacement_is_safe_when_source_is_target() {
        let fixture = SetupFixture::new();
        fixture.consent();
        let stable = fixture.config.join("bin/binloom");
        let original = fs::read(&stable).unwrap();
        fs::remove_file(fixture.config.join("activation-prompt")).unwrap();
        fs::remove_file(fixture.home.path().join(".bash_profile")).unwrap();
        let setup = ActivationSetup {
            executable: &stable,
            ..fixture.setup()
        };
        prompt_setup(&mut Cursor::new(b"y\n"), &mut Vec::new(), &setup).unwrap();
        assert_eq!(fs::read(stable).unwrap(), original);
    }

    #[test]
    fn consent_rejects_stable_directory_target_and_login_symlinks() {
        for redirect in ["bin", "bin/binloom", "profile"] {
            let fixture = SetupFixture::new();
            let outside = fixture.home.path().join("outside");
            fs::create_dir_all(&fixture.config).unwrap();
            let link = match redirect {
                "bin" => {
                    fs::create_dir(&outside).unwrap();
                    fixture.config.join("bin")
                }
                "bin/binloom" => {
                    fs::create_dir(fixture.config.join("bin")).unwrap();
                    fs::write(&outside, "untouched").unwrap();
                    fixture.config.join("bin/binloom")
                }
                _ => {
                    fs::write(&outside, "untouched").unwrap();
                    fixture.home.path().join(".bash_profile")
                }
            };
            std::os::unix::fs::symlink(&outside, link).unwrap();
            assert!(
                prompt_setup(&mut Cursor::new(b"y\n"), &mut Vec::new(), &fixture.setup()).is_err()
            );
            assert!(!fixture.config.join("activation-prompt").exists());
            assert!(!fixture.config.join("trusted").exists());
            if redirect == "bin" {
                assert!(!outside.join("binloom").exists());
            } else {
                assert_eq!(fs::read_to_string(outside).unwrap(), "untouched");
            }
        }
    }

    #[test]
    fn bash_block_warns_only_in_interactive_bash_when_copy_is_missing() {
        if std::process::Command::new("bash")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let fixture = SetupFixture::new();
        fixture.consent();
        fs::remove_file(fixture.config.join("bin/binloom")).unwrap();
        for (shell, flags, warn) in [
            ("bash", "-ic", true),
            ("bash", "-c", false),
            ("sh", "-ic", false),
        ] {
            let mut command = std::process::Command::new(shell);
            if shell == "bash" {
                command.args(["--noprofile", "--norc"]);
            }
            // macOS sh is Bash too; clear its marker to test a shell without BASH_VERSION.
            let output = command
                .args([
                    flags,
                    "if [ \"$0\" = sh ]; then unset BASH_VERSION; fi; . \"$HOME/.bash_profile\"",
                ])
                .env("HOME", fixture.home.path())
                .env("XDG_CONFIG_HOME", fixture.home.path().join(".config"))
                .env_remove("_BINLOOM_EXE")
                .env_remove("BASH_VERSION")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("binloom: shell activation disabled:"),
                warn
            );
        }
    }
}
