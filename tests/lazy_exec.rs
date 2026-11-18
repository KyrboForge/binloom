#![cfg(unix)]

use flate2::{Compression, write::GzEncoder};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const TOOL: &str = "lazy-test-tool";
const SCRIPT: &[u8] = b"#!/bin/sh\nprintf 'pinned tool\\n'\n";

struct Server {
    url: String,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<usize>>,
}

impl Server {
    fn start(body: &[u8], delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let body = body.to_vec();
        let worker = thread::spawn(move || {
            let mut requests = 0;
            while !stopped.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = BufReader::new(&stream);
                let mut line = String::new();
                assert!(request.read_line(&mut line).unwrap() > 0);
                assert!(line.starts_with("GET "), "{line}");
                loop {
                    line.clear();
                    assert!(request.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                }
                requests += 1;
                thread::sleep(delay);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
            requests
        });
        Self {
            url: format!("http://{address}/{TOOL}"),
            stop,
            worker: Some(worker),
        }
    }

    fn shutdown(mut self) -> usize {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            self.stop.store(true, Ordering::Relaxed);
            let _ = worker.join();
        }
    }
}

fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn checksum(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn tar_gz(script: &[u8]) -> Vec<u8> {
    let mut archive = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    let mut header = tar::Header::new_gnu();
    header.set_mode(0o755);
    header.set_size(script.len() as u64);
    archive
        .append_data(&mut header, format!("release/{TOOL}"), script)
        .unwrap();
    archive.into_inner().unwrap().finish().unwrap()
}

fn write_project(root: &Path, names: &[&str], url: &str, format: &str, sha: &str, target: &str) {
    let mut manifest = "manifest-version = 1\n[binloom]\nversion = '1.0.0'\n".to_owned();
    let mut lock = "lock-version = 1\n".to_owned();
    for name in names {
        manifest.push_str(&format!(
            "[tools.{name}]\nversion = '1.0.0'\nsource = 'github:owner/{name}'\n"
        ));
        lock.push_str(&format!(
            "[tools.{name}]\nversion = '1.0.0'\nsource = 'github:owner/{name}'\ntag = 'v1.0.0'\n\
             [tools.{name}.artifacts.{target}]\nasset = '{name}'\nurl = '{url}'\nsha256 = '{sha}'\nformat = '{format}'\n"
        ));
    }
    fs::write(root.join("binloom.toml"), manifest).unwrap();
    fs::write(root.join("binloom.lock"), lock).unwrap();
}

fn raw_project(root: &Path, server: &Server) {
    write_project(
        root,
        &[TOOL],
        &server.url,
        "raw",
        &checksum(SCRIPT),
        &platform(),
    );
}

fn cli(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_binloom"));
    command
        .current_dir(root)
        .env("NO_PROXY", "127.0.0.1")
        .env("no_proxy", "127.0.0.1");
    command
}

fn exec_tool(root: &Path) -> Output {
    cli(root).args(["exec", TOOL]).output().unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_ran_tool(output: Output) {
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(output.stdout, b"pinned tool\n");
}

fn version_dir(root: &Path) -> PathBuf {
    root.join(".tools").join(TOOL).join("1.0.0")
}

fn link(root: &Path) -> PathBuf {
    root.join(".tools/.bin").join(TOOL)
}

fn assert_no_partial_install(root: &Path) {
    assert!(link(root).symlink_metadata().is_err());
    let files: Vec<_> = fs::read_dir(version_dir(root))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(files, [".install.lock"]);
}

#[test]
fn installs_only_requested_tool_and_reuses_extracted_binary_offline() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    let artifact = tar_gz(SCRIPT);
    let server = Server::start(&artifact, Duration::ZERO);
    let artifact_hash = checksum(&artifact);
    write_project(
        root,
        &[TOOL, "other-tool"],
        &server.url,
        "tar-gz",
        &artifact_hash,
        &platform(),
    );

    let output = exec_tool(root);
    assert!(stderr(&output).contains(&format!("Installed {TOOL} 1.0.0")));
    assert_ran_tool(output);
    assert_eq!(fs::read(link(root)).unwrap(), SCRIPT);
    assert_eq!(
        fs::read_to_string(version_dir(root).join(".artifact-sha256"))
            .unwrap()
            .trim(),
        artifact_hash
    );
    assert_eq!(
        fs::read_to_string(version_dir(root).join(".executable-sha256"))
            .unwrap()
            .trim(),
        checksum(SCRIPT)
    );
    assert!(!root.join(".tools/other-tool").exists());
    assert!(!root.join(".tools/.bin/other-tool").exists());

    assert_ran_tool(exec_tool(root));
    assert_eq!(server.shutdown(), 1);
    assert_ran_tool(exec_tool(root));
}

#[test]
fn missing_or_inconsistent_project_metadata_blocks_a_declared_tool() {
    let server = Server::start(SCRIPT, Duration::ZERO);
    for (file, replacement, diagnostic) in [
        ("binloom.toml", None, "restore binloom.toml"),
        ("binloom.toml", Some("invalid TOML"), "binloom.toml"),
        ("binloom.lock", None, "binloom.lock"),
        ("binloom.lock", Some("invalid TOML"), "binloom.lock"),
        ("binloom.lock", Some("lock-version = 1\n"), "out of sync"),
    ] {
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        raw_project(root, &server);
        let path = root.join(file);
        if let Some(contents) = replacement {
            fs::write(&path, contents).unwrap();
        } else {
            fs::remove_file(&path).unwrap();
        }
        let fallback = tempfile::tempdir().unwrap();
        let impostor = fallback.path().join(TOOL);
        fs::write(&impostor, b"#!/bin/sh\necho 'wrong tool'\n").unwrap();
        fs::set_permissions(&impostor, fs::Permissions::from_mode(0o755)).unwrap();

        let output = cli(root)
            .env("PATH", fallback.path())
            .args(["exec", TOOL])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = stderr(&output);
        assert!(
            error.contains(diagnostic) && error.contains("binloom update"),
            "{error}"
        );
        assert!(!root.join(".tools").exists());
        if let Some(contents) = replacement {
            assert_eq!(fs::read_to_string(&path).unwrap(), contents);
        } else {
            assert!(!path.exists());
        }
    }
    assert_eq!(server.shutdown(), 0);
}

#[test]
fn invalid_manifest_entry_without_a_locked_tool_blocks_path_fallback() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    fs::write(
        root.join("binloom.toml"),
        format!("manifest-version = 1\n[binloom]\nversion = '1.0.0'\n[tools.{TOOL}]\nversion = 'v1.0.0'\nsource = 'github:owner/tool'\n"),
    )
    .unwrap();
    let impostor = root.join(TOOL);
    fs::write(&impostor, b"#!/bin/sh\necho impostor\n").unwrap();
    fs::set_permissions(impostor, fs::Permissions::from_mode(0o755)).unwrap();
    for (lock, diagnostic) in [
        (None, "must be written as 1.0.0"),
        (Some("invalid TOML"), "binloom.lock"),
        (Some("lock-version = 1\n"), "must be written as 1.0.0"),
    ] {
        if let Some(lock) = lock {
            fs::write(root.join("binloom.lock"), lock).unwrap();
        }
        let output = cli(root)
            .env("PATH", root)
            .args(["exec", TOOL])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(stderr(&output).contains(diagnostic), "{}", stderr(&output));
    }
}

#[test]
fn unreadable_tool_tables_block_path_fallback() {
    for (manifest, lock, broken_file) in [
        (b"[".as_slice(), None, "binloom.toml"),
        (b"[", Some("["), "binloom.toml"),
        (b"\xff", None, "binloom.toml"),
        (b"tools = 1", None, "binloom.toml"),
        (b"[tools]", Some("tools = 1"), "binloom.lock"),
    ] {
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        fs::write(root.join("binloom.toml"), manifest).unwrap();
        if let Some(lock) = lock {
            fs::write(root.join("binloom.lock"), lock).unwrap();
        }
        fs::copy("/bin/echo", root.join(TOOL)).unwrap();
        let output = cli(root)
            .env("PATH", root)
            .args(["exec", TOOL, "unexpected fallback"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = stderr(&output);
        assert!(
            error.contains(broken_file) && error.contains("binloom update"),
            "{error}"
        );
    }
}

#[test]
fn case_variant_of_managed_tool_reports_canonical_command() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    let server = Server::start(SCRIPT, Duration::ZERO);
    raw_project(root, &server);
    let variant = TOOL.to_ascii_uppercase();
    let bin = root.join(".tools/.bin");
    fs::create_dir_all(&bin).unwrap();
    let impostor = bin.join(&variant);
    fs::write(&impostor, b"#!/bin/sh\necho impostor\n").unwrap();
    fs::set_permissions(impostor, fs::Permissions::from_mode(0o755)).unwrap();
    let output = cli(root).args(["exec", &variant]).output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = stderr(&output);
    assert!(
        error.contains(&variant) && error.contains(&format!("binloomw exec {TOOL}")),
        "{error}"
    );
    assert_eq!(server.shutdown(), 0);
}

#[test]
fn fifo_install_lock_fails_promptly() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    let server = Server::start(SCRIPT, Duration::ZERO);
    raw_project(root, &server);
    fs::create_dir_all(version_dir(root)).unwrap();
    let lock = version_dir(root).join(".install.lock");
    assert!(
        Command::new("mkfifo")
            .arg(&lock)
            .status()
            .unwrap()
            .success()
    );
    let mut child = cli(root)
        .args(["exec", TOOL])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("exec blocked opening a FIFO at .install.lock");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains(".install.lock"),
        "{}",
        stderr(&output)
    );
    assert_eq!(server.shutdown(), 0);
}

#[test]
fn raw_executable_with_rewritten_stamp_must_match_locked_digest() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    let server = Server::start(SCRIPT, Duration::ZERO);
    raw_project(root, &server);
    assert_ran_tool(exec_tool(root));
    let tamper = || {
        let bytes = b"#!/bin/sh\necho impostor\n";
        fs::write(version_dir(root).join(TOOL), bytes).unwrap();
        fs::write(
            version_dir(root).join(".executable-sha256"),
            checksum(bytes),
        )
        .unwrap();
    };
    tamper();
    assert_ran_tool(exec_tool(root));
    assert_eq!(server.shutdown(), 2);
    tamper();
    let output = exec_tool(root);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        stderr(&output).contains("failed to install tool"),
        "{}",
        stderr(&output)
    );
}

fn assert_archive_cache_repairs(damage: impl Fn(&Path)) {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    let artifact = tar_gz(SCRIPT);
    let server = Server::start(&artifact, Duration::ZERO);
    write_project(
        root,
        &[TOOL],
        &server.url,
        "tar-gz",
        &checksum(&artifact),
        &platform(),
    );
    assert_ran_tool(exec_tool(root));
    let directory = version_dir(root);
    let retained = directory.join(".artifact");
    assert_eq!(fs::read(&retained).unwrap(), artifact);

    damage(&directory);
    assert_ran_tool(exec_tool(root));
    assert_eq!(fs::read(&retained).unwrap(), artifact);
    assert_eq!(fs::read(directory.join(TOOL)).unwrap(), SCRIPT);
    assert_eq!(server.shutdown(), 2);

    damage(&directory);
    let output = exec_tool(root);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        stderr(&output).contains("failed to install tool"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn archive_executable_with_rewritten_stamp_must_match_locked_archive() {
    assert_archive_cache_repairs(|directory| {
        let bytes = [SCRIPT, b"\n"].concat();
        fs::write(directory.join(TOOL), &bytes).unwrap();
        fs::write(directory.join(".executable-sha256"), checksum(&bytes)).unwrap();
    });
}

#[test]
fn retained_archive_must_match_locked_digest() {
    assert_archive_cache_repairs(|directory| {
        let path = directory.join(".artifact");
        let mut archive = fs::read(&path).unwrap();
        archive[4] ^= 1; // Change the gzip timestamp, preserving the extracted executable.
        fs::write(path, archive).unwrap();
    });
}

#[test]
fn missing_retained_archive_requires_download() {
    assert_archive_cache_repairs(|directory| fs::remove_file(directory.join(".artifact")).unwrap());
}

#[test]
fn rejects_bad_downloads_without_partial_install_or_broken_link() {
    for (format, sha, diagnostic) in [
        ("raw", "0".repeat(64), "checksum mismatch"),
        ("tar-gz", checksum(SCRIPT), "failed to unpack"),
    ] {
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        let server = Server::start(SCRIPT, Duration::ZERO);
        write_project(root, &[TOOL], &server.url, format, &sha, &platform());

        let output = exec_tool(root);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(stderr(&output).contains(diagnostic), "{}", stderr(&output));
        assert_no_partial_install(root);
        assert_eq!(server.shutdown(), 1);
    }
}

#[test]
fn rejects_symlinked_install_paths_without_writing_outside_the_project() {
    let server = Server::start(SCRIPT, Duration::ZERO);
    for relative in [
        ".tools".to_owned(),
        format!(".tools/{TOOL}"),
        format!(".tools/{TOOL}/1.0.0"),
        format!(".tools/{TOOL}/1.0.0/.install.lock"),
        ".tools/.bin".to_owned(),
    ] {
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = project.path();
        raw_project(root, &server);
        let path = root.join(&relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let target = if path.ends_with(".install.lock") {
            outside.path().join("lock")
        } else {
            outside.path().to_path_buf()
        };
        symlink(target, path).unwrap();

        let output = exec_tool(root);
        let error = stderr(&output);
        assert!(!output.status.success(), "{relative}");
        assert!(
            error.contains("symlink") && error.contains(&relative),
            "{error}"
        );
        assert!(
            fs::read_dir(outside.path()).unwrap().next().is_none(),
            "{relative}"
        );
    }
    assert_eq!(server.shutdown(), 1);
}

#[test]
fn reinstalls_a_cached_executable_without_execute_permissions() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    let server = Server::start(SCRIPT, Duration::ZERO);
    raw_project(root, &server);
    assert_ran_tool(exec_tool(root));

    let installed = version_dir(root).join(TOOL);
    fs::set_permissions(&installed, fs::Permissions::from_mode(0o644)).unwrap();
    assert_ran_tool(exec_tool(root));
    assert_ne!(
        fs::metadata(&installed).unwrap().permissions().mode() & 0o111,
        0
    );
    assert_eq!(server.shutdown(), 2);
}

#[test]
fn repairs_tampered_legacy_and_symlinked_executables_and_fails_closed_offline() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    let server = Server::start(SCRIPT, Duration::ZERO);
    raw_project(root, &server);
    assert_ran_tool(exec_tool(root));

    let installed = version_dir(root).join(TOOL);
    let tampered = b"#!/bin/sh\necho 'unverified tool'\n";
    fs::write(&installed, tampered).unwrap();
    assert_ran_tool(exec_tool(root));
    assert_eq!(fs::read(&installed).unwrap(), SCRIPT);

    fs::remove_file(version_dir(root).join(".executable-sha256")).unwrap();
    assert_ran_tool(exec_tool(root));

    let external = root.join("same-bytes");
    fs::write(&external, SCRIPT).unwrap();
    fs::set_permissions(&external, fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_file(&installed).unwrap();
    symlink(&external, &installed).unwrap();
    assert_ran_tool(exec_tool(root));
    assert!(installed.symlink_metadata().unwrap().is_file());
    assert_eq!(server.shutdown(), 4);

    fs::write(&installed, tampered).unwrap();
    let output = exec_tool(root);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = stderr(&output);
    assert!(
        error.contains("failed to download")
            && error.contains(&format!(".tools/{TOOL}/1.0.0/{TOOL}")),
        "{error}"
    );
}

#[test]
fn failed_refresh_keeps_the_previously_verified_installation() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    let server = Server::start(SCRIPT, Duration::ZERO);
    raw_project(root, &server);
    assert_ran_tool(exec_tool(root));

    let url = server.url.clone();
    assert_eq!(server.shutdown(), 1);
    let lock = fs::read(root.join("binloom.lock")).unwrap();
    let version_dir = version_dir(root);
    let installed = |name: &str| fs::read(version_dir.join(name)).unwrap();
    let artifact_stamp = installed(".artifact-sha256");
    let executable_stamp = installed(".executable-sha256");
    let target = fs::read_link(link(root)).unwrap();
    write_project(root, &[TOOL], &url, "raw", &"0".repeat(64), &platform());

    let output = exec_tool(root);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(installed(TOOL), SCRIPT);
    assert_eq!(installed(".artifact-sha256"), artifact_stamp);
    assert_eq!(installed(".executable-sha256"), executable_stamp);
    assert_eq!(fs::read_link(link(root)).unwrap(), target);
    assert_eq!(fs::read_dir(version_dir).unwrap().count(), 4);
    fs::write(root.join("binloom.lock"), lock).unwrap();
    assert_ran_tool(exec_tool(root));
}

#[test]
fn rejects_a_tool_missing_the_current_platform_artifact() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    let server = Server::start(SCRIPT, Duration::ZERO);
    let other_platform = if cfg!(target_os = "macos") {
        "linux-x86_64"
    } else {
        "macos-aarch64"
    };
    write_project(
        root,
        &[TOOL],
        &server.url,
        "raw",
        &checksum(SCRIPT),
        other_platform,
    );

    let output = exec_tool(root);
    assert!(!output.status.success());
    let error = stderr(&output);
    assert!(
        error.contains(TOOL) && error.contains(&platform()),
        "{error}"
    );
    assert!(!root.join(".tools").exists());
    assert_eq!(server.shutdown(), 0);
}

#[test]
fn concurrent_first_execs_and_install_share_one_download() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    let server = Server::start(SCRIPT, Duration::from_millis(300));
    raw_project(root, &server);
    let spawn = |arguments: &[&str]| {
        cli(root)
            .args(arguments)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let children: Vec<_> = (0..6).map(|_| spawn(&["exec", TOOL])).collect();
    let install = spawn(&["install", "--no-prompt"]);
    for child in children {
        assert_ran_tool(child.wait_with_output().unwrap());
    }
    let output = install.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(String::from_utf8_lossy(&output.stdout).contains(TOOL));
    assert_eq!(server.shutdown(), 1);
    assert_eq!(fs::read(link(root)).unwrap(), SCRIPT);
    assert_ran_tool(exec_tool(root));
}

#[test]
fn forwards_arguments_cwd_path_and_exit_code_from_a_project_with_special_characters() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().join("project space & [x] 'quoted'");
    let nested = root.join("nested path");
    fs::create_dir_all(&nested).unwrap();
    let script =
        b"#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"${PATH%%:*}\" \"$0\"\nprintf '<%s>\\n' \"$@\"\nexit 42\n";
    let server = Server::start(script, Duration::ZERO);
    write_project(
        &root,
        &[TOOL],
        &server.url,
        "raw",
        &checksum(script),
        &platform(),
    );

    let output = cli(&nested)
        .args(["exec", TOOL, "a b", "q'\"x", "*?[a]", "", "--flag"])
        .output()
        .unwrap();
    let expected = format!(
        "{}\n{}\n{}\n<a b>\n<q'\"x>\n<*?[a]>\n<>\n<--flag>\n",
        nested.canonicalize().unwrap().display(),
        root.canonicalize().unwrap().join(".tools/.bin").display(),
        version_dir(&root.canonicalize().unwrap())
            .join(TOOL)
            .display(),
    );
    assert_eq!(output.status.code(), Some(42), "{}", stderr(&output));
    assert_eq!(output.stdout, expected.as_bytes());
    assert_eq!(server.shutdown(), 1);
}

#[test]
fn unmanaged_commands_use_path_without_installing_any_declared_tool() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    let server = Server::start(SCRIPT, Duration::ZERO);
    raw_project(root, &server);

    let output = cli(root)
        .args(["exec", "sh", "-c", "printf unmanaged"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(output.stdout, b"unmanaged");
    assert!(!root.join(".tools").exists());
    assert_eq!(server.shutdown(), 0);
}

#[test]
fn lock_only_ancestor_does_not_enable_unmanaged_exec() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    let nested = root.join("nested");
    fs::create_dir(&nested).unwrap();
    let server = Server::start(SCRIPT, Duration::ZERO);
    raw_project(root, &server);
    fs::remove_file(root.join("binloom.toml")).unwrap();

    for malformed in [false, true] {
        let program = if malformed {
            fs::write(root.join("binloom.lock"), "invalid TOML").unwrap();
            TOOL
        } else {
            "sh"
        };
        let output = cli(&nested)
            .args(["exec", program, "-c", "exit 0"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            stderr(&output).contains(if malformed {
                "binloom.lock"
            } else {
                "binloom.toml not found"
            }),
            "{}",
            stderr(&output)
        );
        assert!(!root.join(".tools").exists());
    }
    assert_eq!(server.shutdown(), 0);
}

#[test]
fn reserved_tool_names_are_rejected_by_lazy_exec() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    let server = Server::start(SCRIPT, Duration::ZERO);
    for name in ["binloom", "Binloom"] {
        write_project(
            root,
            &[name],
            &server.url,
            "raw",
            &checksum(SCRIPT),
            &platform(),
        );
        let output = cli(root).args(["exec", name]).output().unwrap();
        assert!(!output.status.success());
        assert!(stderr(&output).contains("reserved"), "{}", stderr(&output));
    }
    assert!(!root.join(".tools").exists());
    assert_eq!(server.shutdown(), 0);
}

#[test]
fn cargo_cache_hit_verifies_executable_without_needing_cargo() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    let sha = "0".repeat(64);
    fs::write(
        root.join("binloom.toml"),
        format!("manifest-version = 1\n[binloom]\nversion = '1.0.0'\n[tools.{TOOL}]\nversion = '1.0.0'\nsource = 'cargo:{TOOL}'\n"),
    ).unwrap();
    fs::write(
        root.join("binloom.lock"),
        format!("lock-version = 1\n[tools.{TOOL}]\nversion = '1.0.0'\nsource = 'cargo:{TOOL}'\nsha256 = '{sha}'\n"),
    ).unwrap();
    let directory = version_dir(root);
    fs::create_dir_all(&directory).unwrap();
    let destination = directory.join(TOOL);
    fs::write(&destination, SCRIPT).unwrap();
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(directory.join(".crate-sha256"), sha).unwrap();
    fs::write(directory.join(".executable-sha256"), checksum(SCRIPT)).unwrap();
    let run_offline = || {
        cli(root)
            .env("PATH", "")
            .args(["exec", TOOL])
            .output()
            .unwrap()
    };

    assert_ran_tool(run_offline());
    fs::write(&destination, b"#!/bin/sh\necho unverified\n").unwrap();
    let output = run_offline();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        stderr(&output).contains("require cargo"),
        "{}",
        stderr(&output)
    );
}
