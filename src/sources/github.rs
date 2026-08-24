use crate::{download, manifest::GithubSource, platform::Platform};
use anyhow::{Context, Result, bail, ensure};
use reqwest::{StatusCode, blocking::Client};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub assets: Vec<ReleaseAsset>,
    pub published_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
    pub digest: Option<String>,
}

pub fn fetch_release(client: &Client, source: &GithubSource, version: &str) -> Result<Release> {
    let tags = if version.starts_with('v') {
        vec![version.to_owned()]
    } else {
        vec![format!("v{version}"), version.to_owned()]
    };

    for tag in tags {
        let url = format!(
            "https://api.github.com/repos/{}/{}/releases/tags/{tag}",
            source.owner, source.repository
        );

        let response = client
            .get(&url)
            .send()
            .with_context(|| format!("failed to fetch GitHub release {tag}"))?;

        if response.status() == StatusCode::NOT_FOUND {
            continue;
        }

        return response
            .error_for_status()
            .with_context(|| format!("GitHub rejected release request for {tag}"))?
            .json()
            .with_context(|| format!("failed to parse GitHub release {tag}"));
    }

    bail!(
        "release {} not found for {}/{}",
        version,
        source.owner,
        source.repository
    )
}

pub fn find_asset<'a>(
    release: &'a Release,
    tool_name: &str,
    platform: Platform,
) -> Result<&'a ReleaseAsset> {
    let tool_name = tool_name.to_lowercase();

    let matches: Vec<_> = release
        .assets
        .iter()
        .filter(|asset| {
            let name = asset.name.to_lowercase();
            let is_metadata = [".sha256", ".sha256sum", ".sig", ".minisig"]
                .iter()
                .any(|suffix| name.ends_with(suffix));

            !is_metadata
                && name.contains(&tool_name)
                && platform
                    .os_aliases()
                    .iter()
                    .any(|alias| name.contains(alias))
                && platform
                    .arch_aliases()
                    .iter()
                    .any(|alias| name.contains(alias))
        })
        .collect();
    let gzip_matches = matches
        .iter()
        .copied()
        .filter(|asset| asset.name.to_ascii_lowercase().ends_with(".gz"))
        .collect::<Vec<_>>();

    let matches = if gzip_matches.len() == 1 {
        gzip_matches
    } else {
        matches
    };
    let matches = prefer(matches, |asset| {
        asset
            .name
            .to_ascii_lowercase()
            .contains(platform.os_aliases()[0])
    });

    let matches = prefer(matches, |asset| {
        asset
            .name
            .to_ascii_lowercase()
            .contains(platform.arch_aliases()[0])
    });

    let matches = prefer(matches, |asset| {
        asset.name.to_ascii_lowercase().ends_with(".gz")
    });

    match matches.as_slice() {
        [asset] => Ok(asset),
        [] => bail!("no release asset matched {tool_name} for {platform}"),
        assets => {
            let names = assets
                .iter()
                .map(|asset| asset.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");

            bail!("multiple release assets matched {tool_name} for {platform}: {names}")
        }
    }
}

fn prefer(
    assets: Vec<&ReleaseAsset>,
    predicate: impl Fn(&ReleaseAsset) -> bool,
) -> Vec<&ReleaseAsset> {
    let preferred = assets
        .iter()
        .copied()
        .filter(|asset| predicate(asset))
        .collect::<Vec<_>>();

    if preferred.is_empty() {
        assets
    } else {
        preferred
    }
}

pub fn sha256_from_digest(asset: &ReleaseAsset) -> Result<Option<String>> {
    let Some(digest) = asset.digest.as_deref() else {
        return Ok(None);
    };

    let checksum = digest
        .strip_prefix("sha256:")
        .with_context(|| format!("unsupported digest for {}", asset.name))?;

    ensure!(
        checksum.len() == 64
            && checksum
                .chars()
                .all(|character| character.is_ascii_hexdigit()),
        "invalid SHA-256 digest for {}",
        asset.name
    );

    Ok(Some(checksum.to_ascii_lowercase()))
}

fn normalize_sha256(value: &str) -> Option<String> {
    let value = value.trim();

    if value.len() == 64 && value.chars().all(|character| character.is_ascii_hexdigit()) {
        Some(value.to_ascii_lowercase())
    } else {
        None
    }
}

fn checksum_from_text(
    content: &str,
    asset_name: &str,
    allow_bare_checksum: bool,
) -> Option<String> {
    let bsd_prefix = format!("SHA256 ({asset_name}) = ");

    for line in content.lines().map(str::trim) {
        if allow_bare_checksum && let Some(checksum) = normalize_sha256(line) {
            return Some(checksum);
        }

        if let Some(checksum) = line.strip_prefix(&bsd_prefix).and_then(normalize_sha256) {
            return Some(checksum);
        }

        let mut fields = line.split_whitespace();
        let Some(checksum) = fields.next().and_then(normalize_sha256) else {
            continue;
        };
        let Some(filename) = fields.next() else {
            continue;
        };

        if filename.trim_start_matches('*') == asset_name {
            return Some(checksum);
        }
    }

    None
}
pub fn checksum_from_release(
    client: &Client,
    release: &Release,
    asset: &ReleaseAsset,
) -> Result<Option<String>> {
    if let Some(checksum) = sha256_from_digest(asset)? {
        return Ok(Some(checksum));
    }

    let exact_names = [
        format!("{}.sha256", asset.name),
        format!("{}.sha256sum", asset.name),
    ];

    for exact in [true, false] {
        for checksum_asset in &release.assets {
            let is_exact = exact_names
                .iter()
                .any(|name| checksum_asset.name.eq_ignore_ascii_case(name));

            let is_global = matches!(
                checksum_asset.name.to_ascii_lowercase().as_str(),
                "sha256sums" | "sha256sums.txt" | "checksums.txt" | "checksums.sha256"
            );

            if (exact && !is_exact) || (!exact && !is_global) {
                continue;
            }

            let content = download::text_url(client, &checksum_asset.browser_download_url)?;

            if let Some(checksum) = checksum_from_text(&content, &asset.name, is_exact) {
                return Ok(Some(checksum));
            }
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> ReleaseAsset {
        ReleaseAsset {
            name: name.to_owned(),
            browser_download_url: format!("https://example.com/{name}"),
            digest: None,
        }
    }

    #[test]
    fn matches_lefthook_assets_for_every_platform() {
        let release = Release {
            tag_name: "v2.1.10".to_owned(),
            published_at: Some("2026-01-01T00:00:00Z".to_owned()),
            assets: vec![
                asset("lefthook_2.1.10_MacOS_arm64.gz"),
                asset("lefthook_2.1.10_MacOS_x86_64.gz"),
                asset("lefthook_2.1.10_Linux_aarch64.gz"),
                asset("lefthook_2.1.10_Linux_x86_64.gz"),
            ],
        };

        let expected = [
            (Platform::MacosAarch64, "lefthook_2.1.10_MacOS_arm64.gz"),
            (Platform::MacosX86_64, "lefthook_2.1.10_MacOS_x86_64.gz"),
            (Platform::LinuxAarch64, "lefthook_2.1.10_Linux_aarch64.gz"),
            (Platform::LinuxX86_64, "lefthook_2.1.10_Linux_x86_64.gz"),
        ];

        for (platform, expected_name) in expected {
            let matched = find_asset(&release, "lefthook", platform).unwrap();

            assert_eq!(matched.name, expected_name);
        }
    }

    #[test]
    fn rejects_ambiguous_assets() {
        let release = Release {
            tag_name: "v1.0.0".to_owned(),
            published_at: Some("2026-01-01T00:00:00Z".to_owned()),
            assets: vec![
                asset("tool_1.0.0_linux_x86_64.gz"),
                asset("tool-pro_1.0.0_linux_x86_64.gz"),
            ],
        };

        let error = find_asset(&release, "tool", Platform::LinuxX86_64).unwrap_err();

        assert!(
            error
                .to_string()
                .starts_with("multiple release assets matched")
        );
    }

    #[test]
    fn reads_sha256_from_github_digest() {
        let mut release_asset = asset("tool_linux_x86_64.gz");
        release_asset.digest = Some(format!("sha256:{}", "a".repeat(64)));

        assert_eq!(
            sha256_from_digest(&release_asset).unwrap(),
            Some("a".repeat(64))
        );
    }
    #[test]
    fn reads_checksum_from_sha256sums() {
        let checksum = "a".repeat(64);
        let content = format!(
            "{}  other-tool.gz\n{}  tool_linux_x86_64.gz\n",
            "b".repeat(64),
            checksum
        );

        assert_eq!(
            checksum_from_text(&content, "tool_linux_x86_64.gz", false),
            Some(checksum)
        );
    }

    #[test]
    fn reads_bare_sidecar_checksum() {
        let checksum = "A".repeat(64);

        assert_eq!(
            checksum_from_text(&checksum, "tool.gz", true),
            Some("a".repeat(64))
        );
    }

    #[test]
    fn ignores_checksum_assets_when_matching_binary() {
        let release = Release {
            tag_name: "v1.0.0".to_owned(),
            published_at: Some("2026-01-01T00:00:00Z".to_owned()),
            assets: vec![
                asset("tool_1.0.0_linux_x86_64.gz"),
                asset("tool_1.0.0_linux_x86_64.gz.sha256"),
            ],
        };

        let matched = find_asset(&release, "tool", Platform::LinuxX86_64).unwrap();

        assert_eq!(matched.name, "tool_1.0.0_linux_x86_64.gz");
    }
    #[test]
    fn prefers_gzip_over_raw_asset() {
        let release = Release {
            tag_name: "v1.0.0".to_owned(),
            published_at: Some("2026-01-01T00:00:00Z".to_owned()),
            assets: vec![
                asset("tool_1.0.0_MacOS_arm64"),
                asset("tool_1.0.0_MacOS_arm64.gz"),
            ],
        };

        let matched = find_asset(&release, "tool", Platform::MacosAarch64).unwrap();

        assert_eq!(matched.name, "tool_1.0.0_MacOS_arm64.gz");
    }
}
