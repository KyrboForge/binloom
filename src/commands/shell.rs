use super::env::{Shell, path_text};
use crate::common::{MANIFEST, TOOLS_DIR, project_root};
use anyhow::{Context, Result, ensure};
use clap::Subcommand;
use std::{
    env, fs,
    io::{self, IsTerminal, Write},
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
    if [ "${_BINLOOM_LAST_PWD+x}" = x ] && [ "$_BINLOOM_LAST_PWD" = "$PWD" ]; then
        return 0
    fi
    _BINLOOM_LAST_PWD=$PWD
    if [ ! -x "$_BINLOOM_EXE" ]; then
        _binloom_deactivate
        return 0
    fi
    case $- in
        *i*) _binloom_env=$("$_BINLOOM_EXE" shell hook-env --shell @SHELL@ --interactive) || { _binloom_deactivate; return 0; } ;;
        *) _binloom_env=$("$_BINLOOM_EXE" shell hook-env --shell @SHELL@) || { _binloom_deactivate; return 0; } ;;
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
        # This branch handles scalar PROMPT_COMMAND; the branch above handles arrays.
        # shellcheck disable=SC2178,SC2128
        case ";${PROMPT_COMMAND-};" in
            *';_binloom_hook;'*) ;;
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
}

pub(super) fn maybe_prompt(root: &Path, no_prompt: bool) -> Result<()> {
    if no_prompt
        || env::var("BINLOOM_NO_PROMPT").as_deref() == Ok("1")
        || !io::stdin().is_terminal()
        || !io::stderr().is_terminal()
        || env::var("BINLOOM_SHELL_HOOK").as_deref() == Ok("1")
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
    let home = PathBuf::from(env::var_os("HOME").context("HOME is not set")?);
    let config = config_dir()?;
    let executable = env::current_exe()?;
    prompt_setup(
        &mut io::stdin().lock(),
        &mut io::stderr().lock(),
        &ActivationSetup {
            home: &home,
            config: &config,
            shell,
            root,
            executable: &executable,
        },
    )
    .context(
        "shell activation setup failed; installed tools are still available through binloom exec",
    )
}

const BLOCK_BEGIN: &str = "# >>> binloom shell activation >>>";
const BLOCK_END: &str = "# <<< binloom shell activation <<<";

fn profile_path(home: &Path, shell: Shell) -> Result<PathBuf> {
    ensure!(home.is_absolute(), "HOME must be absolute");
    let mut path = home.canonicalize().context("cannot resolve HOME")?;
    let relative = match shell {
        Shell::Bash => ".bashrc",
        Shell::Zsh => ".zshrc",
        Shell::Fish => ".config/fish/config.fish",
    };
    // Reject symlink redirects at every level, before reading or creating files.
    for component in Path::new(relative).components() {
        path.push(component);
        reject_symlink(&path)?;
    }
    Ok(path)
}

fn rc_block(shell: Shell, executable: &Path) -> Result<String> {
    let fallback = shell.quote(path_text(executable)?);
    let name = shell.name();
    let body = match shell {
        Shell::Bash | Shell::Zsh => format!(
            "if command -v binloom >/dev/null 2>&1; then\n    eval \"$(binloom shell init --shell {name})\"\nelif [ -x {fallback} ]; then\n    eval \"$({fallback} shell init --shell {name})\"\nfi"
        ),
        Shell::Fish => format!(
            "if command -q binloom\n    binloom shell init --shell fish | source\nelse if test -x {fallback}\n    {fallback} shell init --shell fish | source\nend"
        ),
    };
    Ok(format!("{BLOCK_BEGIN}\n{body}\n{BLOCK_END}\n"))
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

fn prompt_setup(
    input: &mut impl io::BufRead,
    output: &mut impl Write,
    setup: &ActivationSetup<'_>,
) -> Result<()> {
    reject_symlink(setup.config)?;
    let marker = setup.config.join("activation-prompt");
    if !read_optional(&marker)?.is_empty() {
        return Ok(());
    }
    let rc = profile_path(setup.home, setup.shell)?;
    let existing = read_optional(&rc)?;
    if existing.contains(BLOCK_BEGIN) {
        ensure!(
            existing.contains(BLOCK_END),
            "incomplete Binloom rc block in {}",
            rc.display()
        );
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
    let rc = profile_path(setup.home, setup.shell)?;
    let existing = read_optional(&rc)?;
    let configured = existing.contains(BLOCK_BEGIN);
    ensure!(
        !configured || existing.contains(BLOCK_END),
        "incomplete Binloom rc block in {}",
        rc.display()
    );
    let root = setup.root.canonicalize()?;
    ensure!(
        root.join(MANIFEST).is_file(),
        "binloom.toml not found in project"
    );
    let block = rc_block(setup.shell, setup.executable)?;
    set_trust(setup.config, &root, true)?;
    if !configured {
        append_rc(&rc, &existing, &block)?;
    }
    write_private(&marker, "yes\n")?;
    writeln!(
        output,
        "Shell activation configured in {}. Open a new shell or source that file. Remove the marked Binloom block to disable it.",
        rc.display()
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

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
            executable: Path::new("/cached/binloom"),
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
                executable: Path::new("/cached/binloom"),
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
                executable: Path::new("/cached/binloom"),
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
                executable: Path::new("/cached/binloom"),
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
}
