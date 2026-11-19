#![cfg(unix)]

mod common;

use common::{
    Artifact, TestServer, artifact_section, assert_ran, assert_refused, binloom_lock, checksum,
    gzip, isolated_path, leftover_temp_files, script, stderr, stdout, wrapper_command,
    wrapper_source, write_executable,
};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

const TOOL: &str = "wrapper-test-tool";
const OTHER_TOOL: &str = "other-tool";
const TOOL_SCRIPT: &[u8] = b"#!/bin/sh\nprintf '<%s>\\n' \"$@\"\nexit 42\n";

#[test]
fn wrapper_bootstraps_binloom_and_installs_only_the_executed_tool() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("fixture project");
    fs::create_dir(&root).unwrap();
    isolated_path(temp.path());

    let version = env!("CARGO_PKG_VERSION");
    let binloom = fs::read(env!("CARGO_BIN_EXE_binloom")).unwrap();
    write_executable(&root.join("binloomw"), wrapper_source(version));

    let server = TestServer::new();
    let address = server.address();
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let binloom_sha = checksum(&binloom);
    let tool_sha = checksum(TOOL_SCRIPT);
    let mut manifest = format!("manifest-version = 1\n[binloom]\nversion = \"{version}\"\n");
    let mut lock = format!(
        r#"lock-version = 1
[binloom]
version = "{version}"
source = "github:KyrboForge/binloom"
tag = "v{version}"
[binloom.artifacts.{platform}]
asset = "binloom"
url = "http://{address}/binloom"
sha256 = "{binloom_sha}"
format = "raw"
"#
    );
    for name in [TOOL, OTHER_TOOL] {
        let tool =
            format!("[tools.{name}]\nversion = \"1.0.0\"\nsource = \"github:owner/{name}\"\n");
        manifest.push_str(&tool);
        lock.push_str(&tool);
        lock.push_str(&format!(
            r#"tag = "v1.0.0"
[tools.{name}.artifacts.{platform}]
asset = "{name}"
url = "http://{address}/{name}"
sha256 = "{tool_sha}"
format = "raw"
"#
        ));
    }
    fs::write(root.join("binloom.toml"), manifest).unwrap();
    fs::write(root.join("binloom.lock"), lock).unwrap();
    assert!(!root.join(".tools").exists());

    let tool_route = format!("/{TOOL}");
    let other_route = format!("/{OTHER_TOOL}");
    let artifacts = [
        ("/binloom", binloom.as_slice().into()),
        (tool_route.as_str(), TOOL_SCRIPT.into()),
        (other_route.as_str(), TOOL_SCRIPT.into()),
    ];
    let (output, requests) = server.serving(&artifacts, || {
        wrapper_command(&root, temp.path(), None)
            .env("XDG_CACHE_HOME", temp.path().join("cache"))
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .args(["exec", TOOL, "two words", "--flag", "", "*?[a]"])
            .output()
            .unwrap()
    });

    assert_eq!(
        output.status.code(),
        Some(42),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"<two words>\n<--flag>\n<>\n<*?[a]>\n");
    assert_eq!(requests, ["/binloom", &tool_route]);
    assert!(
        root.join(".tools/binloom")
            .join(version)
            .join("binloom")
            .is_file()
    );
    assert!(
        root.join(".tools")
            .join(TOOL)
            .join("1.0.0")
            .join(TOOL)
            .is_file()
    );
    assert!(!root.join(".tools").join(OTHER_TOOL).exists());
}

impl Artifact {
    /// A gz artifact made of two concatenated members, as `gzip -dc` and `cat a.gz b.gz` produce.
    fn multi_member(tag: &str) -> Self {
        let program = script(tag);
        let (head, tail) = program.split_at(program.len() / 2);
        let mut body = gzip(head);
        body.extend(gzip(tail));
        Self {
            tag: tag.to_owned(),
            body,
            program,
            format: "gz",
        }
    }
}

const FORMATS: [&str; 2] = ["raw", "gz"];

struct Fixture {
    temp: tempfile::TempDir,
    root: PathBuf,
    server: TestServer,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        fs::create_dir(&root).unwrap();
        isolated_path(temp.path());
        write_executable(
            &root.join("binloomw"),
            wrapper_source(env!("CARGO_PKG_VERSION")),
        );
        Self {
            temp,
            root,
            server: TestServer::new(),
        }
    }

    fn lock(&self, version: &str, artifact: &Artifact) {
        self.lock_with_sha(version, artifact, &checksum(&artifact.body));
    }

    fn lock_with_sha(&self, version: &str, artifact: &Artifact, sha: &str) {
        let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
        let mut lock = binloom_lock(version);
        lock.push_str(&artifact_section(
            &format!("binloom.artifacts.{platform}"),
            &format!("http://{}/binloom", self.server.address()),
            sha,
            artifact.format,
        ));
        fs::write(self.root.join("binloom.lock"), lock).unwrap();
    }

    fn dir(&self, version: &str) -> PathBuf {
        self.root.join(".tools/binloom").join(version)
    }

    fn command(&self) -> Command {
        wrapper_command(&self.root, self.temp.path(), None)
    }

    /// A missing body makes the server return 404 for the Binloom route.
    fn serving<R>(&self, body: &[u8], f: impl FnOnce() -> R) -> (R, Vec<String>) {
        if body.is_empty() {
            self.server.serving(&[], f)
        } else {
            self.server.serving(&[("/binloom", body.into())], f)
        }
    }

    fn run(&self, body: &[u8]) -> (Output, Vec<String>) {
        self.serving(body, || self.command().output().unwrap())
    }

    /// The server answers 404 to everything, which behaves like having no network.
    fn run_offline(&self) -> (Output, Vec<String>) {
        self.run(b"")
    }

    fn install(&self, version: &str, artifact: &Artifact) {
        self.lock(version, artifact);
        let (output, requests) = self.run(&artifact.body);
        assert_ran(&output, &artifact.tag);
        assert_eq!(requests, ["/binloom"]);
    }
}

/// Replaces the cached executable with an attacker script, keeping or rewriting stamps.
fn tamper(dir: &Path, stamps: &[(&str, String)]) {
    let evil = script("evil");
    fs::write(dir.join("binloom"), &evil).unwrap();
    fs::set_permissions(dir.join("binloom"), fs::Permissions::from_mode(0o755)).unwrap();
    for (name, value) in stamps {
        fs::write(dir.join(name), format!("{value}\n")).unwrap();
    }
}

#[test]
fn tampered_cache_is_never_executed_and_is_repaired_online() {
    let evil_sha = checksum(&script("evil"));
    let variants: [(&str, Vec<(&str, String)>); 3] = [
        ("original markers", vec![]),
        (
            "rewritten executable marker",
            vec![(".executable-sha256", evil_sha.clone())],
        ),
        (
            "both markers rewritten",
            vec![
                (".executable-sha256", evil_sha.clone()),
                (".artifact-sha256", evil_sha),
            ],
        ),
    ];
    for format in FORMATS {
        for (name, stamps) in &variants {
            let fx = Fixture::new();
            let artifact = Artifact::new("good", format);
            fx.install("1.0.0", &artifact);
            let dir = fx.dir("1.0.0");
            tamper(&dir, stamps);

            let (output, requests) = fx.run_offline();
            assert_refused(&output, "evil");
            assert!(stderr(&output).contains("unverified"), "{format} {name}");
            assert_eq!(requests, ["/binloom"], "{format} {name}");
            assert_eq!(fs::read(dir.join("binloom")).unwrap(), script("evil"));

            let (output, _) = fx.run(&artifact.body);
            assert_ran(&output, "good");
            assert_eq!(fs::read(dir.join("binloom")).unwrap(), artifact.program);
            assert!(leftover_temp_files(&dir).is_empty());
            let (output, requests) = fx.run_offline();
            assert_ran(&output, "good");
            assert!(requests.is_empty(), "{format} {name}");
        }
    }
}

#[test]
fn damaged_retained_artifact_is_repaired() {
    for damage in ["missing", "garbage"] {
        let fx = Fixture::new();
        let artifact = Artifact::new("good", "gz");
        fx.install("1.0.0", &artifact);
        let retained = fx.dir("1.0.0").join(".artifact");
        assert_eq!(fs::read(&retained).unwrap(), artifact.body);
        if damage == "missing" {
            fs::remove_file(&retained).unwrap();
        } else {
            fs::write(&retained, b"garbage").unwrap();
        }

        let (output, _) = fx.run_offline();
        assert_refused(&output, "good");
        assert!(stderr(&output).contains("unverified"), "{damage}");

        let (output, _) = fx.run(&artifact.body);
        assert_ran(&output, "good");
        assert_eq!(fs::read(&retained).unwrap(), artifact.body);
    }
}

#[test]
fn legacy_stamp_only_cache_is_not_trusted() {
    for format in FORMATS {
        let fx = Fixture::new();
        let artifact = Artifact::new("good", format);
        fx.lock("1.0.0", &artifact);
        let dir = fx.dir("1.0.0");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("binloom"), &artifact.program).unwrap();
        fs::set_permissions(dir.join("binloom"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            dir.join(".artifact-sha256"),
            format!("{}\n", checksum(&artifact.body)),
        )
        .unwrap();

        let (output, _) = fx.run_offline();
        assert_refused(&output, "good");
        assert!(stderr(&output).contains("unverified"), "{format}");

        let (output, _) = fx.run(&artifact.body);
        assert_ran(&output, "good");
        assert_eq!(
            fs::read_to_string(dir.join(".executable-sha256")).unwrap(),
            format!("{}\n", checksum(&artifact.program))
        );
        assert_eq!(dir.join(".artifact").exists(), format == "gz");
    }
}

#[test]
fn intact_cache_runs_without_network() {
    for format in FORMATS {
        let fx = Fixture::new();
        let artifact = Artifact::new("good", format);
        fx.install("1.0.0", &artifact);
        let (output, requests) = fx.run_offline();
        assert_ran(&output, "good");
        assert!(requests.is_empty(), "{format}");
    }
}

#[test]
fn invalid_gzip_with_matching_checksum_is_rejected() {
    let fx = Fixture::new();
    let mut artifact = Artifact::new("good", "gz");
    artifact.body = b"definitely not gzip".to_vec();
    fx.lock("1.0.0", &artifact);
    let (output, _) = fx.run(&artifact.body);
    assert_refused(&output, "good");
    assert!(stderr(&output).contains("not a valid gzip"));
    assert!(!fx.dir("1.0.0").join("binloom").exists());
    assert!(leftover_temp_files(&fx.dir("1.0.0")).is_empty());
}

#[test]
fn unsafe_cache_layout_is_refused_without_writing_outside() {
    let cases = [
        "tools-symlink",
        "version-dir-symlink",
        "exe-symlink",
        "stamp-symlink",
        "artifact-symlink",
        "exe-directory",
        "stamp-directory",
        "exe-fifo",
        "stamp-fifo",
        "artifact-fifo",
    ];
    for case in cases {
        let fx = Fixture::new();
        let artifact = Artifact::new("good", "gz");
        fx.install("1.0.0", &artifact);
        let dir = fx.dir("1.0.0");
        let outside = fx.temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("file"), b"untouched").unwrap();

        let target = match case {
            "tools-symlink" => fx.root.join(".tools"),
            "version-dir-symlink" => dir.clone(),
            name if name.starts_with("exe") => dir.join("binloom"),
            name if name.starts_with("stamp") => dir.join(".executable-sha256"),
            _ => dir.join(".artifact"),
        };
        if case.ends_with("symlink") && !case.starts_with("exe") && !case.starts_with("stamp") {
            let moved = fx.temp.path().join("moved");
            fs::rename(&target, &moved).unwrap();
            symlink(&moved, &target).unwrap();
        } else {
            fs::remove_file(&target).unwrap();
            match case.rsplit('-').next().unwrap() {
                "symlink" => symlink(outside.join("file"), &target).unwrap(),
                "directory" => fs::create_dir(&target).unwrap(),
                _ => assert!(
                    Command::new("mkfifo")
                        .arg(&target)
                        .status()
                        .unwrap()
                        .success()
                ),
            }
        }

        let (output, requests) = fx.run(&artifact.body);
        assert_eq!(output.status.code(), Some(1), "{case}: {}", stderr(&output));
        assert!(
            stderr(&output).contains("refusing to use symlink")
                || stderr(&output).contains("expected a"),
            "{case}: {}",
            stderr(&output)
        );
        assert!(stdout(&output).is_empty(), "{case}");
        assert!(requests.is_empty(), "{case}");
        assert_eq!(fs::read(outside.join("file")).unwrap(), b"untouched");
        assert!(leftover_temp_files(&dir).is_empty(), "{case}");
    }
}

#[test]
fn failed_reinstall_keeps_the_existing_install() {
    for format in FORMATS {
        let fx = Fixture::new();
        let artifact = Artifact::new("good", format);
        fx.install("1.0.0", &artifact);
        let dir = fx.dir("1.0.0");
        let before: Vec<_> = [
            "binloom",
            ".artifact-sha256",
            ".executable-sha256",
            ".artifact",
        ]
        .map(|name| fs::read(dir.join(name)).ok())
        .into();

        // The lock now expects other bytes than the cache holds, and the server serves bad ones.
        fx.lock_with_sha("1.0.0", &artifact, &checksum(b"expected"));
        let (output, _) = fx.run(&artifact.body);
        assert_refused(&output, "good");
        assert!(stderr(&output).contains("checksum mismatch"), "{format}");

        let after: Vec<_> = [
            "binloom",
            ".artifact-sha256",
            ".executable-sha256",
            ".artifact",
        ]
        .map(|name| fs::read(dir.join(name)).ok())
        .into();
        assert_eq!(before, after, "{format}");
        assert!(leftover_temp_files(&dir).is_empty(), "{format}");
    }
}

#[test]
fn executable_modes_are_respected_or_repaired() {
    let fx = Fixture::new();
    let artifact = Artifact::new("good", "raw");
    fx.install("1.0.0", &artifact);
    let exe = fx.dir("1.0.0").join("binloom");
    let mode = || fs::metadata(&exe).unwrap().permissions().mode() & 0o777;

    // Only the owner execute bit is added; other bits are never widened.
    for (before, after) in [(0o600, 0o700), (0o644, 0o744), (0o700, 0o700)] {
        fs::set_permissions(&exe, fs::Permissions::from_mode(before)).unwrap();
        let (output, requests) = fx.run_offline();
        assert_ran(&output, "good");
        assert!(requests.is_empty());
        assert_eq!(mode(), after, "{before:o}");
    }
}

#[test]
fn writable_cached_executable_is_replaced_not_run() {
    let fx = Fixture::new();
    let artifact = Artifact::new("good", "raw");
    fx.install("1.0.0", &artifact);
    let exe = fx.dir("1.0.0").join("binloom");
    // Either write bit alone must be rejected, matching the Rust `mode & 0o022` rule.
    for mode in [0o777, 0o775, 0o757] {
        fs::set_permissions(&exe, fs::Permissions::from_mode(mode)).unwrap();
        let (output, _) = fx.run_offline();
        assert_refused(&output, "good");

        let (output, requests) = fx.run(&artifact.body);
        assert_ran(&output, "good");
        assert_eq!(requests, ["/binloom"]);
        assert_eq!(
            fs::metadata(&exe).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
}

#[test]
fn bumped_lock_never_runs_an_older_cached_version() {
    let fx = Fixture::new();
    let old = Artifact::new("old", "gz");
    fx.install("1.0.0", &old);

    let new = Artifact::new("new", "gz");
    fx.lock("2.0.0", &new);
    let (output, requests) = fx.run(&new.body);
    assert_ran(&output, "new");
    assert_eq!(requests, ["/binloom"]);
    assert_eq!(
        fs::read(fx.dir("1.0.0").join("binloom")).unwrap(),
        old.program
    );

    // Offline with a lock for a version that was never installed: stale dirs are ignored.
    fx.lock("3.0.0", &new);
    let (output, _) = fx.run_offline();
    assert_refused(&output, "ran:");
    assert!(!fx.dir("3.0.0").join("binloom").exists());
}

#[test]
fn concurrent_wrappers_on_an_empty_cache_all_succeed() {
    for format in FORMATS {
        let fx = Fixture::new();
        let artifact = Artifact::new("good", format);
        fx.lock("1.0.0", &artifact);

        let (outputs, _) = fx.serving(&artifact.body, || {
            let children: Vec<_> = (0..6)
                .map(|_| {
                    fx.command()
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .spawn()
                        .unwrap()
                })
                .collect();
            children
                .into_iter()
                .map(|child| child.wait_with_output().unwrap())
                .collect::<Vec<_>>()
        });
        for output in &outputs {
            assert_ran(output, "good");
        }

        let (output, requests) = fx.run_offline();
        assert_ran(&output, "good");
        assert!(requests.is_empty(), "{format}");
        assert!(leftover_temp_files(&fx.dir("1.0.0")).is_empty(), "{format}");
    }
}

#[test]
fn abandoned_temp_files_are_swept_but_live_and_foreign_ones_are_kept() {
    let fx = Fixture::new();
    let artifact = Artifact::new("good", "raw");
    fx.lock("1.0.0", &artifact);
    let dir = fx.dir("1.0.0");
    fs::create_dir_all(&dir).unwrap();
    let outside = fx.temp.path().join("outside");
    fs::write(&outside, b"untouched").unwrap();

    let dead = dir.join(".binloomw-tmp.99999999.AAAAAA");
    let live = dir.join(format!(".binloomw-tmp.{}.BBBBBB", std::process::id()));
    let link = dir.join(".binloomw-tmp.99999999.CCCCCC");
    let foreign = dir.join(".download.DDDDDD");
    for file in [&dead, &live, &foreign] {
        fs::write(file, b"temp").unwrap();
    }
    symlink(&outside, &link).unwrap();

    let (output, _) = fx.run(&artifact.body);
    assert_ran(&output, "good");
    assert!(!dead.exists());
    assert!(live.exists() && foreign.exists());
    assert!(link.symlink_metadata().is_ok());
    assert_eq!(fs::read(&outside).unwrap(), b"untouched");
}

#[test]
fn group_or_other_writable_cache_directories_are_refused() {
    let fx = Fixture::new();
    let artifact = Artifact::new("good", "raw");
    fx.install("1.0.0", &artifact);
    let dirs = [
        fx.root.join(".tools"),
        fx.root.join(".tools/binloom"),
        fx.dir("1.0.0"),
    ];
    // Either write bit alone must be rejected, matching the Rust `mode & 0o022` rule.
    for dir in &dirs {
        for mode in [0o775, 0o757, 0o777] {
            fs::set_permissions(dir, fs::Permissions::from_mode(mode)).unwrap();
            let (output, requests) = fx.run_offline();
            assert_refused(&output, "good");
            let message = stderr(&output);
            assert!(
                message.contains(&format!("chmod go-w {}", dir.display())),
                "{message}"
            );
            assert!(requests.is_empty());
        }
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
        let (output, _) = fx.run_offline();
        assert_ran(&output, "good");
    }
}

#[test]
fn fresh_install_under_a_group_writable_umask_creates_private_directories() {
    let fx = Fixture::new();
    let artifact = Artifact::new("good", "raw");
    fx.lock("1.0.0", &artifact);
    let (output, _) = fx.serving(&artifact.body, || {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "umask 002; exec ./binloomw"])
            .current_dir(&fx.root)
            .env_clear()
            .env("PATH", fx.temp.path().join("bin"))
            .env("HOME", fx.temp.path());
        command.output().unwrap()
    });
    assert_ran(&output, "good");
    for dir in [
        fx.root.join(".tools"),
        fx.root.join(".tools/binloom"),
        fx.dir("1.0.0"),
    ] {
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
}

#[test]
fn multi_member_gzip_installs_and_is_reused() {
    let fx = Fixture::new();
    let artifact = Artifact::multi_member("multi");
    fx.install("1.0.0", &artifact);
    assert_eq!(
        fs::read(fx.dir("1.0.0").join("binloom")).unwrap(),
        artifact.program
    );
    let (output, requests) = fx.run_offline();
    assert_ran(&output, "multi");
    assert!(requests.is_empty());
}
