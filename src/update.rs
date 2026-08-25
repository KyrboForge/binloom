use std::{collections::BTreeMap, path::Path};

use crate::{
    common::{LOCKFILE, MANIFEST, project_root, validate_version, warn},
    download,
    lockfile::{
        ArtifactFormat, ChecksumSource, LockedArtifact, LockedTool, LockedWrapper, Lockfile,
    },
    manifest::{self, Manifest, Tool},
    platform::Platform,
    sources::{Source, release},
};
use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
pub fn update(tool_name: Option<&str>) -> Result<()> {
    let root = project_root()?;

    update_tools(&root, tool_name, true)
}

pub fn lock_added_tool(tool_name: &str) -> Result<()> {
    let root = project_root()?;
    let lock_path = root.join(LOCKFILE);

    let existing = if lock_path
        .try_exists()
        .context("failed to check binloom.lock")?
    {
        Some(Lockfile::try_from(lock_path.as_path())?)
    } else {
        None
    };

    let target = lock_target(existing.as_ref(), tool_name);

    update_tools(&root, target, false)
}

fn update_tools(root: &Path, tool_name: Option<&str>, latest: bool) -> Result<()> {
    let manifest_path = root.join(MANIFEST);
    let lock_path = root.join(LOCKFILE);
    let manifest = Manifest::try_from(manifest_path.as_path())?;

    let minimum_age = manifest.update.minimum_release_age_minutes;

    let (selected, mut lockfile): (Vec<(&str, &Tool)>, Lockfile) = match tool_name {
        Some(name) => {
            let tool = manifest
                .tools
                .get(name)
                .with_context(|| format!("tool {name} is not configured"))?;

            let lockfile = Lockfile::try_from(lock_path.as_path())
                .context("failed to load binloom.lock; run `binloom update` first")?;

            (vec![(name, tool)], lockfile)
        }
        None => {
            let selected = manifest
                .tools
                .iter()
                .map(|(name, tool)| (name.as_str(), tool))
                .collect();

            let existing = if lock_path
                .try_exists()
                .context("failed to check binloom.lock")?
            {
                Some(Lockfile::try_from(lock_path.as_path())?)
            } else {
                None
            };

            (selected, fresh_lockfile(existing))
        }
    };

    let client = download::client()?;
    let mut versions = BTreeMap::new();

    for (name, tool) in selected {
        let locked = if latest {
            resolve_latest_tool(name, tool, minimum_age, &client)?
        } else {
            resolve_tool(name, tool, minimum_age, &client)?
        };

        versions.insert(name.to_owned(), locked.version.clone());
        lockfile.tools.insert(name.to_owned(), locked);
    }

    let binloom_version = if tool_name.is_none() {
        let source = binloom_source();

        let (locked, wrapper) = if latest {
            resolve_latest_binloom(&source, minimum_age, &client)?
        } else {
            resolve_binloom_version(&source, &manifest.binloom.version, minimum_age, &client)?
        };

        let version = locked.version.clone();

        lockfile.binloom = Some(locked);
        lockfile.wrapper = Some(wrapper);

        latest.then_some(version)
    } else {
        None
    };

    lockfile.write(lock_path.as_path())?;

    if latest {
        manifest::update_versions(
            manifest_path.as_path(),
            binloom_version.as_deref(),
            versions
                .iter()
                .map(|(name, version)| (name.as_str(), version.as_str())),
        )?;
    }

    println!("Updated {}", lock_path.display());

    Ok(())
}

fn resolve_latest_tool(
    name: &str,
    tool: &Tool,
    minimum_age_minutes: u64,
    client: &Client,
) -> Result<LockedTool> {
    let release = tool.source.provider().fetch_latest_release(client)?;

    resolve_release(
        name,
        &tool.source,
        tool.asset.as_deref(),
        &release,
        minimum_age_minutes,
        client,
    )
}

fn resolve_tool(
    name: &str,
    tool: &Tool,
    minimum_age_minutes: u64,
    client: &Client,
) -> Result<LockedTool> {
    let release = tool
        .source
        .provider()
        .fetch_release(client, &tool.version)?;

    resolve_release(
        name,
        &tool.source,
        tool.asset.as_deref(),
        &release,
        minimum_age_minutes,
        client,
    )
}

fn resolve_checksum(
    client: &Client,
    release: &release::Release,
    asset: &release::ReleaseAsset,
) -> Result<(String, ChecksumSource)> {
    if let Some(checksum) = &asset.sha256 {
        return Ok((checksum.clone(), ChecksumSource::Digest));
    }

    if let Some(checksum) = release.checksum_from_sidecar(client, asset)? {
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

fn version_from_tag(tag: &str) -> Result<String> {
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
) -> Result<LockedTool> {
    println!("Found {} for {name}:", release.tag);
    ensure_minimum_release_age(release, minimum_age_minutes, OffsetDateTime::now_utc())?;
    let version = version_from_tag(&release.tag)?;
    let mut artifacts = BTreeMap::new();
    for platform in Platform::ALL {
        let asset = match asset_pattern {
            Some(pattern) => release.find_asset_by_pattern(pattern, &version, platform)?,
            None => release.find_asset(name, platform)?,
        };
        let (sha256, checksum_source) = resolve_checksum(client, release, asset)?;
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

fn resolve_latest_binloom(
    source: &Source,
    minimum_age_minutes: u64,
    client: &Client,
) -> Result<(LockedTool, LockedWrapper)> {
    let release = source.provider().fetch_latest_release(client)?;

    resolve_binloom_release(source, &release, minimum_age_minutes, client)
}

fn resolve_binloom_version(
    source: &Source,
    version: &str,
    minimum_age_minutes: u64,
    client: &Client,
) -> Result<(LockedTool, LockedWrapper)> {
    let release = source.provider().fetch_release(client, version)?;

    resolve_binloom_release(source, &release, minimum_age_minutes, client)
}

fn ensure_minimum_release_age(
    release: &release::Release,
    minimum_minutes: u64,
    now: OffsetDateTime,
) -> Result<()> {
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

fn fresh_lockfile(existing: Option<Lockfile>) -> Lockfile {
    let Some(existing) = existing else {
        return Lockfile::default();
    };

    Lockfile {
        wrapper: existing.wrapper,
        binloom: existing.binloom,
        ..Lockfile::default()
    }
}

pub fn update_binloom() -> Result<()> {
    let root = project_root()?;
    let manifest_path = root.join(MANIFEST);
    let lock_path = root.join(LOCKFILE);
    let manifest = Manifest::try_from(manifest_path.as_path())?;

    let source = binloom_source();

    let client = download::client()?;

    let (locked, wrapper) = resolve_latest_binloom(
        &source,
        manifest.update.minimum_release_age_minutes,
        &client,
    )?;

    let mut lockfile = if lock_path
        .try_exists()
        .context("failed to check binloom.lock")?
    {
        Lockfile::try_from(lock_path.as_path())?
    } else {
        Lockfile::default()
    };

    let version = locked.version.clone();
    lockfile.binloom = Some(locked);
    lockfile.wrapper = Some(wrapper);
    lockfile.write(lock_path.as_path())?;

    manifest::update_versions(manifest_path.as_path(), Some(&version), [])?;

    println!("Updated Binloom in {}", lock_path.display());

    Ok(())
}

fn lock_target<'a>(existing: Option<&Lockfile>, tool_name: &'a str) -> Option<&'a str> {
    existing
        .is_some_and(|lockfile| lockfile.binloom.is_some() && lockfile.wrapper.is_some())
        .then_some(tool_name)
}

fn binloom_source() -> Source {
    Source::try_from("github:KyrboForge/binloom".to_owned())
        .expect("hardcoded Binloom source must be valid")
}

fn resolve_binloom_release(
    source: &Source,
    release: &release::Release,
    minimum_age_minutes: u64,
    client: &Client,
) -> Result<(LockedTool, LockedWrapper)> {
    let binloom = resolve_release(
        "binloom",
        source,
        None,
        release,
        minimum_age_minutes,
        client,
    )?;
    let asset = release.find_asset_by_name("binloomw")?;
    let (sha256, checksum_source) = resolve_checksum(client, release, asset)?;

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
    use crate::http_fixture::{Response, Server};

    #[test]
    fn enforces_minimum_release_age() {
        let release = release::Release {
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
    fn preserves_binloom_and_wrapper_when_rebuilding_lockfile() {
        let existing = Lockfile {
            wrapper: Some(LockedWrapper {
                version: "0.1.1".to_owned(),
                url: "https://example.com/binloomw".to_owned(),
                sha256: "a".repeat(64),
                checksum_source: ChecksumSource::Digest,
            }),
            binloom: Some(LockedTool {
                version: "0.1.0".to_owned(),
                source: "github:KyrboForge/binloom".to_owned(),
                tag: "v0.1.0".to_owned(),
                artifacts: BTreeMap::new(),
            }),
            ..Lockfile::default()
        };

        assert_eq!(lock_target(Some(&existing), "lefthook"), Some("lefthook"));
        assert_eq!(lock_target(None, "lefthook"), None);
        assert_eq!(lock_target(Some(&Lockfile::default()), "lefthook"), None);

        let fresh = fresh_lockfile(Some(existing));

        assert_eq!(fresh.binloom.as_ref().unwrap().version, "0.1.0");
        assert_eq!(fresh.wrapper.as_ref().unwrap().version, "0.1.1");
        assert!(fresh.tools.is_empty());
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
        let release = release::Release {
            tag: "v1.0.0".to_owned(),
            published_at: None,
            assets: vec![release::ReleaseAsset {
                name: "tool.gz".to_owned(),
                download_url: "https://example.com/tool.gz".to_owned(),
                sha256: Some(checksum.clone()),
            }],
        };
        let client = Client::builder().build().unwrap();

        let resolved = resolve_checksum(&client, &release, &release.assets[0]).unwrap();

        assert_eq!(resolved, (checksum, ChecksumSource::Digest));
    }

    #[test]
    fn records_sidecar_checksum_provenance() {
        let checksum = "b".repeat(64);
        let server = Server::start(vec![Response {
            status: 200,
            body: format!("{checksum}  tool.gz\n").into_bytes(),
        }]);

        let release = release::Release {
            tag: "v1.0.0".to_owned(),
            published_at: None,
            assets: vec![
                release::ReleaseAsset {
                    name: "tool.gz".to_owned(),
                    download_url: format!("{}/tool.gz", server.url()),
                    sha256: None,
                },
                release::ReleaseAsset {
                    name: "tool.gz.sha256".to_owned(),
                    download_url: format!("{}/tool.gz.sha256", server.url()),
                    sha256: None,
                },
            ],
        };
        let client = Client::builder().build().unwrap();

        let resolved = resolve_checksum(&client, &release, &release.assets[0]).unwrap();

        assert_eq!(resolved, (checksum, ChecksumSource::Sidecar));
        assert_eq!(server.requests()[0].path, "/tool.gz.sha256");
    }

    #[test]
    fn records_downloaded_checksum_provenance() {
        let server = Server::start(vec![Response {
            status: 200,
            body: b"hello".to_vec(),
        }]);

        let release = release::Release {
            tag: "v1.0.0".to_owned(),
            published_at: None,
            assets: vec![release::ReleaseAsset {
                name: "tool.gz".to_owned(),
                download_url: format!("{}/tool.gz", server.url()),
                sha256: None,
            }],
        };
        let client = Client::builder().build().unwrap();

        let resolved = resolve_checksum(&client, &release, &release.assets[0]).unwrap();

        assert_eq!(
            resolved,
            (
                "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".to_owned(),
                ChecksumSource::Download,
            )
        );
        assert_eq!(server.requests()[0].path, "/tool.gz");
    }
}
