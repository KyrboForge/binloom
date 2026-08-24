use std::{
    fmt::Write,
    io::{self, Read},
};

use anyhow::{Context, Result, ensure};
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};

pub fn client() -> Result<Client> {
    Client::builder()
        .user_agent(concat!("binloom/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("failed to create HTTP client")
}

pub fn sha256_url(client: &Client, url: &str) -> Result<String> {
    let mut response = client
        .get(url)
        .send()
        .with_context(|| format!("failed to download {url}"))?
        .error_for_status()
        .with_context(|| format!("download failed for {url}"))?;

    sha256(&mut response).with_context(|| format!("failed to hash {url}"))
}

fn sha256(mut reader: impl Read) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];

    loop {
        let bytes_read = reader.read(&mut buffer)?;

        if bytes_read == 0 {
            break;
        }

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

    let response = client
        .get(url)
        .send()
        .with_context(|| format!("failed to download {url}"))?
        .error_for_status()
        .with_context(|| format!("download failed for {url}"))?;

    let mut content = String::new();

    response
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
        .send()
        .with_context(|| format!("failed to download {url}"))?
        .error_for_status()
        .with_context(|| format!("download failed for {url}"))?;

    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];

    loop {
        let bytes_read = response
            .read(&mut buffer)
            .with_context(|| format!("failed to read {url}"))?;

        if bytes_read == 0 {
            break;
        }

        writer
            .write_all(&buffer[..bytes_read])
            .with_context(|| format!("failed to write download from {url}"))?;

        hasher.update(&buffer[..bytes_read]);
    }

    let digest = hasher.finalize();
    let mut checksum = String::with_capacity(digest.len() * 2);

    for byte in digest {
        write!(&mut checksum, "{byte:02x}").expect("writing to String cannot fail");
    }

    Ok(checksum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculates_sha256() {
        let checksum = sha256("hello".as_bytes()).unwrap();

        assert_eq!(
            checksum,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }
}
