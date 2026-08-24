use crate::{
    download,
    lockfile::{ArtifactFormat, Lockfile},
    platform::Platform,
    update,
};
use anyhow::{Context, Result, ensure};
use flate2::read::GzDecoder;
use std::{
    fs,
    io::{self, Read, Write},
    path::Path,
};

pub fn install() -> Result<()> {
    let lock_path = Path::new("binloom.lock");

    if !lock_path
        .try_exists()
        .context("failed to check binloom.lock")?
    {
        update::update(None)?;
    }

    let lockfile = Lockfile::try_from(lock_path)?;
    ensure!(!lockfile.tools.is_empty(), "no tools in binloom.lock");

    let platform = Platform::current()?;
    let platform_key = platform.to_string();
    let client = download::client()?;

    for (name, tool) in &lockfile.tools {
        let artifact = tool
            .artifacts
            .get(&platform_key)
            .with_context(|| format!("tool {name} has no artifact for {platform}"))?;

        let directory = Path::new(".tools").join(name).join(&tool.version);

        fs::create_dir_all(&directory)
            .with_context(|| format!("failed to create {}", directory.display()))?;

        let destination = directory.join(name);
        let checksum_stamp = directory.join(".artifact-sha256");

        if cached_artifact_matches(&destination, &checksum_stamp, &artifact.sha256)? {
            println!("Already installed {name} {}", tool.version);
            link_tool(name, &tool.version)?;
            continue;
        }

        let mut downloaded = tempfile::NamedTempFile::new_in(&directory)?;

        let actual_sha256 = download::download_to(&client, &artifact.url, &mut downloaded)?;

        ensure!(
            actual_sha256 == artifact.sha256,
            "checksum mismatch for {name}: expected {}, got {actual_sha256}",
            artifact.sha256
        );

        let mut executable = tempfile::NamedTempFile::new_in(&directory)?;
        let downloaded_file = downloaded.reopen()?;

        unpack(artifact.format, downloaded_file, executable.as_file_mut())
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

        link_tool(name, &tool.version)?;

        println!("Installed {name} {}", tool.version);
    }

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
    mut source: impl Read,
    mut destination: impl Write,
) -> io::Result<u64> {
    match format {
        ArtifactFormat::Raw => io::copy(&mut source, &mut destination),
        ArtifactFormat::Gz => io::copy(&mut GzDecoder::new(source), &mut destination),
    }
}

#[cfg(unix)]
fn link_tool(name: &str, version: &str) -> Result<()> {
    let bin_directory = Path::new(".tools/.bin");

    fs::create_dir_all(bin_directory).context("failed to create .tools/.bin")?;

    let link = bin_directory.join(name);
    let target = Path::new("..").join(name).join(version).join(name);

    match fs::remove_file(&link) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("failed to replace {}", link.display()));
        }
    }

    std::os::unix::fs::symlink(&target, &link)
        .with_context(|| format!("failed to link {}", link.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};

    #[test]
    fn unpacks_raw_and_gzip() {
        let input = b"hello";

        let mut raw = Vec::new();
        unpack(ArtifactFormat::Raw, &input[..], &mut raw).unwrap();
        assert_eq!(raw, input);

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(input).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut gzip = Vec::new();
        unpack(ArtifactFormat::Gz, &compressed[..], &mut gzip).unwrap();
        assert_eq!(gzip, input);
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
}
