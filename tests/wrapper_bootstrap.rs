#![cfg(unix)]

use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
    process::Command,
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

fn serve(listener: TcpListener, artifacts: &[(&str, &[u8])]) -> Vec<String> {
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
    for utility in [
        "awk",
        "cat",
        "chmod",
        "cp",
        "curl",
        "dirname",
        "mkdir",
        "mktemp",
        "mv",
        "rm",
        "sha256sum",
        "shasum",
        "uname",
        "wget",
    ] {
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
        let server = scope.spawn(|| serve(listener, &artifacts));
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
