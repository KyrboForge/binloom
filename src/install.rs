use std::{
    io::{self, Read, Write},
    path::Path,
};

use crate::{
    download,
    lockfile::{ArtifactFormat, Lockfile},
    platform::Platform,
    update,
};
use anyhow::{Context, Result, ensure};
use flate2::read::GzDecoder;

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

        std::fs::create_dir_all(&directory)
            .with_context(|| format!("failed to create {}", directory.display()))?;

        let destination = directory.join(name);

        if destination.is_file() {
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
                .set_permissions(std::fs::Permissions::from_mode(0o755))?;
        }

        executable.as_file().sync_all()?;

        executable
            .persist(&destination)
            .map_err(|error| error.error)
            .with_context(|| format!("failed to install {}", destination.display()))?;
        link_tool(name, &tool.version)?;

        println!("Installed {name} {}", tool.version);
    }

    Ok(())
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

    std::fs::create_dir_all(bin_directory).context("failed to create .tools/.bin")?;

    let link = bin_directory.join(name);
    let target = Path::new("..").join(name).join(version).join(name);

    match std::fs::remove_file(&link) {
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
}
