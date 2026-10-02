use super::update;
use crate::{
    common::{LOCKFILE, MANIFEST, TOOLS_DIR, project_root},
    domain::{
        lockfile::{ArtifactFormat, LockedTool, Lockfile},
        manifest::Manifest,
        platform::Platform,
        sources::{CargoSource, Source},
    },
    download,
};
use anyhow::{Context, Result, bail, ensure};
use flate2::read::GzDecoder;
use std::{
    fs,
    io::{self, Read, Seek, Write},
    path::{Component, Path},
    process::{Command, Stdio},
};
use tar::{Archive, EntryType};

pub(crate) fn install() -> Result<()> {
    let root = project_root()?;
    let lock_path = root.join(LOCKFILE);

    if !lock_path
        .try_exists()
        .context("failed to check binloom.lock")?
    {
        update::lock()?;
    }

    let client = download::client();

    install_from(&root, &client)
}

pub(super) fn install_from(root: &Path, client: &download::Client) -> Result<()> {
    let manifest_path = root.join(MANIFEST);
    let lock_path = root.join(LOCKFILE);

    let manifest = Manifest::try_from(manifest_path.as_path())?;
    let lockfile = Lockfile::try_from(lock_path.as_path())?;

    manifest.ensure_matches_lockfile(&lockfile)?;
    ensure!(!lockfile.tools.is_empty(), "no tools in binloom.lock");

    let platform = Platform::current()?;
    let platform_key = platform.to_string();

    for (name, tool) in &lockfile.tools {
        let source = Source::try_from(tool.source.clone()).map_err(anyhow::Error::msg)?;

        if let Source::Cargo(source) = source {
            install_cargo(root, name, tool, &source, client)?;
            continue;
        }
        let artifact = tool
            .artifacts
            .get(&platform_key)
            .with_context(|| format!("tool {name} has no artifact for {platform}"))?;

        let directory = root.join(TOOLS_DIR).join(name).join(&tool.version);

        fs::create_dir_all(&directory)
            .with_context(|| format!("failed to create {}", directory.display()))?;

        let destination = directory.join(name);
        let checksum_stamp = directory.join(".artifact-sha256");

        if cached_artifact_matches(&destination, &checksum_stamp, &artifact.sha256)? {
            println!("Already installed {name} {}", tool.version);
            link_tool(root, name, &tool.version)?;
            continue;
        }

        let mut downloaded = tempfile::NamedTempFile::new_in(&directory)?;

        let actual_sha256 = download::download_to(client, &artifact.url, &mut downloaded)?;

        ensure!(
            actual_sha256 == artifact.sha256,
            "checksum mismatch for {name}: expected {}, got {actual_sha256}",
            artifact.sha256
        );

        let mut executable = tempfile::NamedTempFile::new_in(&directory)?;
        let downloaded_file = downloaded.reopen()?;

        unpack(
            artifact.format,
            name,
            downloaded_file,
            executable.as_file_mut(),
        )
        .with_context(|| format!("failed to unpack {}", artifact.asset))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            executable
                .as_file()
                .set_permissions(fs::Permissions::from_mode(0o755))?;
        }

        executable.as_file().sync_all()?;

        executable
            .persist(&destination)
            .map_err(|error| error.error)
            .with_context(|| format!("failed to install {}", destination.display()))?;

        fs::write(&checksum_stamp, format!("{}\n", artifact.sha256))
            .with_context(|| format!("failed to write {}", checksum_stamp.display()))?;

        link_tool(root, name, &tool.version)?;

        println!("Installed {name} {}", tool.version);
    }

    Ok(())
}

fn install_cargo(
    root: &Path,
    name: &str,
    tool: &LockedTool,
    source: &CargoSource,
    client: &download::Client,
) -> Result<()> {
    let expected_sha256 = tool
        .sha256
        .as_deref()
        .context("Cargo lock entry is missing SHA-256")?;

    let directory = root.join(TOOLS_DIR).join(name).join(&tool.version);
    let destination = directory.join(name);
    let checksum_stamp = directory.join(".crate-sha256");

    fs::create_dir_all(&directory)
        .with_context(|| format!("failed to create {}", directory.display()))?;

    if cached_artifact_matches(&destination, &checksum_stamp, expected_sha256)? {
        println!("Already installed {name} {}", tool.version);
        link_tool(root, name, &tool.version)?;
        return Ok(());
    }

    ensure_cargo_toolchain()?;

    let mut archive = tempfile::NamedTempFile::new_in(&directory)?;
    let actual_sha256 =
        download::download_to(client, &source.download_url(&tool.version), &mut archive)?;

    ensure!(
        actual_sha256 == expected_sha256,
        "checksum mismatch for {name}: expected {expected_sha256}, got {actual_sha256}"
    );

    let extracted = tempfile::tempdir_in(&directory)?;
    let archive_file = archive.reopen()?;

    tar::Archive::new(GzDecoder::new(archive_file))
        .unpack(extracted.path())
        .context("failed to unpack Cargo package")?;

    let package_root = extracted
        .path()
        .join(format!("{}-{}", source.package(), tool.version));

    ensure!(
        package_root.join("Cargo.toml").is_file(),
        "Cargo package archive has no expected Cargo.toml"
    );

    let install_root = tempfile::tempdir_in(&directory)?;

    let status = Command::new("cargo")
        .arg("install")
        .arg("--path")
        .arg(&package_root)
        .arg("--locked")
        .arg("--root")
        .arg(install_root.path())
        .status()
        .context("failed to run cargo install")?;

    ensure!(
        status.success(),
        "cargo install failed for {}@{}",
        source.package(),
        tool.version
    );

    let installed = install_root.path().join("bin").join(name);

    ensure!(
        installed.is_file(),
        "Cargo package {} did not install binary {name}",
        source.package()
    );

    fs::rename(&installed, &destination)
        .with_context(|| format!("failed to install {}", destination.display()))?;

    fs::write(&checksum_stamp, format!("{expected_sha256}\n"))
        .with_context(|| format!("failed to write {}", checksum_stamp.display()))?;

    link_tool(root, name, &tool.version)?;

    println!("Installed {name} {}", tool.version);

    Ok(())
}
fn command_available(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn ensure_cargo_toolchain() -> Result<()> {
    ensure!(
        command_available("cargo") && command_available("rustc"),
        "Cargo sources require cargo and rustc; install a Rust toolchain or use a prebuilt GitHub/GitLab release"
    );

    Ok(())
}

fn cached_artifact_matches(
    destination: &Path,
    checksum_stamp: &Path,
    expected_sha256: &str,
) -> Result<bool> {
    if !destination.is_file() {
        return Ok(false);
    }

    let installed_sha256 = match fs::read_to_string(checksum_stamp) {
        Ok(checksum) => checksum,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read {}", checksum_stamp.display()));
        }
    };

    Ok(installed_sha256.trim() == expected_sha256)
}

fn unpack(
    format: ArtifactFormat,
    name: &str,
    mut source: impl Read + Seek,
    mut destination: impl Write,
) -> Result<()> {
    match format {
        ArtifactFormat::Raw => io::copy(&mut source, &mut destination)?,
        ArtifactFormat::Gz => io::copy(&mut GzDecoder::new(source), &mut destination)?,
        ArtifactFormat::TarGz => {
            let index = select_tar_entry(&mut source, name)?;

            source.rewind()?;

            let mut archive = Archive::new(GzDecoder::new(source));
            let mut entry = archive
                .entries()?
                .nth(index)
                .context("archive entry disappeared")??;

            io::copy(&mut entry, &mut destination)?
        }
    };

    Ok(())
}

/// Validates every archive entry and picks the executable without touching the
/// filesystem: a regular file named after the tool, or the archive's only file.
fn select_tar_entry(source: impl Read, name: &str) -> Result<usize> {
    let mut archive = Archive::new(GzDecoder::new(source));
    let mut files = Vec::new();
    let mut matches = Vec::new();

    for (index, entry) in archive.entries()?.enumerate() {
        let entry = entry?;
        let path = entry.path()?;

        ensure!(
            path.components()
                .all(|component| matches!(component, Component::Normal(_) | Component::CurDir)),
            "unsafe archive entry: {}",
            path.display()
        );

        match entry.header().entry_type() {
            EntryType::Regular => {
                if path.file_name() == Some(name.as_ref()) {
                    matches.push(index);
                }

                files.push(index);
            }
            EntryType::Directory | EntryType::XGlobalHeader => {}
            other => bail!("unsupported archive entry {}: {other:?}", path.display()),
        }
    }

    match (matches.as_slice(), files.as_slice()) {
        ([index], _) | ([], [index]) => Ok(*index),
        ([], _) => bail!("archive has no file named {name}"),
        _ => bail!("archive has multiple files named {name}"),
    }
}

#[cfg(unix)]
fn link_tool(root: &Path, name: &str, version: &str) -> Result<()> {
    let bin_directory = root.join(TOOLS_DIR).join(".bin");

    fs::create_dir_all(&bin_directory).context("failed to create .tools/.bin")?;

    let link = bin_directory.join(name);
    let target = Path::new("..").join(name).join(version).join(name);

    let temporary = tempfile::tempdir_in(&bin_directory)
        .context("failed to create temporary link directory")?;
    let temporary_link = temporary.path().join(name);

    std::os::unix::fs::symlink(&target, &temporary_link)
        .with_context(|| format!("failed to create temporary link for {name}"))?;

    fs::rename(&temporary_link, &link)
        .with_context(|| format!("failed to replace {}", link.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::lockfile::{ChecksumSource, LockedArtifact, LockedTool},
        http_fixture::{Response, Server},
    };
    use flate2::{Compression, write::GzEncoder};
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;

    fn checksum(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn write_project(root: &Path, url: &str, sha256: &str) {
        fs::write(
            root.join("binloom.toml"),
            r#"manifest-version = 1

[binloom]
version = "0.2.2"

[tools.tool]
version = "1.0.0"
source = "github:owner/tool"
"#,
        )
        .unwrap();

        let platform = Platform::current().unwrap().to_string();
        let mut lockfile = Lockfile::default();

        lockfile.tools.insert(
            "tool".to_owned(),
            LockedTool {
                version: "1.0.0".to_owned(),
                source: "github:owner/tool".to_owned(),
                tag: Some("v1.0.0".to_owned()),
                sha256: None,
                artifacts: BTreeMap::from([(
                    platform,
                    LockedArtifact {
                        asset: "tool".to_owned(),
                        url: url.to_owned(),
                        sha256: sha256.to_owned(),
                        format: ArtifactFormat::Raw,
                        checksum_source: ChecksumSource::Download,
                    },
                )]),
            },
        );

        lockfile.write(root.join("binloom.lock").as_path()).unwrap();
    }

    #[test]
    fn unpacks_raw_and_gzip() {
        let input = b"hello";

        let mut raw = Vec::new();
        unpack(
            ArtifactFormat::Raw,
            "tool",
            io::Cursor::new(input),
            &mut raw,
        )
        .unwrap();
        assert_eq!(raw, input);

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(input).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut gzip = Vec::new();
        unpack(
            ArtifactFormat::Gz,
            "tool",
            io::Cursor::new(&compressed),
            &mut gzip,
        )
        .unwrap();
        assert_eq!(gzip, input);
    }

    fn tar_gz(entries: &[(&str, EntryType, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));

        for (path, kind, data) in entries {
            // Raw header names bypass the builder's path validation so tests can
            // produce the malicious archives that real attackers would.
            let mut header = tar::Header::new_gnu();
            header.as_gnu_mut().unwrap().name[..path.len()].copy_from_slice(path.as_bytes());
            header.set_entry_type(*kind);
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append(&header, *data).unwrap();
        }

        builder.into_inner().unwrap().finish().unwrap()
    }

    fn unpack_tar(entries: &[(&str, EntryType, &[u8])]) -> Result<Vec<u8>> {
        let mut output = Vec::new();
        unpack(
            ArtifactFormat::TarGz,
            "tool",
            io::Cursor::new(tar_gz(entries)),
            &mut output,
        )?;
        Ok(output)
    }

    #[test]
    fn unpacks_tool_from_tar_gz() {
        assert_eq!(
            unpack_tar(&[("tool", EntryType::Regular, b"only")]).unwrap(),
            b"only"
        );
        assert_eq!(
            unpack_tar(&[("other", EntryType::Regular, b"single")]).unwrap(),
            b"single"
        );
        assert_eq!(
            unpack_tar(&[
                ("./dist/", EntryType::Directory, b""),
                ("./dist/README.md", EntryType::Regular, b"docs"),
                ("./dist/tool", EntryType::Regular, b"binary"),
                ("./dist/LICENSE", EntryType::Regular, b"license"),
            ])
            .unwrap(),
            b"binary"
        );
    }

    #[test]
    fn rejects_unselectable_tar_gz() {
        let error = unpack_tar(&[
            ("README.md", EntryType::Regular, b"docs"),
            ("LICENSE", EntryType::Regular, b"license"),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("no file named tool"));

        let error = unpack_tar(&[
            ("a/tool", EntryType::Regular, b"one"),
            ("b/tool", EntryType::Regular, b"two"),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("multiple files named tool"));
    }

    #[test]
    fn rejects_malicious_tar_gz_entries() {
        for entries in [
            &[("../tool", EntryType::Regular, &b"x"[..])][..],
            &[("dist/../../tool", EntryType::Regular, b"x")],
            &[("/usr/bin/tool", EntryType::Regular, b"x")],
            &[("tool", EntryType::Symlink, b"")],
            &[("tool", EntryType::Link, b"")],
            &[("tool", EntryType::Char, b"")],
            &[
                ("tool", EntryType::Regular, b"binary"),
                ("../escape", EntryType::Regular, b"x"),
            ],
        ] {
            assert!(unpack_tar(entries).is_err(), "accepted {entries:?}");
        }
    }

    #[test]
    fn reuses_cache_only_with_matching_checksum_stamp() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("tool");
        let checksum_stamp = directory.path().join(".artifact-sha256");

        assert!(!cached_artifact_matches(&destination, &checksum_stamp, "expected").unwrap());

        std::fs::write(&destination, "binary").unwrap();

        assert!(!cached_artifact_matches(&destination, &checksum_stamp, "expected").unwrap());

        std::fs::write(&checksum_stamp, "different\n").unwrap();

        assert!(!cached_artifact_matches(&destination, &checksum_stamp, "expected").unwrap());

        std::fs::write(&checksum_stamp, "expected\n").unwrap();

        assert!(cached_artifact_matches(&destination, &checksum_stamp, "expected").unwrap());
    }

    #[test]
    fn installs_tool_from_http() {
        let directory = tempfile::tempdir().unwrap();
        let binary = b"new tool";
        let server = Server::start(vec![Response {
            status: 200,
            body: binary.to_vec(),
        }]);

        write_project(
            directory.path(),
            &format!("{}/tool", server.url()),
            &checksum(binary),
        );

        let client = download::client();

        install_from(directory.path(), &client).unwrap();

        let installed = directory.path().join(".tools/tool/1.0.0/tool");

        assert_eq!(fs::read(installed).unwrap(), binary);
        assert_eq!(
            fs::read_to_string(directory.path().join(".tools/tool/1.0.0/.artifact-sha256"))
                .unwrap(),
            format!("{}\n", checksum(binary))
        );
        assert!(directory.path().join(".tools/.bin/tool").exists());
        assert_eq!(server.requests()[0].path, "/tool");
    }

    #[test]
    fn rejects_download_with_wrong_checksum() {
        let directory = tempfile::tempdir().unwrap();
        let server = Server::start(vec![Response {
            status: 200,
            body: b"tampered".to_vec(),
        }]);

        write_project(
            directory.path(),
            &format!("{}/tool", server.url()),
            &"0".repeat(64),
        );

        let client = download::client();
        let error = install_from(directory.path(), &client).unwrap_err();

        assert!(error.to_string().contains("checksum mismatch for tool"));
        assert!(!directory.path().join(".tools/tool/1.0.0/tool").exists());
    }

    #[test]
    fn replaces_install_with_stale_checksum_stamp() {
        let directory = tempfile::tempdir().unwrap();
        let binary = b"fresh tool";
        let server = Server::start(vec![Response {
            status: 200,
            body: binary.to_vec(),
        }]);

        write_project(
            directory.path(),
            &format!("{}/tool", server.url()),
            &checksum(binary),
        );

        let install_directory = directory.path().join(".tools/tool/1.0.0");
        fs::create_dir_all(&install_directory).unwrap();
        fs::write(install_directory.join("tool"), b"stale tool").unwrap();
        fs::write(
            install_directory.join(".artifact-sha256"),
            format!("{}\n", "0".repeat(64)),
        )
        .unwrap();

        let client = download::client();

        install_from(directory.path(), &client).unwrap();

        assert_eq!(fs::read(install_directory.join("tool")).unwrap(), binary);
        assert_eq!(
            fs::read_to_string(install_directory.join(".artifact-sha256")).unwrap(),
            format!("{}\n", checksum(binary))
        );
        assert_eq!(server.requests().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn replaces_existing_tool_link() {
        let directory = tempfile::tempdir().unwrap();

        link_tool(directory.path(), "tool", "1.0.0").unwrap();
        link_tool(directory.path(), "tool", "2.0.0").unwrap();

        assert_eq!(
            fs::read_link(directory.path().join(".tools/.bin/tool")).unwrap(),
            Path::new("../tool/2.0.0/tool")
        );
    }

    #[test]
    fn detects_missing_commands() {
        assert!(!command_available(
            "binloom-command-that-definitely-does-not-exist"
        ));
    }
}
