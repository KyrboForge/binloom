use sha2::Digest;
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

fn project(root: &std::path::Path) {
    fs::write(root.join("binloom.toml"), "manifest-version = 1\n[binloom]\nversion = '1.0.0'\n[tools.tool]\nversion = '1.0.0'\nsource = 'github:owner/tool'\n").unwrap();
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let arch = std::env::consts::ARCH;
    let sha = "306c6ca7407560340797866e077e053627ad409277d1b9da58106fce4cf717cb";
    let mut lock = "lock-version = 1\n".to_string();
    for (section, name) in [("binloom", "binloom"), ("tools.tool", "tool")] {
        lock.push_str(&format!("[{section}]\nversion = '1.0.0'\nsource = 'github:owner/{name}'\ntag = 'v1.0.0'\n[{section}.artifacts.{os}-{arch}]\nasset = '{name}'\nurl = 'https://example.invalid/{name}'\nsha256 = '{sha}'\nformat = 'raw'\n"));
        let directory = root.join(format!(".tools/{name}/1.0.0"));
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join(name), "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(directory.join(name), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(directory.join(".artifact-sha256"), sha).unwrap();
        if name == "tool" {
            fs::write(directory.join(".executable-sha256"), sha).unwrap();
        }
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
fn install_removes_a_link_to_an_older_binloom_when_the_locked_one_is_missing() {
    let home = tempfile::tempdir().unwrap();
    project(home.path());
    fs::remove_file(home.path().join(".tools/binloom/1.0.0/binloom")).unwrap();
    let old = home.path().join(".tools/binloom/old/binloom");
    fs::create_dir_all(old.parent().unwrap()).unwrap();
    fs::write(&old, "#!/bin/sh\nexit 0\n").unwrap();
    fs::create_dir_all(home.path().join(".tools/.bin")).unwrap();
    let link = home.path().join(".tools/.bin/binloom");
    std::os::unix::fs::symlink("../binloom/old/binloom", &link).unwrap();
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
    assert!(String::from_utf8_lossy(&output.stderr).contains("./binloomw install"));
    assert!(link.symlink_metadata().is_err());
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

fn checksum(bytes: &[u8]) -> String {
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn install(root: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_binloom"))
        .arg("install")
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join(".config"))
        .current_dir(root)
        .output()
        .unwrap()
}

#[test]
fn install_skips_linking_a_tampered_binloom_despite_matching_stamps() {
    let home = tempfile::tempdir().unwrap();
    project(home.path());
    let directory = home.path().join(".tools/binloom/1.0.0");
    let tampered = "#!/bin/sh\nexit 1\n";
    fs::write(directory.join("binloom"), tampered).unwrap();
    // Stamps are rewritable by whoever tampered with the binary, so they prove nothing.
    fs::write(
        directory.join(".executable-sha256"),
        checksum(tampered.as_bytes()),
    )
    .unwrap();
    let output = install(home.path());
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("binloomw install"));
    assert!(!home.path().join(".tools/.bin/binloom").exists());
}

#[test]
fn install_links_a_gz_binloom_only_while_its_retained_archive_verifies() {
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;

    let home = tempfile::tempdir().unwrap();
    project(home.path());
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(b"#!/bin/sh\nexit 0\n").unwrap();
    let archive = encoder.finish().unwrap();
    let archive_sha = checksum(&archive);
    let raw_sha = "306c6ca7407560340797866e077e053627ad409277d1b9da58106fce4cf717cb";
    let lock_path = home.path().join("binloom.lock");
    let lock = fs::read_to_string(&lock_path)
        .unwrap()
        .replacen(raw_sha, &archive_sha, 1)
        .replacen("format = 'raw'", "format = 'gz'", 1);
    fs::write(&lock_path, lock).unwrap();
    let directory = home.path().join(".tools/binloom/1.0.0");
    fs::write(directory.join(".artifact"), &archive).unwrap();
    fs::write(directory.join(".artifact-sha256"), &archive_sha).unwrap();

    let output = install(home.path());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let link = home.path().join(".tools/.bin/binloom");
    assert!(link.symlink_metadata().is_ok());

    let mut corrupted = archive;
    corrupted[4] ^= 1;
    fs::write(directory.join(".artifact"), corrupted).unwrap();
    let output = install(home.path());
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("binloomw install"));
    assert!(link.symlink_metadata().is_err());
}

#[test]
fn install_refuses_a_symlinked_binloom_directory_without_touching_its_target() {
    let home = tempfile::tempdir().unwrap();
    project(home.path());
    let external = home.path().join("external");
    fs::rename(home.path().join(".tools/binloom"), &external).unwrap();
    let target = external.join("1.0.0/binloom");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    std::os::unix::fs::symlink(&external, home.path().join(".tools/binloom")).unwrap();

    let output = install(home.path());
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("symlink"));
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o644
    );
    assert!(!home.path().join(".tools/.bin/binloom").exists());
}

#[test]
fn install_skips_linking_a_group_writable_binloom() {
    let home = tempfile::tempdir().unwrap();
    project(home.path());
    let executable = home.path().join(".tools/binloom/1.0.0/binloom");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o775)).unwrap();
    let output = install(home.path());
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("binloomw install"));
    assert!(!home.path().join(".tools/.bin/binloom").exists());
}

#[test]
fn install_refuses_group_or_other_writable_cache_directories() {
    for relative in [".tools", ".tools/binloom", ".tools/binloom/1.0.0"] {
        for mode in [0o775, 0o757, 0o777] {
            let home = tempfile::tempdir().unwrap();
            project(home.path());
            let directory = home.path().join(relative);
            fs::set_permissions(&directory, fs::Permissions::from_mode(mode)).unwrap();
            let output = install(home.path());
            let message = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "{relative} {mode:o}: {message}");
            assert!(
                message.contains("chmod go-w") && message.contains(&*directory.to_string_lossy()),
                "{relative} {mode:o}: {message}"
            );
            assert!(!home.path().join(".tools/.bin/binloom").exists());
        }
    }
}

#[test]
fn install_creates_private_link_directory_under_a_group_writable_umask() {
    let home = tempfile::tempdir().unwrap();
    project(home.path());
    let output = Command::new("/bin/sh")
        .args(["-c", "umask 002; exec \"$0\" install"])
        .arg(env!("CARGO_BIN_EXE_binloom"))
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
    let bin = home.path().join(".tools/.bin");
    assert_eq!(
        fs::metadata(&bin).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert!(bin.join("binloom").symlink_metadata().is_ok());
}

#[test]
fn install_links_a_multi_member_gz_binloom() {
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;

    let home = tempfile::tempdir().unwrap();
    project(home.path());
    let program = b"#!/bin/sh\nexit 0\n";
    let mut archive = Vec::new();
    for part in program.chunks(7) {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(part).unwrap();
        archive.extend(encoder.finish().unwrap());
    }
    let archive_sha = checksum(&archive);
    let raw_sha = "306c6ca7407560340797866e077e053627ad409277d1b9da58106fce4cf717cb";
    let lock_path = home.path().join("binloom.lock");
    let lock = fs::read_to_string(&lock_path)
        .unwrap()
        .replacen(raw_sha, &archive_sha, 1)
        .replacen("format = 'raw'", "format = 'gz'", 1);
    fs::write(&lock_path, lock).unwrap();
    let directory = home.path().join(".tools/binloom/1.0.0");
    fs::write(directory.join("binloom"), program).unwrap();
    fs::write(directory.join(".artifact"), &archive).unwrap();
    fs::write(directory.join(".artifact-sha256"), &archive_sha).unwrap();

    let output = install(home.path());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        home.path()
            .join(".tools/.bin/binloom")
            .symlink_metadata()
            .is_ok()
    );
}
