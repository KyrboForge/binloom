#![cfg(unix)]

mod common;

use common::{
    Artifact, Response, TestServer, artifact_section, assert_ran, assert_refused, binloom_lock,
    checksum, isolated_path, leftover_temp_files, script, stderr, wrapper_command, wrapper_source,
    write_executable,
};
use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    process::{Child, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const VERSION: &str = "1.0.0";
const BOOTSTRAP_ROUTE: &str = "/binloom";
const WRAPPER_ROUTE: &str = "/binloomw";
const SIMULATED_KEY: &str = "linux-x86_64";

struct Project {
    home: tempfile::TempDir,
    root: PathBuf,
}

impl Project {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("project");
        fs::create_dir(&root).unwrap();
        isolated_path(home.path());
        fs::write(
            root.join("binloom.toml"),
            format!("manifest-version = 1\n[binloom]\nversion = \"{VERSION}\"\n"),
        )
        .unwrap();
        let project = Self { home, root };
        write_executable(&project.wrapper(), wrapper_source(VERSION));
        project
    }

    fn simulated(os: &str, arch: &str, label: &str) -> Self {
        let project = Self::new();
        let uname = project.home.path().join("bin/uname");
        fs::remove_file(&uname).unwrap();
        write_executable(
            &uname,
            format!(
                "#!/bin/sh\ncase \"${{1:-}}\" in\n-s) echo '{os}';;\n-m) echo '{arch}';;\n*) echo '{label}';;\nesac\n"
            ),
        );
        project
    }

    fn wrapper(&self) -> PathBuf {
        self.root.join("binloomw")
    }

    fn cache(&self, version: &str) -> PathBuf {
        self.root.join(".tools/binloom").join(version)
    }

    fn set_lock(&self, lock: &str) {
        fs::write(self.root.join("binloom.lock"), lock).unwrap();
    }

    fn inputs(&self) -> [Option<Vec<u8>>; 2] {
        ["binloom.toml", "binloom.lock"].map(|name| match fs::read(self.root.join(name)) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => panic!("read {name}: {error}"),
        })
    }

    fn spawn(&self, shell: Option<&Path>) -> Child {
        wrapper_command(&self.root, self.home.path(), shell)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }

    fn run(&self) -> Output {
        self.run_with_shell(None)
    }

    fn run_with_shell(&self, shell: Option<&Path>) -> Output {
        let before = self.inputs();
        let output = wait(self.spawn(shell));
        assert_eq!(self.inputs(), before, "wrapper changed project inputs");
        output
    }

    fn assert_wrapper_bytes(&self, expected: impl AsRef<[u8]>) {
        assert_eq!(fs::read(self.wrapper()).unwrap(), expected.as_ref());
        assert!(fs::read_dir(&self.root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".binloomw.")
        }));
    }
}

fn wait(mut child: Child) -> Output {
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            child.wait().unwrap();
            panic!("wrapper exceeded 15 seconds (possible re-exec loop)");
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

fn url(server: &TestServer, route: &str) -> String {
    format!("http://{}{route}", server.address())
}

fn bootstrap_section(server: &TestServer, key: &str, route: &str, artifact: &Artifact) -> String {
    artifact_section(
        &format!("binloom.artifacts.{key}"),
        &url(server, route),
        &checksum(&artifact.body),
        artifact.format,
    )
}

fn bootstrap_lock(server: &TestServer, version: &str, key: &str, artifact: &Artifact) -> String {
    binloom_lock(version) + &bootstrap_section(server, key, BOOTSTRAP_ROUTE, artifact)
}

fn wrapper_pin(server: &TestServer, version: &str, expected: &[u8]) -> String {
    format!(
        "\n[wrapper]\nversion = \"{version}\"\nurl = \"{}\"\nsha256 = \"{}\"\n",
        url(server, WRAPPER_ROUTE),
        checksum(expected),
    )
}

fn serve_binloom(server: &TestServer, project: &Project, artifact: &Artifact) -> Output {
    let (output, requests) = server.serving(
        &[(BOOTSTRAP_ROUTE, artifact.body.as_slice().into())],
        || project.run(),
    );
    assert_eq!(requests, [BOOTSTRAP_ROUTE]);
    output
}

fn cache_bytes(dir: &Path) -> [Vec<u8>; 4] {
    [
        "binloom",
        ".artifact",
        ".artifact-sha256",
        ".executable-sha256",
    ]
    .map(|name| fs::read(dir.join(name)).unwrap())
}

fn assert_install(project: &Project, version: &str, artifact: &Artifact) {
    let dir = project.cache(version);
    assert_eq!(
        cache_bytes(&dir),
        [
            artifact.program.clone(),
            artifact.body.clone(),
            format!("{}\n", checksum(&artifact.body)).into_bytes(),
            format!("{}\n", checksum(&artifact.program)).into_bytes(),
        ]
    );
    assert!(leftover_temp_files(&dir).is_empty());
}

fn assert_no_install(project: &Project, version: &str) {
    let cache = project.cache(version);
    assert!(!cache.exists() || fs::read_dir(cache).unwrap().next().is_none());
}

fn assert_error(output: &Output, message: &str) {
    assert_refused(output, "ran:");
    assert!(
        stderr(output).lines().any(|line| line == message),
        "{}",
        stderr(output)
    );
}

#[test]
fn simulated_clean_checkout_installs_verified_pin_and_runs_with_server_down() {
    let project = Project::simulated("Linux", "x86_64", "glibc");
    let server = TestServer::new();
    let artifact = Artifact::new(VERSION, "gz");
    project.set_lock(&bootstrap_lock(&server, VERSION, SIMULATED_KEY, &artifact));
    assert!(!project.root.join(".tools").exists());

    let check_no_publication = || {
        for name in [
            "binloom",
            ".artifact",
            ".artifact-sha256",
            ".executable-sha256",
        ] {
            assert!(!project.cache(VERSION).join(name).exists());
        }
    };
    let response = Response {
        before_body: Some(&check_no_publication),
        ..artifact.body.as_slice().into()
    };
    let (output, requests) = server.serving(&[(BOOTSTRAP_ROUTE, response)], || project.run());
    assert_ran(&output, VERSION);
    assert_eq!(requests, [BOOTSTRAP_ROUTE]);
    assert_install(&project, VERSION, &artifact);

    let (output, requests) = server.serving(&[], || project.run());
    assert_ran(&output, VERSION);
    assert!(requests.is_empty());
    drop(server);
    assert_ran(&project.run(), VERSION);
    assert_install(&project, VERSION, &artifact);
}

#[test]
fn simulated_checksum_failure_and_short_download_leave_empty_cache_then_recover() {
    let project = Project::simulated("Linux", "x86_64", "glibc");
    let server = TestServer::new();
    let artifact = Artifact::new(VERSION, "gz");
    let bad = Artifact::new("wrong-pin", "gz");
    project.set_lock(&bootstrap_lock(&server, VERSION, SIMULATED_KEY, &artifact));

    let output = serve_binloom(&server, &project, &bad);
    assert_error(&output, "error: Binloom checksum mismatch");
    assert_no_install(&project, VERSION);

    let truncated = Response {
        body: &artifact.body[..artifact.body.len() / 2],
        content_length: artifact.body.len(),
        before_body: None,
    };
    let (output, requests) = server.serving(&[(BOOTSTRAP_ROUTE, truncated)], || project.run());
    assert_refused(&output, "ran:");
    assert!(stderr(&output).contains("cannot download Binloom"));
    assert!(!requests.is_empty() && requests.iter().all(|route| route == BOOTSTRAP_ROUTE));
    assert_no_install(&project, VERSION);

    assert_ran(&serve_binloom(&server, &project, &artifact), VERSION);
    assert_install(&project, VERSION, &artifact);
}

#[test]
fn simulated_corrupt_cache_is_repaired_online_and_refused_offline() {
    let project = Project::simulated("Linux", "x86_64", "musl");
    let server = TestServer::new();
    let artifact = Artifact::new(VERSION, "gz");
    project.set_lock(&bootstrap_lock(&server, VERSION, SIMULATED_KEY, &artifact));
    assert_ran(&serve_binloom(&server, &project, &artifact), VERSION);

    let binary = project.cache(VERSION).join("binloom");
    let modified = script("modified");
    write_executable(&binary, &modified);
    assert_ran(&serve_binloom(&server, &project, &artifact), VERSION);
    assert_install(&project, VERSION, &artifact);

    write_executable(&binary, &modified);
    drop(server);
    let output = project.run();
    assert_refused(&output, "ran:");
    assert!(stderr(&output).contains("refusing to run unverified bytes"));
    assert_eq!(fs::read(binary).unwrap(), modified);
    assert!(leftover_temp_files(&project.cache(VERSION)).is_empty());
}

#[test]
fn simulated_parallel_bootstraps_all_run_verified_bytes() {
    let project = Project::simulated("Linux", "x86_64", "musl");
    let server = TestServer::new();
    let artifact = Artifact::new(VERSION, "gz");
    project.set_lock(&bootstrap_lock(&server, VERSION, SIMULATED_KEY, &artifact));
    let before = project.inputs();
    let (_, requests) = server.serving(
        &[(BOOTSTRAP_ROUTE, artifact.body.as_slice().into())],
        || {
            let children: Vec<_> = (0..12).map(|_| project.spawn(None)).collect();
            for child in children {
                assert_ran(&wait(child), VERSION);
                assert_eq!(project.inputs(), before);
            }
        },
    );
    assert!(!requests.is_empty() && requests.iter().all(|route| route == BOOTSTRAP_ROUTE));
    assert_install(&project, VERSION, &artifact);
    drop(server);
    assert_ran(&project.run(), VERSION);
}

#[test]
fn simulated_failed_binloom_upgrade_preserves_old_pin_and_cache() {
    let project = Project::simulated("Linux", "x86_64", "glibc");
    let server = TestServer::new();
    let old = Artifact::new(VERSION, "gz");
    let upgrade = Artifact::new("2.0.0", "gz");
    let old_lock = bootstrap_lock(&server, VERSION, SIMULATED_KEY, &old);
    project.set_lock(&old_lock);
    assert_ran(&serve_binloom(&server, &project, &old), VERSION);
    let old_cache = cache_bytes(&project.cache(VERSION));

    project.set_lock(&bootstrap_lock(&server, "2.0.0", SIMULATED_KEY, &upgrade));
    let (output, requests) = server.serving(&[], || project.run());
    assert_refused(&output, "ran:");
    assert!(stderr(&output).contains("cannot download Binloom 2.0.0"));
    assert_eq!(requests, [BOOTSTRAP_ROUTE]);
    assert_no_install(&project, "2.0.0");
    assert_eq!(cache_bytes(&project.cache(VERSION)), old_cache);

    let output = serve_binloom(&server, &project, &old);
    assert_error(&output, "error: Binloom checksum mismatch");
    assert_no_install(&project, "2.0.0");
    assert_eq!(cache_bytes(&project.cache(VERSION)), old_cache);

    project.set_lock(&old_lock);
    drop(server);
    assert_ran(&project.run(), VERSION);
    assert_eq!(cache_bytes(&project.cache(VERSION)), old_cache);
}

#[test]
fn simulated_failed_wrapper_download_and_checksum_preserve_existing_wrapper() {
    let project = Project::simulated("Linux", "x86_64", "glibc");
    let server = TestServer::new();
    let artifact = Artifact::new(VERSION, "gz");
    let original = fs::read(project.wrapper()).unwrap();
    let upgraded = wrapper_source("2.0.0");
    let lock = bootstrap_lock(&server, VERSION, SIMULATED_KEY, &artifact);
    project.set_lock(&(lock + &wrapper_pin(&server, "2.0.0", upgraded.as_bytes())));

    let (output, requests) = server.serving(&[], || project.run());
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(requests, [WRAPPER_ROUTE]);
    project.assert_wrapper_bytes(&original);
    assert!(!project.root.join(".tools").exists());

    let (output, requests) = server.serving(&[(WRAPPER_ROUTE, original.as_slice().into())], || {
        project.run()
    });
    assert_error(&output, "error: Binloom wrapper checksum mismatch");
    assert_eq!(requests, [WRAPPER_ROUTE]);
    project.assert_wrapper_bytes(&original);
    assert!(!project.root.join(".tools").exists());
}

#[test]
fn simulated_wrapper_pin_rejects_mismatching_version_then_accepts_matching_version() {
    let project = Project::simulated("Linux", "x86_64", "glibc");
    let server = TestServer::new();
    let artifact = Artifact::new(VERSION, "gz");
    let original = fs::read(project.wrapper()).unwrap();
    let lock = bootstrap_lock(&server, VERSION, SIMULATED_KEY, &artifact);
    let published = wrapper_source("2.0.0");
    project.set_lock(&(lock.clone() + &wrapper_pin(&server, "3.0.0", published.as_bytes())));

    let (output, requests) = server
        .serving(&[(WRAPPER_ROUTE, published.as_bytes().into())], || {
            project.run()
        });
    assert_error(
        &output,
        "error: downloaded Binloom wrapper embeds version 2.0.0; lock requires 3.0.0",
    );
    assert_eq!(requests, [WRAPPER_ROUTE]);
    project.assert_wrapper_bytes(&original);
    assert!(!project.root.join(".tools").exists());

    project.set_lock(&(lock + &wrapper_pin(&server, "2.0.0", published.as_bytes())));
    assert_replacement_runs(&project, &server, &artifact, &published, VERSION);
    drop(server);
    assert_ran(&project.run(), VERSION);
}

#[test]
fn simulated_reexecuted_wrapper_can_never_attempt_second_replacement() {
    let project = Project::simulated("Linux", "x86_64", "glibc");
    let server = TestServer::new();
    let artifact = Artifact::new(VERSION, "gz");
    // The release line matches, but this test wrapper changes its own runtime value.
    let published = wrapper_source("2.0.0").replace(
        "BINLOOMW_VERSION=\"2.0.0\"\n",
        "BINLOOMW_VERSION=\"2.0.0\"\nBINLOOMW_VERSION=unexpected\n",
    );
    project.set_lock(
        &(bootstrap_lock(&server, VERSION, SIMULATED_KEY, &artifact)
            + &wrapper_pin(&server, "2.0.0", published.as_bytes())),
    );
    let (output, requests) = server
        .serving(&[(WRAPPER_ROUTE, published.as_bytes().into())], || {
            project.run()
        });
    assert_error(
        &output,
        "error: Binloom wrapper still differs from its pin after replacement; refusing to replace it again",
    );
    assert_eq!(requests, [WRAPPER_ROUTE]);
    project.assert_wrapper_bytes(&published);
    assert!(!project.root.join(".tools").exists());
}

fn assert_replacement_runs(
    project: &Project,
    server: &TestServer,
    artifact: &Artifact,
    published: &str,
    version: &str,
) {
    let (output, requests) = server.serving(
        &[
            (WRAPPER_ROUTE, published.as_bytes().into()),
            (BOOTSTRAP_ROUTE, artifact.body.as_slice().into()),
        ],
        || project.run(),
    );
    assert_ran(&output, &artifact.tag);
    assert_eq!(requests, [WRAPPER_ROUTE, BOOTSTRAP_ROUTE]);
    project.assert_wrapper_bytes(published);
    assert_install(project, version, artifact);
}

#[test]
fn simulated_tagged_wrappers_self_update_then_bootstrap_new_binloom() {
    // Verbatim tagged wrapper sources with the release version placeholder expanded.
    for previous in [
        include_bytes!("fixtures/binloomw-0.4.0.sh").as_slice(),
        include_bytes!("fixtures/binloomw-0.3.0.sh").as_slice(),
    ] {
        let project = Project::simulated("Linux", "x86_64", "glibc");
        let server = TestServer::new();
        let artifact = Artifact::new("2.0.0", "gz");
        let version = env!("CARGO_PKG_VERSION");
        let current = wrapper_source(version);
        write_executable(&project.wrapper(), previous);
        project.set_lock(
            &(evolved_lock(&server, &artifact)
                + &wrapper_pin(&server, version, current.as_bytes())),
        );
        fs::write(project.root.join("binloom.toml"), evolved_manifest()).unwrap();

        let (output, requests) = server.serving(
            &[
                (WRAPPER_ROUTE, current.as_bytes().into()),
                ("/linux-x86_64", artifact.body.as_slice().into()),
            ],
            || project.run(),
        );
        assert_ran(&output, "2.0.0");
        assert_eq!(requests, [WRAPPER_ROUTE, "/linux-x86_64"]);
        project.assert_wrapper_bytes(&current);
        assert_install(&project, "2.0.0", &artifact);
    }
}

const LEGACY_KEYS: [&str; 4] = [
    "macos-aarch64",
    "macos-x86_64",
    "linux-aarch64",
    "linux-x86_64",
];
const EXPLICIT_TARGETS: &str = r#"
[platforms]
targets = ["macos-aarch64", "macos-x86_64", "linux-aarch64-musl", "linux-x86_64-gnu", "linux-x86_64-musl"]

[tools.example]
version = "9.0.0"
source = "github:example/example"
targets = ["linux-x86_64-gnu", "linux-x86_64-musl"]
asset-pattern = "example_{target}.gz"
"#;

fn evolved_manifest() -> String {
    format!("manifest-version = 1\n[binloom]\nversion = \"2.0.0\"\n{EXPLICIT_TARGETS}")
}

fn evolved_lock(server: &TestServer, artifact: &Artifact) -> String {
    let mut lock = binloom_lock("2.0.0") + EXPLICIT_TARGETS + "tag = \"v9.0.0\"\n";
    for (section, route) in [
        ("tools.example.artifacts.linux-x86_64-gnu", "/gnu-tool"),
        ("tools.example.artifacts.linux-x86_64-musl", "/musl-tool"),
    ] {
        lock.push_str(&artifact_section(
            section,
            &url(server, route),
            &checksum(&artifact.body),
            artifact.format,
        ));
    }
    for key in LEGACY_KEYS {
        lock.push_str(&bootstrap_section(
            server,
            key,
            &format!("/{key}"),
            artifact,
        ));
    }
    lock
}

#[test]
fn simulated_evolved_v1_uses_only_legacy_bootstrap_keys_on_each_host() {
    for (os, arch, label, index) in [
        ("Darwin", "arm64", "macOS", 0),
        ("Darwin", "x86_64", "macOS", 1),
        ("Linux", "aarch64", "Linux", 2),
        // Both libcs select the same static Binloom build. Tool targets can differ.
        ("Linux", "x86_64", "glibc", 3),
        ("Linux", "x86_64", "musl", 3),
    ] {
        let project = Project::simulated(os, arch, label);
        let server = TestServer::new();
        let key = LEGACY_KEYS[index];
        let route = format!("/{key}");
        let artifact = Artifact::new(key, "gz");
        project.set_lock(&evolved_lock(&server, &artifact));
        fs::write(project.root.join("binloom.toml"), evolved_manifest()).unwrap();
        let (output, requests) = server
            .serving(&[(&route, artifact.body.as_slice().into())], || {
                project.run()
            });
        assert_ran(&output, key);
        assert_eq!(requests, [route], "{label}");
        assert_install(&project, "2.0.0", &artifact);
    }
}

#[test]
fn simulated_unsupported_hosts_and_missing_pins_fail_before_network_or_cache() {
    let server = TestServer::new();
    let artifact = Artifact::new(VERSION, "gz");
    let published = wrapper_source("2.0.0");
    let wrong_key_lock = bootstrap_lock(&server, VERSION, "linux-aarch64", &artifact)
        + &wrapper_pin(&server, "2.0.0", published.as_bytes())
        + &artifact_section(
            "tools.example.artifacts.linux-x86_64-musl",
            &url(&server, "/musl-tool"),
            &checksum(&artifact.body),
            artifact.format,
        );
    for (os, arch, lock, message) in [
        (
            "FreeBSD",
            "x86_64",
            Some(wrong_key_lock.as_str()),
            "error: unsupported operating system: FreeBSD. Supported hosts: macos or linux on aarch64 or x86_64.",
        ),
        (
            "Linux",
            "riscv64",
            Some(wrong_key_lock.as_str()),
            "error: unsupported architecture: riscv64. Supported hosts: macos or linux on aarch64 or x86_64.",
        ),
        (
            "Linux",
            "x86_64",
            Some(wrong_key_lock.as_str()),
            "error: missing [binloom.artifacts.linux-x86_64] in binloom.lock. The wrapper selects Binloom's own artifact by OS+arch only (no libc detection or fallback). Re-lock with 'binloom update --self' using a Binloom release that publishes this artifact, or use a supported host with a locked artifact.",
        ),
        (
            "Linux",
            "x86_64",
            Some("lock-version = 1\n[platforms]\ntargets = [\"linux-x86_64-musl\"]\n"),
            "error: missing [binloom] in binloom.lock. Run 'binloom update' to regenerate the lock, or 'binloom init' to set up the project.",
        ),
        (
            "Linux",
            "x86_64",
            None,
            "error: binloom.lock not found. Run 'binloom update' to generate the lock, or 'binloom init' to set up the project.",
        ),
    ] {
        let project = Project::simulated(os, arch, "unsupported");
        if let Some(lock) = lock {
            project.set_lock(lock);
        }
        let (output, requests) = server.serving(&[], || project.run());
        assert_error(&output, message);
        assert!(requests.is_empty());
        assert!(!project.root.join(".tools").exists());
        project.assert_wrapper_bytes(wrapper_source(VERSION));
    }
}

#[test]
fn real_host_smoke_uses_native_uname() {
    let key = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    if !LEGACY_KEYS.contains(&key.as_str()) {
        eprintln!("REAL-HOST bootstrap smoke skipped: unsupported host {key}");
        return;
    }
    let project = Project::new();
    let server = TestServer::new();
    let artifact = Artifact::new(&key, "gz");
    project.set_lock(&bootstrap_lock(&server, VERSION, &key, &artifact));
    assert_ran(&serve_binloom(&server, &project, &artifact), &key);
    assert_install(&project, VERSION, &artifact);

    let dash = Path::new("/bin/dash");
    if dash.is_file() {
        drop(server);
        assert_ran(&project.run_with_shell(Some(dash)), &key);
    }
}
