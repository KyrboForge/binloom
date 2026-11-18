use super::update;
use crate::{
    common::{LOCKFILE, MANIFEST, TOOLS_DIR, project_root},
    domain::{
        lockfile::{ArtifactFormat, LockedArtifact, LockedTool, Lockfile},
        manifest::Manifest,
        platform::Platform,
        sources::{CargoSource, Source},
    },
    download,
};
use anyhow::{Context, Result, bail, ensure};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    io::{self, Read, Seek, Write},
    path::{Component, Path},
    process::{Command, Stdio},
};
use tar::{Archive, EntryType};

const RETAINED_ARTIFACT: &str = ".artifact";
const ARTIFACT_SHA256: &str = ".artifact-sha256";
const CRATE_SHA256: &str = ".crate-sha256";
const EXECUTABLE_SHA256: &str = ".executable-sha256";

pub(crate) fn install_with_prompt(no_prompt: bool) -> Result<()> {
    install()?;
    super::shell::maybe_prompt(&project_root()?, no_prompt)
}

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

    for name in lockfile.tools.keys() {
        ensure_unreserved_tool_name(name)?;
    }
    ensure!(
        !lockfile.tools.is_empty() || lockfile.binloom.is_some(),
        "no tools in binloom.lock"
    );

    if let Some(binloom) = &lockfile.binloom {
        let cached = root
            .join(TOOLS_DIR)
            .join("binloom")
            .join(&binloom.version)
            .join("binloom");
        let link = root.join(TOOLS_DIR).join(".bin/binloom");
        if !cached.is_file()
            && let (Ok(executable), Ok(link_target)) = (
                std::env::current_exe().and_then(|path| path.canonicalize()),
                link.canonicalize(),
            )
            && executable == link_target
        {
            bail!("locked Binloom version is not cached; run ./binloomw install");
        }
    }

    let platform = Platform::current()?;
    let platform_key = platform.to_string();

    for (name, tool) in &lockfile.tools {
        ensure_tool_installed(
            root,
            name,
            tool,
            &platform_key,
            client,
            InstallOutput::Stdout,
        )?;
    }

    if let Some(binloom) = &lockfile.binloom {
        let directory = root.join(TOOLS_DIR).join("binloom").join(&binloom.version);
        let cached = directory.join("binloom");
        let matches = match binloom.artifacts.get(&platform_key) {
            // The POSIX wrapper owns the Binloom bootstrap cache and its stamp format.
            Some(artifact) => {
                cached.is_file()
                    && checksum_matches(&directory.join(ARTIFACT_SHA256), &artifact.sha256)?
            }
            None => false,
        };
        if matches {
            link_tool(root, "binloom", &binloom.version)?;
        } else {
            // A link left by an older lock would keep the wrong Binloom active.
            let link = root.join(TOOLS_DIR).join(".bin/binloom");
            if link.symlink_metadata().is_ok() {
                fs::remove_file(&link)
                    .with_context(|| format!("failed to remove {}", link.display()))?;
            }
            eprintln!(
                "warning: Binloom {} from binloom.lock is not cached; run ./binloomw install",
                binloom.version
            );
        }
    }

    Ok(())
}

pub(super) fn ensure_unreserved_tool_name(name: &str) -> Result<()> {
    // Case-insensitive filesystems would alias `.tools/.bin/Binloom` to the manager link.
    ensure!(
        !name.eq_ignore_ascii_case("binloom"),
        "tool name binloom is reserved for the project-locked Binloom binary"
    );
    Ok(())
}

#[derive(Clone, Copy)]
pub(super) enum InstallOutput {
    Stdout,
    Stderr,
}

pub(super) fn ensure_tool_installed(
    root: &Path,
    name: &str,
    tool: &LockedTool,
    platform_key: &str,
    client: &download::Client,
    output: InstallOutput,
) -> Result<()> {
    let source = Source::try_from(tool.source.clone()).map_err(anyhow::Error::msg)?;
    let version = &tool.version;

    let cached = match source {
        Source::Cargo(source) => {
            let sha256 = tool
                .sha256
                .as_deref()
                .context("Cargo lock entry is missing SHA-256")?;
            ensure_cached(
                root,
                name,
                version,
                CRATE_SHA256,
                sha256,
                None,
                |directory, destination, stamp| {
                    let install_root =
                        install_cargo(directory, name, version, &source, sha256, client, output)?;
                    let staged = install_root.path().join("bin").join(name);
                    persist_executable(&staged, destination, stamp, sha256, None)
                },
            )?
        }
        Source::GitHub(_) | Source::GitLab(_) => {
            let artifact = tool
                .artifacts
                .get(platform_key)
                .with_context(|| format!("tool {name} has no artifact for {platform_key}"))?;
            ensure_cached(
                root,
                name,
                version,
                ARTIFACT_SHA256,
                &artifact.sha256,
                Some(artifact.format),
                |directory, destination, stamp| {
                    install_artifact(directory, name, artifact, client, destination, stamp)
                },
            )?
        }
    };

    match (output, cached) {
        (InstallOutput::Stdout, true) => println!("Already installed {name} {version}"),
        (InstallOutput::Stdout, false) => println!("Installed {name} {version}"),
        // exec stays quiet on cache hits so repeated runs add no noise.
        (InstallOutput::Stderr, false) => eprintln!("Installed {name} {version}"),
        (InstallOutput::Stderr, true) => {}
    }
    Ok(())
}

fn ensure_cached(
    root: &Path,
    name: &str,
    version: &str,
    checksum_file: &str,
    expected_sha256: &str,
    format: Option<ArtifactFormat>,
    install: impl FnOnce(&Path, &Path, &Path) -> Result<()>,
) -> Result<bool> {
    let tools_directory = root.join(TOOLS_DIR);
    let tool_directory = tools_directory.join(name);
    let directory = tool_directory.join(version);
    // Check each component before creating any children beneath it.
    for path in [&tools_directory, &tool_directory, &directory] {
        create_cache_directory(path)?;
    }
    let lock_path = directory.join(".install.lock");
    check_cache_type(&lock_path, fs::Metadata::is_file)?;
    let lock = fs::OpenOptions::new()
        // On macOS and Linux, opening both ends of a FIFO does not wait for a peer.
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("failed to open {}", lock_path.display()))?;
    ensure!(
        lock.metadata()?.is_file(),
        "expected regular file at {}",
        lock_path.display()
    );
    lock.lock()
        .with_context(|| format!("failed to lock {}", lock_path.display()))?;

    let destination = directory.join(name);
    let checksum_stamp = directory.join(checksum_file);
    let cached =
        cached_executable_matches(&destination, &checksum_stamp, expected_sha256, format, name)?;
    if !cached {
        install(&directory, &destination, &checksum_stamp).with_context(|| {
            format!("failed to install tool {name} at {}", destination.display())
        })?;
    }
    link_tool(root, name, version)?;
    Ok(cached)
}

fn create_cache_directory(path: &Path) -> Result<()> {
    let created = fs::create_dir_all(path);
    check_cache_type(path, fs::Metadata::is_dir)?;
    created.with_context(|| format!("failed to create {}", path.display()))
}

fn check_cache_type(path: &Path, expected: fn(&fs::Metadata) -> bool) -> Result<()> {
    match path.symlink_metadata() {
        Ok(metadata) => ensure!(
            expected(&metadata),
            "refusing symlink or unexpected file type at {}",
            path.display()
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    }
    Ok(())
}

fn install_artifact(
    directory: &Path,
    name: &str,
    artifact: &LockedArtifact,
    client: &download::Client,
    destination: &Path,
    stamp: &Path,
) -> Result<()> {
    let mut downloaded = tempfile::NamedTempFile::new_in(directory)?;
    let actual_sha256 = download::download_to(client, &artifact.url, &mut downloaded)?;
    ensure!(
        actual_sha256 == artifact.sha256,
        "checksum mismatch for {name}: expected {}, got {actual_sha256}",
        artifact.sha256
    );

    let mut executable = tempfile::NamedTempFile::new_in(directory)?;
    unpack(
        artifact.format,
        name,
        downloaded.reopen()?,
        executable.as_file_mut(),
    )
    .with_context(|| format!("failed to unpack {}", artifact.asset))?;

    #[cfg(unix)]
    executable
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o755))?;

    let retained = (!matches!(artifact.format, ArtifactFormat::Raw)).then_some(downloaded);
    persist_executable(
        executable.path(),
        destination,
        stamp,
        &artifact.sha256,
        retained,
    )
}

fn install_cargo(
    directory: &Path,
    name: &str,
    version: &str,
    source: &CargoSource,
    expected_sha256: &str,
    client: &download::Client,
    output: InstallOutput,
) -> Result<tempfile::TempDir> {
    ensure_cargo_toolchain()?;

    let mut archive = tempfile::NamedTempFile::new_in(directory)?;
    let actual_sha256 = download::download_to(client, &source.download_url(version), &mut archive)?;
    ensure!(
        actual_sha256 == expected_sha256,
        "checksum mismatch for {name}: expected {expected_sha256}, got {actual_sha256}"
    );

    let extracted = tempfile::tempdir_in(directory)?;
    tar::Archive::new(GzDecoder::new(archive.reopen()?))
        .unpack(extracted.path())
        .context("failed to unpack Cargo package")?;

    let package_root = extracted
        .path()
        .join(format!("{}-{version}", source.package()));
    ensure!(
        package_root.join("Cargo.toml").is_file(),
        "Cargo package archive has no expected Cargo.toml"
    );

    let install_root = tempfile::tempdir_in(directory)?;
    let mut command = Command::new("cargo");
    command
        .arg("install")
        .arg("--path")
        .arg(&package_root)
        .arg("--locked")
        .arg("--root")
        .arg(install_root.path());
    if let InstallOutput::Stderr = output {
        command.stdout(io::stderr());
    }
    let status = command.status().context("failed to run cargo install")?;
    ensure!(
        status.success(),
        "cargo install failed for {}@{version}",
        source.package()
    );

    let installed = install_root.path().join("bin").join(name);
    ensure!(
        installed
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.is_file()),
        "Cargo package {} did not install binary {name}",
        source.package()
    );

    Ok(install_root)
}

fn persist_executable(
    staged: &Path,
    destination: &Path,
    checksum_stamp: &Path,
    expected_sha256: &str,
    archive: Option<tempfile::NamedTempFile>,
) -> Result<()> {
    let mut executable = fs::File::open(staged)?;
    let sha256 = sha256_reader(&mut executable)?;
    executable.sync_all()?;

    let executable_stamp = checksum_stamp.with_file_name(EXECUTABLE_SHA256);
    let directory = checksum_stamp
        .parent()
        .context("missing install directory")?;
    let mut source_checksum = tempfile::NamedTempFile::new_in(directory)?;
    let mut executable_checksum = tempfile::NamedTempFile::new_in(directory)?;
    writeln!(source_checksum, "{expected_sha256}")?;
    writeln!(executable_checksum, "{sha256}")?;
    source_checksum.as_file().sync_all()?;
    executable_checksum.as_file().sync_all()?;

    // Invalidate all stamps before replacement so a previous source cannot
    // validate against the new executable stamp.
    let retained = directory.join(RETAINED_ARTIFACT);
    for path in [
        &executable_stamp,
        &directory.join(ARTIFACT_SHA256),
        &directory.join(CRATE_SHA256),
        &retained,
    ] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("failed to remove {}", path.display()));
            }
        }
    }
    if let Some(archive) = archive {
        archive
            .persist(&retained)
            .with_context(|| format!("failed to retain {}", retained.display()))?;
    }
    fs::rename(staged, destination)?;
    for (temporary, stamp) in [
        (source_checksum, checksum_stamp),
        (executable_checksum, executable_stamp.as_path()),
    ] {
        temporary
            .persist(stamp)
            .map_err(|error| error.error)
            .with_context(|| format!("failed to write {}", stamp.display()))?;
    }

    Ok(())
}

#[derive(Default)]
struct Sha256Writer(Sha256);

impl Write for Sha256Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Sha256Writer {
    fn finish(self) -> String {
        self.0
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

fn sha256_reader(mut reader: impl Read) -> io::Result<String> {
    let mut digest = Sha256Writer::default();
    io::copy(&mut reader, &mut digest)?;
    Ok(digest.finish())
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

fn cached_file_metadata(path: &Path) -> Result<Option<fs::Metadata>> {
    match path.symlink_metadata() {
        Ok(metadata) => Ok(metadata.is_file().then_some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {}", path.display())),
    }
}

fn cached_executable_matches(
    destination: &Path,
    checksum_stamp: &Path,
    expected_sha256: &str,
    format: Option<ArtifactFormat>,
    name: &str,
) -> Result<bool> {
    let Some(metadata) = cached_file_metadata(destination)? else {
        return Ok(false);
    };
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o111 == 0 {
        return Ok(false);
    }
    if !checksum_matches(checksum_stamp, expected_sha256)? {
        return Ok(false);
    }

    let executable_stamp = checksum_stamp.with_file_name(EXECUTABLE_SHA256);
    let Some(expected) = read_checksum(&executable_stamp)? else {
        return Ok(false);
    };
    let actual = fs::File::open(destination)
        .and_then(sha256_reader)
        .with_context(|| format!("failed to hash {}", destination.display()))?;
    if actual != expected.trim() {
        return Ok(false);
    }
    match format {
        None => Ok(true), // Cargo builds use their locally recorded executable digest.
        Some(ArtifactFormat::Raw) => Ok(actual == expected_sha256),
        Some(format) => {
            let path = checksum_stamp.with_file_name(RETAINED_ARTIFACT);
            if cached_file_metadata(&path)?.is_none() {
                return Ok(false);
            }
            let mut archive = fs::File::open(&path)
                .with_context(|| format!("failed to open {}", path.display()))?;
            if sha256_reader(&mut archive)? != expected_sha256 {
                return Ok(false);
            }
            archive.rewind()?;
            let mut digest = Sha256Writer::default();
            unpack(format, name, archive, &mut digest)
                .with_context(|| format!("failed to unpack {}", path.display()))?;
            Ok(actual == digest.finish())
        }
    }
}

fn checksum_matches(stamp: &Path, expected_sha256: &str) -> Result<bool> {
    Ok(read_checksum(stamp)?.is_some_and(|checksum| checksum.trim() == expected_sha256))
}

fn read_checksum(stamp: &Path) -> Result<Option<String>> {
    if cached_file_metadata(stamp)?.is_none() {
        return Ok(None);
    }
    match fs::read_to_string(stamp) {
        Ok(checksum) => Ok(Some(checksum)),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::InvalidData
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error).with_context(|| format!("failed to read {}", stamp.display())),
    }
}

const MAX_DECOMPRESSED_BYTES: u64 = 512 * 1024 * 1024;

fn unpack(
    format: ArtifactFormat,
    name: &str,
    source: impl Read + Seek,
    destination: impl Write,
) -> Result<()> {
    unpack_with_limit(format, name, source, destination, MAX_DECOMPRESSED_BYTES)
}

fn unpack_with_limit(
    format: ArtifactFormat,
    name: &str,
    mut source: impl Read + Seek,
    mut destination: impl Write,
    max_bytes: u64,
) -> Result<()> {
    match format {
        ArtifactFormat::Raw => {
            io::copy(&mut source, &mut destination)?;
        }
        ArtifactFormat::Gz => {
            copy_limited(GzDecoder::new(source), &mut destination, max_bytes)?;
        }
        ArtifactFormat::Zip => {
            let mut archive = zip::ZipArchive::new(source)?;
            let index = select_zip_entry(&mut archive, name)?;
            copy_limited(archive.by_index(index)?, &mut destination, max_bytes)?;
        }
        ArtifactFormat::TarGz
        | ArtifactFormat::TarXz
        | ArtifactFormat::TarZst
        | ArtifactFormat::TarBz2 => {
            let index = select_tar_entry(tar_decoder(format, &mut source)?, name, max_bytes)?;
            source.rewind()?;
            let mut archive =
                Archive::new(LimitedReader::new(tar_decoder(format, source)?, max_bytes));
            let entry = archive
                .entries()?
                .nth(index)
                .context("archive entry disappeared")??;
            copy_limited(entry, &mut destination, max_bytes)?;
        }
    }
    Ok(())
}

fn tar_decoder<'a>(format: ArtifactFormat, source: impl Read + 'a) -> Result<Box<dyn Read + 'a>> {
    Ok(match format {
        ArtifactFormat::TarGz => Box::new(GzDecoder::new(source)),
        ArtifactFormat::TarXz => Box::new(lzma_rust2::XzReader::new_mem_limit(
            source,
            true,
            (MAX_DECOMPRESSED_BYTES / 1024) as u32,
        )),
        ArtifactFormat::TarZst => Box::new(
            ruzstd::decoding::StreamingDecoder::new_with_max_window_size(
                source,
                MAX_DECOMPRESSED_BYTES,
            )?,
        ),
        ArtifactFormat::TarBz2 => Box::new(bzip2::read::MultiBzDecoder::new(source)),
        _ => unreachable!("expected a compressed tar format"),
    })
}

struct LimitedReader<R> {
    source: R,
    remaining: u64,
    max_bytes: u64,
}

impl<R> LimitedReader<R> {
    fn new(source: R, max_bytes: u64) -> Self {
        Self {
            source,
            remaining: max_bytes,
            max_bytes,
        }
    }
}

impl<R: Read> Read for LimitedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            return match self.source.read(&mut [0])? {
                0 => Ok(0),
                _ => Err(io::Error::other(format!(
                    "decompressed archive exceeds size limit of {} bytes",
                    self.max_bytes
                ))),
            };
        }
        let length = buf
            .len()
            .min(self.remaining.min(usize::MAX as u64) as usize);
        let read = self.source.read(&mut buf[..length])?;
        self.remaining -= read as u64;
        Ok(read)
    }
}

fn copy_limited(mut source: impl Read, destination: &mut impl Write, max_bytes: u64) -> Result<()> {
    io::copy(&mut source.by_ref().take(max_bytes), destination)?;
    ensure!(
        source.read(&mut [0])? == 0,
        "decompressed entry exceeds size limit of {max_bytes} bytes"
    );
    Ok(())
}

fn select_zip_entry<R: Read + Seek>(archive: &mut zip::ZipArchive<R>, name: &str) -> Result<usize> {
    let mut files = Vec::new();
    let mut matches = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index)?;
        let entry_name = entry.name()?;
        let path = Path::new(entry_name.as_ref());
        ensure!(
            entry.enclosed_name().is_some()
                && path
                    .components()
                    .all(|component| matches!(component, Component::Normal(_) | Component::CurDir)),
            "unsafe archive entry: {}",
            path.display()
        );
        let kind = entry.unix_mode().unwrap_or(0) & 0o170000;
        ensure!(
            !entry.is_symlink() && matches!(kind, 0 | 0o100000 | 0o040000),
            "unsupported archive entry {}: mode {kind:o}",
            path.display()
        );
        if entry.is_file() {
            if path.file_name() == Some(name.as_ref()) {
                matches.push(index);
            }
            files.push(index);
        }
    }
    match (matches.as_slice(), files.as_slice()) {
        ([index], _) | ([], [index]) => Ok(*index),
        ([], _) => bail!("archive has no file named {name}"),
        _ => bail!("archive has multiple files named {name}"),
    }
}

/// Validates every archive entry and picks the executable without touching the
/// filesystem: a regular file named after the tool, or the archive's only file.
fn select_tar_entry(source: impl Read, name: &str, max_bytes: u64) -> Result<usize> {
    let mut archive = Archive::new(LimitedReader::new(source, max_bytes));
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
                // Validation consumes unselected entries too. Reject oversized
                // entries before the tar iterator can decompress and skip them.
                ensure!(
                    entry.size() <= max_bytes,
                    "decompressed entry exceeds size limit of {max_bytes} bytes"
                );
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

    create_cache_directory(&bin_directory)?;

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
        domain::lockfile::ChecksumSource,
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

    fn compressed_tar(format: &str, entries: &[(&str, EntryType, &[u8])]) -> Vec<u8> {
        let mut tar = Vec::new();
        GzDecoder::new(io::Cursor::new(tar_gz(entries)))
            .read_to_end(&mut tar)
            .unwrap();
        match format {
            "tar.gz" => tar_gz(entries),
            "tar.xz" => {
                let mut encoder =
                    lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(0))
                        .unwrap();
                encoder.write_all(&tar).unwrap();
                encoder.finish().unwrap()
            }
            "tar.zst" => ruzstd::encoding::compress_to_vec(
                tar.as_slice(),
                ruzstd::encoding::CompressionLevel::Fastest,
            ),
            "tar.bz2" => {
                let mut encoder =
                    bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
                encoder.write_all(&tar).unwrap();
                encoder.finish().unwrap()
            }
            _ => unreachable!(),
        }
    }

    fn zip_archive(entries: &[(&str, EntryType, &[u8])]) -> Vec<u8> {
        let mut archive = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (path, kind, data) in entries {
            match *kind {
                EntryType::Directory => archive.add_directory(*path, options).unwrap(),
                EntryType::Symlink => archive.add_symlink(*path, "tool", options).unwrap(),
                _ => {
                    archive.start_file(*path, options).unwrap();
                    archive.write_all(data).unwrap();
                }
            }
        }
        archive.finish().unwrap().into_inner()
    }

    fn archive_fixture(extension: &str, entries: &[(&str, EntryType, &[u8])]) -> Vec<u8> {
        if extension == "zip" {
            zip_archive(entries)
        } else {
            compressed_tar(extension, entries)
        }
    }

    #[test]
    fn unpacks_new_archive_formats_and_selects_only_the_tool() {
        for extension in ["tar.xz", "zip", "tar.zst", "tar.bz2"] {
            let format = ArtifactFormat::try_from(format!("tool.{extension}").as_str()).unwrap();
            for entries in [
                &[("tool", EntryType::Regular, &b"binary"[..])][..],
                &[("other", EntryType::Regular, &b"binary"[..])][..],
                &[
                    ("./dist/", EntryType::Directory, &b""[..]),
                    ("./dist/README.md", EntryType::Regular, b"docs"),
                    ("./dist/tool", EntryType::Regular, b"binary"),
                    ("./dist/LICENSE", EntryType::Regular, b"license"),
                ][..],
            ] {
                let mut output = Vec::new();
                unpack(
                    format,
                    "tool",
                    io::Cursor::new(archive_fixture(extension, entries)),
                    &mut output,
                )
                .unwrap();
                assert_eq!(output, b"binary", "{extension}");
            }
        }
    }

    #[test]
    fn rejects_zstd_frame_with_oversized_window() {
        // Zstandard magic, frame descriptor, and a 1 GiB window descriptor.
        let frame = [0x28, 0xb5, 0x2f, 0xfd, 0x00, 0xa0];
        let error = tar_decoder(ArtifactFormat::TarZst, io::Cursor::new(frame))
            .err()
            .expect("oversized window must be rejected");
        assert!(format!("{error:#}").to_lowercase().contains("window"));
    }

    #[test]
    fn rejects_oversized_tar_long_name_metadata() {
        let mut header = tar::Header::new_gnu();
        header.set_path("././@LongLink").unwrap();
        header.set_entry_type(EntryType::GNULongName);
        header.set_size(4096);
        header.set_mode(0o644);
        header.set_cksum();
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(header.as_bytes()).unwrap();
        encoder.write_all(&vec![b'a'; 4096]).unwrap();
        let archive = encoder.finish().unwrap();
        let error = unpack_with_limit(
            ArtifactFormat::TarGz,
            "tool",
            io::Cursor::new(archive),
            Vec::new(),
            1024,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("size limit of 1024 bytes"));
    }

    #[test]
    fn unpacks_tar_with_long_name_under_limit() {
        let path = format!("{}/tool", "a".repeat(120));
        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
        let mut header = tar::Header::new_gnu();
        header.set_size(6);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, path, &b"binary"[..])
            .unwrap();
        let archive = builder.into_inner().unwrap().finish().unwrap();
        let mut output = Vec::new();
        unpack_with_limit(
            ArtifactFormat::TarGz,
            "tool",
            io::Cursor::new(archive),
            &mut output,
            4096,
        )
        .unwrap();
        assert_eq!(output, b"binary");
    }

    #[test]
    fn unpacks_shellcheck_v0_11_0_tar_xz() {
        let format = ArtifactFormat::try_from("shellcheck-v0.11.0.darwin.aarch64.tar.xz").unwrap();
        let archive = compressed_tar(
            "tar.xz",
            &[
                ("shellcheck-v0.11.0/", EntryType::Directory, b""),
                (
                    "shellcheck-v0.11.0/LICENSE.txt",
                    EntryType::Regular,
                    b"license",
                ),
                ("shellcheck-v0.11.0/README.txt", EntryType::Regular, b"docs"),
                (
                    "shellcheck-v0.11.0/shellcheck",
                    EntryType::Regular,
                    b"shellcheck binary",
                ),
            ],
        );
        let mut output = Vec::new();
        unpack(format, "shellcheck", io::Cursor::new(archive), &mut output).unwrap();
        assert_eq!(output, b"shellcheck binary");
    }

    #[test]
    fn rejects_unsafe_and_unselectable_new_archives_before_writing() {
        for extension in ["tar.xz", "zip", "tar.zst", "tar.bz2"] {
            let format = ArtifactFormat::try_from(format!("tool.{extension}").as_str()).unwrap();
            for (entries, message) in [
                (
                    &[("../tool", EntryType::Regular, &b"x"[..])][..],
                    "unsafe archive entry",
                ),
                (
                    &[("dist/../../tool", EntryType::Regular, &b"x"[..])][..],
                    "unsafe archive entry",
                ),
                (
                    &[("dist/../tool", EntryType::Regular, &b"x"[..])][..],
                    "unsafe archive entry",
                ),
                (
                    &[("/usr/bin/tool", EntryType::Regular, &b"x"[..])][..],
                    "unsafe archive entry",
                ),
                (
                    &[("tool", EntryType::Symlink, &b""[..])][..],
                    "unsupported archive entry",
                ),
                (
                    &[
                        ("tool", EntryType::Regular, &b"binary"[..]),
                        ("../escape", EntryType::Regular, b"x"),
                    ][..],
                    "unsafe archive entry",
                ),
                (
                    &[
                        ("tool", EntryType::Regular, &b"binary"[..]),
                        ("link", EntryType::Symlink, b""),
                    ][..],
                    "unsupported archive entry",
                ),
                (
                    &[
                        ("README", EntryType::Regular, &b"docs"[..]),
                        ("LICENSE", EntryType::Regular, b"license"),
                    ][..],
                    "archive has no file named tool",
                ),
                (
                    &[
                        ("a/tool", EntryType::Regular, &b"one"[..]),
                        ("b/tool", EntryType::Regular, b"two"),
                    ][..],
                    "archive has multiple files named tool",
                ),
                (&[][..], "archive has no file named tool"),
            ] {
                let mut output = Vec::new();
                let error = unpack(
                    format,
                    "tool",
                    io::Cursor::new(archive_fixture(extension, entries)),
                    &mut output,
                )
                .unwrap_err();
                assert!(
                    error.to_string().contains(message),
                    "{extension}: {error:#}"
                );
                assert!(output.is_empty(), "wrote data before rejecting {extension}");
            }
        }
    }

    #[test]
    fn bounds_decompression_for_every_compressed_format() {
        // Highly compressible local fixtures exercise the actual decoders with
        // a small private limit rather than allocating a 512 MiB executable.
        const LIMIT: u64 = 4096;
        for extension in ["gz", "tar.gz", "tar.xz", "zip", "tar.zst", "tar.bz2"] {
            let format = ArtifactFormat::try_from(format!("tool.{extension}").as_str()).unwrap();
            let valid_length = if extension.starts_with("tar.") {
                LIMIT - 1024
            } else {
                LIMIT
            };
            for length in [valid_length, LIMIT + 1] {
                let payload = vec![0; length as usize];
                let archive = if extension == "gz" {
                    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
                    encoder.write_all(&payload).unwrap();
                    encoder.finish().unwrap()
                } else {
                    archive_fixture(extension, &[("tool", EntryType::Regular, &payload)])
                };
                assert!(
                    archive.len() < LIMIT as usize,
                    "fixture is not compressed: {extension}"
                );
                let mut output = Vec::new();
                let result =
                    unpack_with_limit(format, "tool", io::Cursor::new(archive), &mut output, LIMIT);
                if length == valid_length {
                    result.unwrap();
                    assert_eq!(output, payload, "{extension}");
                } else {
                    let error = result.unwrap_err();
                    assert!(
                        error
                            .to_string()
                            .contains("exceeds size limit of 4096 bytes"),
                        "{extension}: {error:#}"
                    );
                    assert!(output.len() <= LIMIT as usize, "{extension}");
                }
            }
        }
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
    fn reuses_cache_only_with_matching_artifact_and_executable_stamps() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("tool");
        let checksum_stamp = directory.path().join(".artifact-sha256");
        let executable_stamp = directory.path().join(EXECUTABLE_SHA256);
        let cached = || {
            cached_executable_matches(&destination, &checksum_stamp, "expected", None, "tool")
                .unwrap()
        };

        assert!(!cached());

        fs::write(&destination, "binary").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(&executable_stamp, checksum(b"binary")).unwrap();
        assert!(!cached());

        fs::write(&checksum_stamp, "different\n").unwrap();
        assert!(!cached());

        fs::write(&checksum_stamp, "expected\n").unwrap();
        assert!(cached());

        fs::write(&destination, "modified binary").unwrap();
        assert!(!cached());

        fs::write(&destination, "binary").unwrap();
        fs::remove_file(&executable_stamp).unwrap();
        assert!(!cached());
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
        assert_eq!(
            fs::read_to_string(
                directory
                    .path()
                    .join(".tools/tool/1.0.0/.executable-sha256")
            )
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

        assert!(format!("{error:#}").contains("checksum mismatch for tool"));
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
    fn switching_sources_at_the_same_version_invalidates_the_previous_stamp() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("tool");
        let staged = directory.path().join("staged");
        let artifact_stamp = directory.path().join(".artifact-sha256");
        let crate_stamp = directory.path().join(".crate-sha256");
        let artifact_sha = checksum(b"artifact");
        let crate_sha = checksum(b"crate");

        for (current_stamp, sha, previous_stamp) in [
            (&artifact_stamp, &artifact_sha, &crate_stamp),
            (&crate_stamp, &crate_sha, &artifact_stamp),
            (&artifact_stamp, &artifact_sha, &crate_stamp),
        ] {
            let retained = directory.path().join(RETAINED_ARTIFACT);
            fs::write(&retained, b"previous archive").unwrap();
            fs::write(&staged, sha).unwrap();
            fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)).unwrap();
            persist_executable(&staged, &destination, current_stamp, sha, None).unwrap();

            assert!(
                cached_executable_matches(&destination, current_stamp, sha, None, "tool").unwrap()
            );
            assert!(!retained.exists());
            assert!(!previous_stamp.exists(), "{}", previous_stamp.display());
        }
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
