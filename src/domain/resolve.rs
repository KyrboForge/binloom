use crate::common::{validate_version, warn};
use crate::domain::lockfile::{
    ArtifactFormat, ChecksumSource, LockedArtifact, LockedTool, LockedWrapper,
};
use crate::domain::manifest::Tool;
use crate::domain::platform::Platform;
use crate::domain::sources::{Source, release};
use crate::download;
use crate::download::Client;
use anyhow::{Context, bail};
use std::collections::{BTreeMap, BTreeSet};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

pub(crate) fn resolve_tool(
    name: &str,
    tool: &Tool,
    version: Option<&str>,
    minimum_age_minutes: u64,
    client: &Client,
) -> anyhow::Result<LockedTool> {
    let provider = tool.source.provider();
    let mut checksum_cache = BTreeMap::new();

    let release = match version {
        Some(version) => provider.fetch_release(client, version)?,
        None => provider.fetch_latest_release(client)?,
    };

    resolve_release(
        name,
        &tool.source,
        tool.asset.as_deref(),
        &release,
        minimum_age_minutes,
        client,
        &mut checksum_cache,
    )
}

pub(crate) fn resolve_binloom(
    source: &Source,
    version: Option<&str>,
    minimum_age_minutes: u64,
    client: &Client,
) -> anyhow::Result<(LockedTool, LockedWrapper)> {
    let provider = source.provider();

    let release = match version {
        Some(version) => provider.fetch_release(client, version)?,
        None => provider.fetch_latest_release(client)?,
    };

    resolve_binloom_release(source, &release, minimum_age_minutes, client)
}

fn resolve_checksum(
    client: &Client,
    release: &release::Release,
    asset: &release::ReleaseAsset,
    checksum_cache: &mut BTreeMap<String, String>,
) -> anyhow::Result<(String, ChecksumSource)> {
    if let Some(checksum) = &asset.sha256 {
        return Ok((checksum.clone(), ChecksumSource::Digest));
    }

    if let Some(checksum) = release.checksum_from_sidecar(client, asset, checksum_cache)? {
        return Ok((checksum, ChecksumSource::Sidecar));
    }

    warn(&format!(
        "{} has no published checksum; hashing downloaded bytes (TOFU)",
        asset.name
    ));

    Ok((
        download::sha256_url(client, &asset.download_url)?,
        ChecksumSource::Download,
    ))
}

fn version_from_tag(tag: &str) -> anyhow::Result<String> {
    let version = tag.strip_prefix('v').unwrap_or(tag);

    validate_version(version)
        .with_context(|| format!("release tag {tag} contains an unsafe version"))?;

    Ok(version.to_owned())
}

fn resolve_release(
    name: &str,
    source: &Source,
    asset_pattern: Option<&str>,
    release: &release::Release,
    minimum_age_minutes: u64,
    client: &Client,
    checksum_cache: &mut BTreeMap<String, String>,
) -> anyhow::Result<LockedTool> {
    println!("Found {} for {name}:", release.tag);
    ensure_minimum_release_age(release, minimum_age_minutes, OffsetDateTime::now_utc())?;
    let version = version_from_tag(&release.tag)?;
    let mut artifacts = BTreeMap::new();
    let mut emitted_warnings = BTreeSet::new();
    for platform in Platform::ALL {
        let asset = match asset_pattern {
            Some(pattern) => release.find_asset_by_pattern(pattern, &version, platform)?,
            None => release.find_asset(name, platform, &mut emitted_warnings)?,
        };
        let (sha256, checksum_source) = resolve_checksum(client, release, asset, checksum_cache)?;
        artifacts.insert(
            platform.to_string(),
            LockedArtifact {
                asset: asset.name.clone(),
                url: asset.download_url.clone(),
                sha256,
                format: ArtifactFormat::try_from(asset.name.as_str())?,
                checksum_source,
            },
        );
    }

    Ok(LockedTool {
        version,
        source: source.to_string(),
        tag: release.tag.clone(),
        artifacts,
    })
}

fn ensure_minimum_release_age(
    release: &release::Release,
    minimum_minutes: u64,
    now: OffsetDateTime,
) -> anyhow::Result<()> {
    let published_at = release
        .published_at
        .as_deref()
        .with_context(|| format!("release {} has no publication date", release.tag))?;

    let published_at = OffsetDateTime::parse(published_at, &Rfc3339)
        .with_context(|| format!("release {} has invalid publication date", release.tag))?;

    let minimum_seconds = minimum_minutes
        .checked_mul(60)
        .and_then(|seconds| i64::try_from(seconds).ok())
        .context("minimum release age is too large")?;

    let minimum_age = Duration::seconds(minimum_seconds);
    let actual_age = now - published_at;

    if actual_age < minimum_age {
        let remaining_seconds = (minimum_age - actual_age).whole_seconds();
        let remaining_minutes = (remaining_seconds + 59) / 60;

        bail!(
            "release {} is too new; wait {remaining_minutes} more minute(s)",
            release.tag
        );
    }

    Ok(())
}

fn resolve_binloom_release(
    source: &Source,
    release: &release::Release,
    minimum_age_minutes: u64,
    client: &Client,
) -> anyhow::Result<(LockedTool, LockedWrapper)> {
    let mut checksum_cache = BTreeMap::new();
    let binloom = resolve_release(
        "binloom",
        source,
        None,
        release,
        minimum_age_minutes,
        client,
        &mut checksum_cache,
    )?;
    let asset = release.find_asset_by_name("binloomw")?;
    let (sha256, checksum_source) = resolve_checksum(client, release, asset, &mut checksum_cache)?;

    let wrapper = LockedWrapper {
        version: binloom.version.clone(),
        url: asset.download_url.clone(),
        sha256,
        checksum_source,
    };

    Ok((binloom, wrapper))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::sources::release::{Release, ReleaseAsset};
    use crate::http_fixture::{Response, Server};

    fn asset(name: &str) -> ReleaseAsset {
        ReleaseAsset {
            name: name.to_owned(),
            download_url: format!("https://example.com/{name}"),
            sha256: None,
        }
    }

    #[test]
    fn enforces_minimum_release_age() {
        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: Some("2026-01-01T12:00:00Z".to_owned()),
            assets: vec![],
        };

        let now = OffsetDateTime::parse("2026-01-02T00:00:00Z", &Rfc3339).unwrap();

        let error = ensure_minimum_release_age(&release, 24 * 60, now).unwrap_err();

        assert_eq!(
            error.to_string(),
            "release v1.0.0 is too new; wait 720 more minute(s)"
        );

        ensure_minimum_release_age(&release, 12 * 60, now).unwrap();
    }

    #[test]
    fn rejects_unsafe_release_tags() {
        assert_eq!(version_from_tag("v1.2.3").unwrap(), "1.2.3");
        assert_eq!(version_from_tag("1.2.3").unwrap(), "1.2.3");

        for tag in ["v/tmp/x", "v../../x", r"v..\..\x", "v.hidden"] {
            assert!(version_from_tag(tag).is_err(), "{tag:?}");
        }
    }

    #[test]
    fn records_embedded_digest_provenance() {
        let checksum = "a".repeat(64);
        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: None,
            assets: vec![ReleaseAsset {
                name: "tool.gz".to_owned(),
                download_url: "https://example.com/tool.gz".to_owned(),
                sha256: Some(checksum.clone()),
            }],
        };
        let client = download::client();
        let mut checksum_cache = BTreeMap::new();

        let resolved =
            resolve_checksum(&client, &release, &release.assets[0], &mut checksum_cache).unwrap();

        assert_eq!(resolved, (checksum, ChecksumSource::Digest));
    }

    #[test]
    fn records_sidecar_checksum_provenance() {
        let checksum = "b".repeat(64);
        let server = Server::start(vec![Response {
            status: 200,
            body: format!("{checksum}  tool.gz\n").into_bytes(),
        }]);
        let mut checksum_cache = BTreeMap::new();

        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: None,
            assets: vec![
                ReleaseAsset {
                    name: "tool.gz".to_owned(),
                    download_url: format!("{}/tool.gz", server.url()),
                    sha256: None,
                },
                ReleaseAsset {
                    name: "tool.gz.sha256".to_owned(),
                    download_url: format!("{}/tool.gz.sha256", server.url()),
                    sha256: None,
                },
            ],
        };
        let client = download::client();

        let resolved =
            resolve_checksum(&client, &release, &release.assets[0], &mut checksum_cache).unwrap();

        assert_eq!(resolved, (checksum, ChecksumSource::Sidecar));
        assert_eq!(server.requests()[0].path, "/tool.gz.sha256");
    }

    #[test]
    fn records_downloaded_checksum_provenance() {
        let server = Server::start(vec![Response {
            status: 200,
            body: b"hello".to_vec(),
        }]);

        let mut checksum_cache = BTreeMap::new();

        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: None,
            assets: vec![ReleaseAsset {
                name: "tool.gz".to_owned(),
                download_url: format!("{}/tool.gz", server.url()),
                sha256: None,
            }],
        };
        let client = download::client();

        let resolved =
            resolve_checksum(&client, &release, &release.assets[0], &mut checksum_cache).unwrap();

        assert_eq!(
            resolved,
            (
                "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".to_owned(),
                ChecksumSource::Download,
            )
        );
        assert_eq!(server.requests()[0].path, "/tool.gz");
    }

    #[test]
    fn reuses_global_checksum_sidecar() {
        let first_checksum = "a".repeat(64);
        let second_checksum = "b".repeat(64);

        let server = Server::start(vec![Response {
            status: 200,
            body: format!(
                "{first_checksum}  tool_linux_x86_64.gz\n\
             {second_checksum}  tool_macos_aarch64.gz\n"
            )
            .into_bytes(),
        }]);

        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: None,
            assets: vec![
                asset("tool_linux_x86_64.gz"),
                asset("tool_macos_aarch64.gz"),
                ReleaseAsset {
                    name: "SHA256SUMS".to_owned(),
                    download_url: format!("{}/SHA256SUMS", server.url()),
                    sha256: None,
                },
            ],
        };

        let client = download::client();
        let mut checksum_cache = BTreeMap::new();

        assert_eq!(
            release
                .checksum_from_sidecar(&client, &release.assets[0], &mut checksum_cache)
                .unwrap(),
            Some(first_checksum)
        );

        assert_eq!(
            release
                .checksum_from_sidecar(&client, &release.assets[1], &mut checksum_cache)
                .unwrap(),
            Some(second_checksum)
        );

        assert_eq!(server.requests().len(), 1);
    }
}
