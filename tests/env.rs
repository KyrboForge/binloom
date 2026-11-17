use std::{fs, process::Command};

#[test]
fn env_quotes_nested_projects_and_is_idempotent() {
    let home = tempfile::tempdir().unwrap();
    let project = home
        .path()
        .join("project with ' quotes $dollar `ticks` \\ slash");
    let nested = project.join("nested");
    fs::create_dir_all(&nested).unwrap();
    fs::write(project.join("binloom.toml"), "").unwrap();
    let bin = project.canonicalize().unwrap().join(".tools/.bin");
    for shell in ["bash", "zsh", "fish"] {
        let output = Command::new(env!("CARGO_BIN_EXE_binloom"))
            .args(["env", "--shell", shell])
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path().join(".config"))
            .current_dir(&nested)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let snippet = String::from_utf8(output.stdout).unwrap();
        if shell == "fish" {
            assert!(snippet.contains("set -gx PATH"));
            assert!(snippet.contains("\\'"));
            continue;
        }
        // The Bash/Zsh env snippet is POSIX; run both with sh, even on CI without Zsh.
        let script = format!("{snippet}\n{snippet}\nprintf '%s' \"$PATH\"");
        let result = Command::new("sh")
            .args(["-c", &script])
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path().join(".config"))
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();
        assert!(result.status.success());
        assert_eq!(
            String::from_utf8(result.stdout).unwrap(),
            format!("{}:/usr/bin:/bin", bin.display())
        );
    }
}

#[test]
fn env_errors_outside_a_project() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_binloom"))
        .args(["env", "--shell", "bash"])
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .current_dir(home.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("binloom.toml not found"));
    assert!(output.stdout.is_empty());
}
