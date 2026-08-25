use std::{
    fmt::Write,
    io::{self, Read},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
pub(crate) use ureq::Agent as Client;

const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;

pub fn client() -> Client {
    Client::config_builder()
        .user_agent(concat!("binloom/", env!("CARGO_PKG_VERSION")))
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(Duration::from_secs(10 * 60)))
        .build()
        .into()
}

pub fn sha256_url(client: &Client, url: &str) -> Result<String> {
    let mut response = client
        .get(url)
        .call()
        .with_context(|| format!("failed to download {url}"))?;

    sha256(response.body_mut().as_reader(), MAX_DOWNLOAD_BYTES)
        .with_context(|| format!("failed to hash {url}"))
}
fn sha256(reader: impl Read, max_bytes: u64) -> io::Result<String> {
    copy_and_sha256(reader, io::sink(), max_bytes)
}

fn copy_and_sha256(
    mut reader: impl Read,
    mut writer: impl io::Write,
    max_bytes: u64,
) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;

    loop {
        let bytes_read = reader.read(&mut buffer)?;

        if bytes_read == 0 {
            break;
        }

        total += bytes_read as u64;

        if total > max_bytes {
            return Err(io::Error::other(format!(
                "download exceeds {max_bytes} bytes"
            )));
        }

        writer.write_all(&buffer[..bytes_read])?;
        hasher.update(&buffer[..bytes_read]);
    }

    let digest = hasher.finalize();
    let mut checksum = String::with_capacity(digest.len() * 2);

    for byte in digest {
        write!(&mut checksum, "{byte:02x}").expect("writing to String cannot fail");
    }

    Ok(checksum)
}

pub fn text_url(client: &Client, url: &str) -> Result<String> {
    const MAX_BYTES: u64 = 1024 * 1024;

    let mut response = client
        .get(url)
        .call()
        .with_context(|| format!("failed to download {url}"))?;

    let mut content = String::new();

    response
        .body_mut()
        .as_reader()
        .take(MAX_BYTES + 1)
        .read_to_string(&mut content)
        .with_context(|| format!("failed to read {url}"))?;

    ensure!(
        content.len() as u64 <= MAX_BYTES,
        "checksum file exceeds 1 MiB: {url}"
    );

    Ok(content)
}

pub fn download_to(client: &Client, url: &str, mut writer: impl io::Write) -> Result<String> {
    let mut response = client
        .get(url)
        .call()
        .with_context(|| format!("failed to download {url}"))?;

    copy_and_sha256(
        response.body_mut().as_reader(),
        &mut writer,
        MAX_DOWNLOAD_BYTES,
    )
    .with_context(|| format!("failed to store download from {url}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculates_sha256() {
        let checksum = sha256("hello".as_bytes(), MAX_DOWNLOAD_BYTES).unwrap();

        assert_eq!(
            checksum,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }
    #[test]
    fn rejects_downloads_over_limit() {
        let mut output = Vec::new();

        let error = copy_and_sha256("hello".as_bytes(), &mut output, 4).unwrap_err();

        assert!(error.to_string().contains("exceeds 4 bytes"));
        assert!(output.is_empty());
    }
}
