use std::{fs, os::unix::fs::PermissionsExt, process::Command};

fn project(root: &std::path::Path) {
    fs::write(root.join("binloom.toml"), "manifest-version = 1\n[binloom]\nversion = '1.0.0'\n[tools.tool]\nversion = '1.0.0'\nsource = 'github:owner/tool'\n").unwrap();
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let arch = std::env::consts::ARCH;
    let sha = "0".repeat(64);
    let mut lock = "lock-version = 1\n".to_string();
    for (section, name) in [("binloom", "binloom"), ("tools.tool", "tool")] {
        lock.push_str(&format!("[{section}]\nversion = '1.0.0'\nsource = 'github:owner/{name}'\ntag = 'v1.0.0'\n[{section}.artifacts.{os}-{arch}]\nasset = '{name}'\nurl = 'https://example.invalid/{name}'\nsha256 = '{sha}'\nformat = 'raw'\n"));
        let directory = root.join(format!(".tools/{name}/1.0.0"));
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join(name), "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(directory.join(name), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(directory.join(".artifact-sha256"), &sha).unwrap();
    }
    fs::write(root.join("binloom.lock"), lock).unwrap();
}

#[test]
fn install_links_and_repairs_locked_binloom_without_non_tty_profile_changes() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("project");
    fs::create_dir(&root).unwrap();
    project(&root);
    for flags in [vec![], vec!["--no-prompt"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_binloom"))
            .arg("install")
            .args(flags)
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path().join(".config"))
            .env("SHELL", "/bin/bash")
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("Enable automatic"));
        assert!(!home.path().join(".bashrc").exists());
        assert!(!home.path().join(".config").exists());
        let link = root.join(".tools/.bin/binloom");
        assert_eq!(
            fs::read_link(&link).unwrap(),
            std::path::Path::new("../binloom/1.0.0/binloom")
        );
        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink("../binloom/old/binloom", &link).unwrap();
    }
}

#[test]
fn install_does_not_link_a_missing_locked_binary() {
    let home = tempfile::tempdir().unwrap();
    project(home.path());
    fs::remove_file(home.path().join(".tools/binloom/1.0.0/binloom")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_binloom"))
        .arg("install")
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .current_dir(home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!home.path().join(".tools/.bin/binloom").exists());
}

#[test]
fn install_skips_linking_binloom_whose_stamp_does_not_match_the_lock() {
    let home = tempfile::tempdir().unwrap();
    project(home.path());
    fs::write(
        home.path().join(".tools/binloom/1.0.0/.artifact-sha256"),
        "1".repeat(64),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_binloom"))
        .arg("install")
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .current_dir(home.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("binloomw install"));
    assert!(!home.path().join(".tools/.bin/binloom").exists());
}

#[test]
fn install_rejects_reserved_binloom_tool_without_overwriting_it() {
    for name in ["binloom", "Binloom"] {
        install_rejects_reserved_tool(name);
    }
}

fn install_rejects_reserved_tool(name: &str) {
    let home = tempfile::tempdir().unwrap();
    project(home.path());
    for file in ["binloom.toml", "binloom.lock"] {
        let path = home.path().join(file);
        let contents = fs::read_to_string(&path)
            .unwrap()
            .replace("tools.tool", &format!("tools.{name}"));
        fs::write(path, contents).unwrap();
    }
    fs::create_dir_all(home.path().join(".tools/.bin")).unwrap();
    let link = home.path().join(".tools/.bin/binloom");
    fs::write(&link, "keep existing command").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_binloom"))
        .arg("install")
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .current_dir(home.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("reserved"));
    assert_eq!(fs::read_to_string(link).unwrap(), "keep existing command");
}

#[test]
fn install_links_binloom_in_a_project_without_other_tools() {
    let home = tempfile::tempdir().unwrap();
    project(home.path());
    for file in ["binloom.toml", "binloom.lock"] {
        let path = home.path().join(file);
        let content = fs::read_to_string(&path).unwrap();
        fs::write(path, content.split("[tools.tool]").next().unwrap()).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_binloom"))
        .arg("install")
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .current_dir(home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_link(home.path().join(".tools/.bin/binloom")).unwrap(),
        std::path::Path::new("../binloom/1.0.0/binloom")
    );
}

#[test]
fn stale_activated_binloom_requires_wrapper() {
    let home = tempfile::tempdir().unwrap();
    project(home.path());
    fs::remove_file(home.path().join(".tools/binloom/1.0.0/binloom")).unwrap();
    let old = home.path().join(".tools/binloom/old/binloom");
    fs::create_dir_all(old.parent().unwrap()).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_binloom"), &old).unwrap();
    fs::create_dir_all(home.path().join(".tools/.bin")).unwrap();
    let link = home.path().join(".tools/.bin/binloom");
    std::os::unix::fs::symlink("../binloom/old/binloom", &link).unwrap();
    let output = Command::new(&link)
        .arg("install")
        .current_dir(home.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("./binloomw install"));
}
