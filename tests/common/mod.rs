use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output},
    thread,
    time::Duration,
};

pub fn checksum(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn serve(listener: &TcpListener, artifacts: &[(&str, Response<'_>)]) -> Vec<String> {
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

        if let Some((_, response)) = artifacts.iter().find(|(route, _)| *route == path) {
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.content_length
            )
            .unwrap();
            if let Some(before_body) = response.before_body {
                before_body();
            }
            stream.write_all(response.body).unwrap();
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

const UTILITIES: [&str; 20] = [
    "awk",
    "cat",
    "chmod",
    "cp",
    "curl",
    "dirname",
    "find",
    "gzip",
    "head",
    "id",
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

pub fn script(tag: &str) -> Vec<u8> {
    format!(
        "#!/bin/sh\nif [ \"${{BINLOOMW_REEXEC+x}}\" ]; then\n  echo 'wrapper re-exec guard leaked' >&2\n  exit 97\nfi\necho ran:{tag}\n"
    )
    .into_bytes()
}

pub fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

/// An artifact as served for a locked Binloom version: the bytes on the wire and the program they unpack to.
pub struct Artifact {
    pub tag: String,
    pub body: Vec<u8>,
    pub program: Vec<u8>,
    pub format: &'static str,
}

impl Artifact {
    pub fn new(tag: &str, format: &'static str) -> Self {
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

pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

pub fn assert_ran(output: &Output, tag: &str) {
    assert!(output.status.success(), "{}", stderr(output));
    assert_eq!(stdout(output), format!("ran:{tag}\n"));
}

pub fn assert_refused(output: &Output, tag: &str) {
    assert_eq!(output.status.code(), Some(1), "{}", stderr(output));
    assert!(!stdout(output).contains(tag), "{}", stdout(output));
}

pub fn leftover_temp_files(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with(".binloomw-tmp."))
        .collect()
}

pub struct Response<'a> {
    pub body: &'a [u8],
    pub content_length: usize,
    pub before_body: Option<&'a (dyn Fn() + Sync)>,
}

impl<'a> From<&'a [u8]> for Response<'a> {
    fn from(body: &'a [u8]) -> Self {
        Self {
            body,
            content_length: body.len(),
            before_body: None,
        }
    }
}

pub struct TestServer(TcpListener);

impl TestServer {
    pub fn new() -> Self {
        Self(TcpListener::bind("127.0.0.1:0").unwrap())
    }

    pub fn address(&self) -> SocketAddr {
        self.0.local_addr().unwrap()
    }

    pub fn serving<R>(
        &self,
        routes: &[(&str, Response<'_>)],
        run: impl FnOnce() -> R,
    ) -> (R, Vec<String>) {
        // Unblock the server when the test body completes or panics.
        struct Stop(SocketAddr);
        impl Drop for Stop {
            fn drop(&mut self) {
                let _ = TcpStream::connect(self.0);
            }
        }

        thread::scope(|scope| {
            let server = scope.spawn(|| serve(&self.0, routes));
            let stop = Stop(self.address());
            let result = run();
            drop(stop);
            (result, server.join().unwrap())
        })
    }
}

pub fn isolated_path(home: &Path) -> PathBuf {
    let path = home.join("bin");
    fs::create_dir(&path).unwrap();
    for utility in UTILITIES {
        if let Some(executable) = ["/usr/bin", "/bin"]
            .iter()
            .map(|dir| Path::new(dir).join(utility))
            .find(|file| file.is_file())
        {
            symlink(executable, path.join(utility)).unwrap();
        }
    }
    path
}

pub fn write_executable(path: &Path, bytes: impl AsRef<[u8]>) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

pub fn wrapper_source(version: &str) -> String {
    include_str!("../../src/binloomw.sh").replace("@BINLOOM_VERSION@", version)
}

pub fn wrapper_command(root: &Path, home: &Path, shell: Option<&Path>) -> Command {
    let wrapper = root.join("binloomw");
    let mut command = Command::new(shell.unwrap_or(&wrapper));
    if shell.is_some() {
        command.arg(wrapper);
    }
    command
        .current_dir(root)
        .env_clear()
        .env("PATH", home.join("bin"))
        .env("HOME", home);
    command
}

pub fn binloom_lock(version: &str) -> String {
    format!(
        "lock-version = 1\n[binloom]\nversion = \"{version}\"\nsource = \"github:KyrboForge/binloom\"\ntag = \"v{version}\"\n"
    )
}

pub fn artifact_section(key: &str, url: &str, sha: &str, format: &str) -> String {
    let asset = url.rsplit('/').next().unwrap();
    format!(
        "\n[{key}]\nasset = \"{asset}\"\nurl = \"{url}\"\nsha256 = \"{sha}\"\nformat = \"{format}\"\nchecksum-source = \"digest\"\n"
    )
}
