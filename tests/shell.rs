use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output},
};

struct Fixture {
    home: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let root = home
            .path()
            .canonicalize()
            .unwrap()
            .join("project ' $special");
        fs::create_dir_all(root.join(".tools/.bin")).unwrap();
        fs::create_dir(root.join("nested")).unwrap();
        fs::write(root.join("binloom.toml"), "").unwrap();
        Self { home, root }
    }

    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut cmd = Command::new(program);
        let mut paths = vec![
            Path::new(env!("CARGO_BIN_EXE_binloom"))
                .parent()
                .unwrap()
                .to_path_buf(),
        ];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        cmd.env("HOME", self.home.path())
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("XDG_CONFIG_HOME", self.home.path().join(".config"))
            .env("BIN", env!("CARGO_BIN_EXE_binloom"))
            .env("ROOT", &self.root)
            .env_remove("BINLOOM_ACTIVE_ROOT")
            .env_remove("BINLOOM_PATH_ADDED")
            .env_remove("BINLOOM_WARNED_ROOTS")
            .env_remove("BINLOOM_SHELL_HOOK")
            .current_dir(self.root.join("nested"));
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(env!("CARGO_BIN_EXE_binloom"))
            .args(args)
            .output()
            .unwrap()
    }

    fn trust(&self) {
        success(self.run(&["shell", "trust"]));
    }

    fn bash(&self, script: &str) -> Output {
        self.command("bash")
            .args(["--noprofile", "--norc", "-c", script])
            .output()
            .unwrap()
    }
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn trust_untrust_status_and_private_files() {
    let fixture = Fixture::new();
    assert!(success(fixture.run(&["shell", "status"])).contains("not trusted"));
    fixture.trust();
    fixture.trust();
    let config = fixture.home.path().join(".config/binloom");
    assert_eq!(
        fs::read_to_string(config.join("trusted")).unwrap(),
        format!("{}\n", fixture.root.display())
    );
    assert_eq!(
        fs::metadata(&config).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(config.join("trusted"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(success(fixture.run(&["shell", "status"])).starts_with("Trusted:"));
    success(fixture.run(&["shell", "untrust"]));
    assert!(success(fixture.run(&["shell", "status"])).contains("not trusted"));
    let alias = fixture.home.path().join("alias");
    symlink(&fixture.root, &alias).unwrap();
    success(fixture.run(&["shell", "trust", alias.to_str().unwrap()]));
    assert_eq!(
        fs::read_to_string(config.join("trusted")).unwrap(),
        format!("{}\n", fixture.root.display())
    );
}

#[test]
fn init_prints_native_hooks_for_each_shell() {
    let fixture = Fixture::new();
    for (shell, marker) in [
        ("bash", "PROMPT_COMMAND"),
        ("zsh", "add-zsh-hook"),
        ("fish", "--on-variable PWD"),
    ] {
        let snippet = success(fixture.run(&["shell", "init", "--shell", shell]));
        assert!(snippet.contains(marker));
        assert!(snippet.contains("shell hook-env"));
        assert!(snippet.contains("BINLOOM_SHELL_HOOK"));
    }
}

#[test]
fn bash_hook_requires_trust_and_is_silent_without_interactivity() {
    let fixture = Fixture::new();
    let output = fixture.bash(
        r#"
        original=$PATH
        eval "$("$BIN" shell init --shell bash)"
        _binloom_hook
        test "$PATH" = "$original" && test -z "${BINLOOM_ACTIVE_ROOT-}"
    "#,
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    success(output);
}

#[test]
fn bash_hook_switches_nearest_projects_and_restores_path() {
    let fixture = Fixture::new();
    fixture.trust();
    let child = fixture.root.join("nested/child");
    fs::create_dir_all(child.join(".tools/.bin")).unwrap();
    fs::write(child.join("binloom.toml"), "").unwrap();
    success(fixture.run(&["shell", "trust", child.to_str().unwrap()]));
    success(fixture.bash(
        r#"
        set -e
        original=$PATH
        PROMPT_COMMAND='printf preserved'
        eval "$("$BIN" shell init --shell bash)"
        eval "$("$BIN" shell init --shell bash)"
        test "${PATH%%:*}" = "$ROOT/.tools/.bin"
        test "$PATH" = "$ROOT/.tools/.bin:$original"
        test "$BINLOOM_ACTIVE_ROOT" = "$ROOT"
        _binloom_hook
        test "$PATH" = "$ROOT/.tools/.bin:$original"
        cd "$ROOT/nested/child"
        _binloom_hook
        test "$PATH" = "$ROOT/nested/child/.tools/.bin:$original"
        cd "$HOME"
        _binloom_hook
        test "$PATH" = "$original"
        test -z "${BINLOOM_ACTIVE_ROOT-}"
        case "$PROMPT_COMMAND" in *'printf preserved'*) ;; *) exit 9 ;; esac
    "#,
    ));
}

#[test]
fn hook_preserves_preexisting_path_and_prompt_array() {
    let fixture = Fixture::new();
    fixture.trust();
    success(fixture.bash(
        r#"
        set -e
        export PATH="$ROOT/.tools/.bin:$PATH"
        original=$PATH
        PROMPT_COMMAND=('printf one' 'printf two')
        eval "$("$BIN" shell init --shell bash)"
        eval "$("$BIN" shell init --shell bash)"
        test "${#PROMPT_COMMAND[@]}" = 3
        cd "$HOME"
        _binloom_hook
        test "$PATH" = "$original"
    "#,
    ));
}

#[test]
fn hook_hints_once_per_root_and_does_not_invoke_on_unchanged_pwd() {
    let fixture = Fixture::new();
    let output = fixture.bash(
        r#"
        set -e
        eval "$("$BIN" shell hook-env --shell bash --interactive)"
        eval "$("$BIN" shell hook-env --shell bash --interactive)"
        eval "$("$BIN" shell init --shell bash)"
        _BINLOOM_EXE=/does/not/exist
        _binloom_hook
    "#,
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .matches("binloom shell trust")
            .count(),
        1
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("/does/not/exist"));
    success(output);
}

#[test]
fn hook_rejects_symlink_escapes_and_untrusted_nested_projects() {
    let fixture = Fixture::new();
    fixture.trust();
    fs::write(fixture.root.join("nested/binloom.toml"), "").unwrap();
    assert!(
        !success(fixture.run(&["shell", "hook-env", "--shell", "bash"]))
            .contains("export BINLOOM_ACTIVE_ROOT=")
    );
    fs::remove_file(fixture.root.join("nested/binloom.toml")).unwrap();
    let bin = fixture.root.join(".tools/.bin");
    fs::remove_dir(&bin).unwrap();
    symlink(fixture.home.path(), &bin).unwrap();
    assert!(
        !success(fixture.run(&["shell", "hook-env", "--shell", "bash"]))
            .contains("export BINLOOM_ACTIVE_ROOT=")
    );
}

#[test]
fn trust_does_not_follow_config_symlinks() {
    let fixture = Fixture::new();
    let config = fixture.home.path().join(".config");
    fs::create_dir(&config).unwrap();
    let outside = fixture.home.path().join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, config.join("binloom")).unwrap();
    assert!(!fixture.run(&["shell", "trust"]).status.success());
    assert!(!Path::new(&outside).join("trusted").exists());
}

#[test]
fn hook_deactivates_if_its_executable_disappears() {
    let fixture = Fixture::new();
    fixture.trust();
    success(fixture.bash(
        r#"
        set -e
        original=$PATH
        eval "$("$BIN" shell init --shell bash)"
        test "$PATH" = "$ROOT/.tools/.bin:$original"
        _BINLOOM_EXE=/does/not/exist
        cd "$HOME"
        _binloom_hook
        test "$PATH" = "$original"
        test -z "${BINLOOM_ACTIVE_ROOT-}"
    "#,
    ));
}

#[test]
fn zsh_directory_hook_activates_and_deactivates() {
    let fixture = Fixture::new();
    if fixture.command("zsh").arg("--version").output().is_err() {
        eprintln!("zsh unavailable; native zsh execution skipped");
        return;
    }
    fixture.trust();
    success(
        fixture
            .command("zsh")
            .args([
                "-f",
                "-c",
                r#"
        set -e
        original=$PATH
        eval "$("$BIN" shell init --shell zsh)"
        eval "$("$BIN" shell init --shell zsh)"
        test "$PATH" = "$ROOT/.tools/.bin:$original"
        cd "$HOME"
        test "$PATH" = "$original"
        test -z "${BINLOOM_ACTIVE_ROOT-}"
    "#,
            ])
            .output()
            .unwrap(),
    );
}

#[test]
fn fallback_config_directory_cannot_redirect_writes_outside_home() {
    let fixture = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), fixture.home.path().join(".config")).unwrap();
    let output = fixture
        .command(env!("CARGO_BIN_EXE_binloom"))
        .env_remove("XDG_CONFIG_HOME")
        .args(["shell", "trust"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!outside.path().join("binloom/trusted").exists());
}
