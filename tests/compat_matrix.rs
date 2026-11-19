#![cfg(unix)]

#[path = "../src/http_fixture.rs"]
mod http_fixture;

use http_fixture::{Response, Server};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

const LEGACY: &[&str] = &[
    "macos-aarch64",
    "macos-x86_64",
    "linux-aarch64",
    "linux-x86_64",
];
const TOOL: &str = "matrix-tool";
const OTHER: &str = "matrix-other";
const SCRIPT: &[u8] = b"#!/bin/sh\nprintf 'pinned matrix tool\\n'\n";
const MANIFEST: &str = "binloom.toml";
const LOCK: &str = "binloom.lock";

fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_binloom"))
        .current_dir(root)
        .env("NO_PROXY", "127.0.0.1")
        .env("no_proxy", "127.0.0.1")
        .args(args)
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn failure(output: &Output) -> String {
    assert!(!output.status.success());
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn server(requests: usize) -> Server {
    Server::start(
        (0..requests)
            .map(|_| Response {
                status: 200,
                body: SCRIPT.to_vec(),
            })
            .collect(),
    )
}

fn assert_requests(server: &Server, expected: &[&str]) {
    let requests = server.requests();
    assert_eq!(
        requests
            .iter()
            .map(|request| request.path.as_str())
            .collect::<Vec<_>>(),
        expected
    );
    assert!(
        requests
            .iter()
            .all(|request| request.authorization.is_none() && request.private_token.is_none())
    );
}

fn platform_table(targets: &[&str]) -> String {
    format!("\n[platforms]\ntargets = {targets:?} # project targets\n")
}

fn seed_project(
    root: &Path,
    url: &str,
    project_targets: Option<&[&str]>,
    tools: &[(&str, &[&str])],
) -> (String, String) {
    use std::fmt::Write;

    let mut manifest =
        "# v1 manifest\nmanifest-version = 1\n[binloom]\nversion = '1.0.0'\n".to_owned();
    let mut lock = "# v1 lock\nlock-version = 1\n".to_owned();
    if let Some(targets) = project_targets {
        let table = platform_table(targets);
        manifest.push_str(&table);
        lock.push_str(&table);
    }
    let digest: String = Sha256::digest(SCRIPT)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    for &(tool, targets) in tools {
        let header =
            format!("\n[tools.{tool}]\nversion = '1.0.0'\nsource = 'github:owner/{tool}'\n");
        manifest.push_str(&header);
        lock.push_str(&header);
        writeln!(lock, "tag = 'v1.0.0'").unwrap();
        if let Some(project_targets) = project_targets {
            if targets != project_targets {
                writeln!(manifest, "targets = {targets:?}").unwrap();
            }
            writeln!(lock, "targets = {targets:?}").unwrap();
        }
        for target in targets {
            writeln!(lock, "\n[tools.{tool}.artifacts.{target}]\nasset = '{tool}-{target}'\nurl = '{url}/{tool}/{target}'\nsha256 = '{digest}'\nformat = 'raw'").unwrap();
        }
    }
    fs::write(root.join(MANIFEST), &manifest).unwrap();
    fs::write(root.join(LOCK), &lock).unwrap();
    (manifest, lock)
}

fn assert_files(root: &Path, manifest: &str, lock: &str) {
    assert_eq!(fs::read(root.join(MANIFEST)).unwrap(), manifest.as_bytes());
    assert_eq!(fs::read(root.join(LOCK)).unwrap(), lock.as_bytes());
}

fn legacy_host() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn foreign_target() -> &'static str {
    if cfg!(target_os = "macos") {
        "linux-x86_64-musl"
    } else {
        "macos-aarch64"
    }
}

#[test]
fn real_host_legacy_install_and_lazy_exec_preserve_v1_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let server = server(2);
    let (manifest, lock) =
        seed_project(root, server.url(), None, &[(TOOL, LEGACY), (OTHER, LEGACY)]);

    let output = run(root, &["exec", TOOL]);
    assert_success(&output);
    assert_eq!(output.stdout, b"pinned matrix tool\n");
    assert!(!root.join(".tools").join(OTHER).exists());
    let tool_path = format!("/{TOOL}/{}", legacy_host());
    let other_path = format!("/{OTHER}/{}", legacy_host());
    assert_requests(&server, &[&tool_path]);
    assert_files(root, &manifest, &lock);

    assert_success(&run(root, &["install", "--no-prompt"]));
    assert_eq!(
        fs::read(root.join(".tools/.bin").join(OTHER)).unwrap(),
        SCRIPT
    );
    assert_requests(&server, &[&tool_path, &other_path]);
    assert_files(root, &manifest, &lock);
}

#[test]
fn real_host_mixed_project_uses_exact_target_and_skips_excluded_tool() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let server = server(2);
    let legacy = legacy_host();
    let linux = cfg!(target_os = "linux");
    let gnu = format!("{legacy}-gnu");
    let musl = format!("{legacy}-musl");
    let mut targets = vec![foreign_target(), if linux { &gnu } else { &legacy }];
    if linux {
        targets.push(&musl);
    }
    let (manifest, lock) = seed_project(
        root,
        server.url(),
        Some(&targets),
        &[(TOOL, &targets), (OTHER, &[foreign_target()])],
    );

    // Read the detected host key from the managed-tool error emitted by the binary.
    let output = run(root, &["exec", OTHER]);
    let error = failure(&output);
    let host = error
        .split_once(&format!("tool {OTHER} is not available on "))
        .and_then(|(_, rest)| rest.split_once(';'))
        .map(|(host, _)| host)
        .unwrap_or_else(|| panic!("{error}"));
    assert!(
        if linux {
            host == gnu || host == musl
        } else {
            host == legacy
        },
        "{error}"
    );
    assert!(!root.join(".tools").exists());

    let output = run(root, &["install", "--no-prompt"]);
    assert_success(&output);
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains(&format!("Skipped {OTHER}: not available on {host}"))
    );
    assert!(!root.join(".tools").join(OTHER).exists());
    let path = format!("/{TOOL}/{host}");
    assert_requests(&server, &[&path]);
    assert_files(root, &manifest, &lock);

    fs::remove_dir_all(root.join(".tools")).unwrap();
    let output = run(root, &["exec", TOOL]);
    assert_success(&output);
    assert_eq!(output.stdout, b"pinned matrix tool\n");
    assert_requests(&server, &[&path, &path]);
    assert_files(root, &manifest, &lock);

    if linux {
        let opposite = if host == gnu { &musl } else { &gnu };
        assert_unsupported_host(opposite, Some(host));
    }
}

fn assert_unsupported_host(target: &str, host: Option<&str>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let server = server(1);
    let (manifest, lock) = seed_project(root, server.url(), Some(&[target]), &[(TOOL, &[target])]);
    for args in [&["install", "--no-prompt"][..], &["exec", TOOL][..]] {
        let error = failure(&run(root, args));
        assert!(
            error.contains(&format!(
                "this project does not support {}",
                host.map_or_else(legacy_host, str::to_owned)
            )),
            "{error}"
        );
        assert_files(root, &manifest, &lock);
        assert!(!root.join(".tools").exists());
    }
    fs::remove_file(root.join(LOCK)).unwrap();
    let error = failure(&run(root, &["install", "--no-prompt"]));
    assert!(error.contains("this project does not support"), "{error}");
    assert_eq!(fs::read(root.join(MANIFEST)).unwrap(), manifest.as_bytes());
    assert!(!root.join(LOCK).exists());
    assert!(!root.join(".tools").exists());
    assert_requests(&server, &[]);
}

#[test]
fn real_host_outside_project_targets_leaves_existing_or_missing_lock_alone() {
    assert_unsupported_host(foreign_target(), None);
}

#[test]
fn real_host_target_adoption_or_edit_requires_update() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let server = server(1);
    let host = legacy_host();
    for original_targets in [None, Some(&[host.as_str()][..])] {
        let (manifest, lock) = seed_project(
            root,
            server.url(),
            original_targets,
            &[(TOOL, original_targets.unwrap_or(LEGACY))],
        );
        let (header, tools) = manifest.split_once("\n[tools.").unwrap();
        let header = header
            .split_once("\n[platforms]")
            .map_or(header, |(header, _)| header);
        let changed = format!(
            "{header}\n{}\n[tools.{tools}",
            platform_table(&[&host, foreign_target()])
        );
        fs::write(root.join(MANIFEST), &changed).unwrap();
        for args in [&["install", "--no-prompt"][..], &["exec", TOOL][..]] {
            let error = failure(&run(root, args));
            assert!(error.contains("run `binloom update`"), "{error}");
            assert_files(root, &changed, &lock);
            assert!(!root.join(".tools").exists());
        }
    }
    let error = failure(&run(root, &["migrate"]));
    assert!(
        error.contains("unrecognized subcommand 'migrate'"),
        "{error}"
    );
    assert_requests(&server, &[]);
}

#[test]
fn real_host_install_reports_missing_binloom_bootstrap_key() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let manifest = "manifest-version = 1\n[binloom]\nversion = '1.0.0'\n";
    let lock = "lock-version = 1\n[binloom]\nversion = '1.0.0'\nsource = 'github:KyrboForge/binloom'\ntag = 'v1.0.0'\n";
    fs::write(root.join(MANIFEST), manifest).unwrap();
    fs::write(root.join(LOCK), lock).unwrap();
    let output = run(root, &["install", "--no-prompt"]);
    assert_success(&output);
    let warning = String::from_utf8_lossy(&output.stderr);
    assert!(
        warning.contains(&format!("missing bootstrap artifact for {}", legacy_host())),
        "{warning}"
    );
    assert!(warning.contains("update --self"), "{warning}");
    assert_files(root, manifest, lock);
}
