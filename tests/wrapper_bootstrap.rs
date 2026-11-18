#![cfg(unix)]

use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::Duration,
};

const TOOL: &str = "wrapper-test-tool";
const OTHER_TOOL: &str = "other-tool";
const TOOL_SCRIPT: &[u8] = b"#!/bin/sh\nprintf '<%s>\\n' \"$@\"\nexit 42\n";

fn checksum(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn serve(listener: &TcpListener, artifacts: &[(&str, &[u8])]) -> Vec<String> {
    let mut requests = Vec::new();
    for stream in listener.incoming() {
        let mut stream = stream.unwrap();
        let timeout = Some(Duration::from_secs(10));
        stream.set_read_timeout(timeout).unwrap();
        stream.set_write_timeout(timeout).unwrap();

        let mut reader = BufReader::new(&stream);
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap() == 0 {
            // The test closes an empty connection to stop the server.
            break;
        }
        let mut request = line.split_ascii_whitespace();
        assert_eq!(request.next(), Some("GET"));
        let path = request.next().unwrap().to_owned();
        loop {
            line.clear();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
        }

        if let Some((_, body)) = artifacts.iter().find(|(route, _)| *route == path) {
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        } else {
            stream
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        }
        requests.push(path);
    }
    requests
}

#[test]
fn wrapper_bootstraps_binloom_and_installs_only_the_executed_tool() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("fixture project");
    let path = temp.path().join("bin");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&path).unwrap();
    for utility in UTILITIES {
        if let Some(executable) = ["/usr/bin", "/bin"]
            .map(|dir| Path::new(dir).join(utility))
            .into_iter()
            .find(|file| file.is_file())
        {
            symlink(executable, path.join(utility)).unwrap();
        }
    }

    let version = env!("CARGO_PKG_VERSION");
    let binloom = fs::read(env!("CARGO_BIN_EXE_binloom")).unwrap();
    let wrapper = root.join("binloomw");
    fs::write(
        &wrapper,
        include_str!("../src/binloomw.sh").replace("@BINLOOM_VERSION@", version),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
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
        ("/binloom", binloom.as_slice()),
        (tool_route.as_str(), TOOL_SCRIPT),
        (other_route.as_str(), TOOL_SCRIPT),
    ];
    let (output, requests) = thread::scope(|scope| {
        let server = scope.spawn(|| serve(&listener, &artifacts));
        let output = Command::new(&wrapper)
            .current_dir(&root)
            .env_clear()
            .env("PATH", &path)
            .env("HOME", temp.path())
            .env("XDG_CACHE_HOME", temp.path().join("cache"))
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .args(["exec", TOOL, "two words", "--flag", "", "*?[a]"])
            .output();
        drop(TcpStream::connect(address).unwrap());
        (output.unwrap(), server.join().unwrap())
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

const UTILITIES: [&str; 18] = [
    "awk",
    "cat",
    "chmod",
    "cp",
    "curl",
    "dirname",
    "gzip",
    "head",
    "mkdir",
    "mktemp",
    "mv",
    "rm",
    "sha256sum",
    "shasum",
    "sync",
    "uname",
    "wc",
    "wget",
];

fn script(tag: &str) -> Vec<u8> {
    format!("#!/bin/sh\necho ran:{tag}\n").into_bytes()
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

/// An artifact as served for a locked Binloom version: the bytes on the wire and the program they unpack to.
struct Artifact {
    tag: String,
    body: Vec<u8>,
    program: Vec<u8>,
    format: &'static str,
}

impl Artifact {
    fn new(tag: &str, format: &'static str) -> Self {
        let program = script(tag);
        let body = if format == "gz" {
            gzip(&program)
        } else {
            program.clone()
        };
        Self {
            tag: tag.to_owned(),
            body,
            program,
            format,
        }
    }
}

const FORMATS: [&str; 2] = ["raw", "gz"];

struct Fixture {
    temp: tempfile::TempDir,
    root: PathBuf,
    listener: TcpListener,
    address: SocketAddr,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        let path = temp.path().join("bin");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&path).unwrap();
        for utility in UTILITIES {
            if let Some(executable) = ["/usr/bin", "/bin"]
                .map(|dir| Path::new(dir).join(utility))
                .into_iter()
                .find(|file| file.is_file())
            {
                symlink(executable, path.join(utility)).unwrap();
            }
        }
        let wrapper = root.join("binloomw");
        fs::write(
            &wrapper,
            include_str!("../src/binloomw.sh")
                .replace("@BINLOOM_VERSION@", env!("CARGO_PKG_VERSION")),
        )
        .unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        Self {
            temp,
            root,
            listener,
            address,
        }
    }

    fn lock(&self, version: &str, artifact: &Artifact) {
        self.lock_with_sha(version, artifact, &checksum(&artifact.body));
    }

    fn lock_with_sha(&self, version: &str, artifact: &Artifact, sha: &str) {
        let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
        let format = artifact.format;
        fs::write(
            self.root.join("binloom.lock"),
            format!(
                "lock-version = 1\n[binloom]\nversion = \"{version}\"\n\
                 [binloom.artifacts.{platform}]\nurl = \"http://{}/binloom\"\n\
                 sha256 = \"{sha}\"\nformat = \"{format}\"\n",
                self.address
            ),
        )
        .unwrap();
    }

    fn dir(&self, version: &str) -> PathBuf {
        self.root.join(".tools/binloom").join(version)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(self.root.join("binloomw"));
        command
            .current_dir(&self.root)
            .env_clear()
            .env("PATH", self.temp.path().join("bin"))
            .env("HOME", self.temp.path());
        command
    }

    /// Runs `f` while the fixture server serves `body` (nothing when empty) at `/binloom`; returns the paths requested.
    fn serving<R>(&self, body: &[u8], f: impl FnOnce() -> R) -> (R, Vec<String>) {
        let routes: Vec<(&str, &[u8])> = if body.is_empty() {
            Vec::new()
        } else {
            vec![("/binloom", body)]
        };
        thread::scope(|scope| {
            let server = scope.spawn(|| serve(&self.listener, &routes));
            let result = f();
            drop(TcpStream::connect(self.address).unwrap());
            (result, server.join().unwrap())
        })
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

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_ran(output: &Output, tag: &str) {
    assert!(output.status.success(), "{}", stderr(output));
    assert_eq!(stdout(output), format!("ran:{tag}\n"));
}

fn assert_refused(output: &Output, tag: &str) {
    assert_eq!(output.status.code(), Some(1), "{}", stderr(output));
    assert!(!stdout(output).contains(tag), "{}", stdout(output));
}

fn leftover_temp_files(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with(".binloomw-tmp."))
        .collect()
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

    fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
    let (output, requests) = fx.run_offline();
    assert_ran(&output, "good");
    assert!(requests.is_empty());
    assert_eq!(mode(), 0o700);

    fs::set_permissions(&exe, fs::Permissions::from_mode(0o644)).unwrap();
    let (output, requests) = fx.run_offline();
    assert_ran(&output, "good");
    assert!(requests.is_empty());
    assert_eq!(mode(), 0o755);
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
