use std::collections::BTreeMap;
use std::path::Path;

use crate::lockfile::{ArtifactFormat, LockedArtifact, LockedTool, LockedWrapper, Lockfile};
use crate::manifest::{GithubSource, Tool};
use crate::{download, manifest, manifest::Manifest, platform::Platform, sources::github};
use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

pub fn update(tool_name: Option<&str>) -> Result<()> {
    update_tools(tool_name, true)
}

pub fn lock() -> Result<()> {
    update_tools(None, false)
}

fn update_tools(tool_name: Option<&str>, latest: bool) -> Result<()> {
    let manifest = Manifest::try_from(Path::new("binloom.toml"))?;

    let minimum_age = manifest.update.minimum_release_age_minutes;
    let lock_path = Path::new("binloom.lock");

    let (selected, mut lockfile): (Vec<(&str, &Tool)>, Lockfile) = match tool_name {
        Some(name) => {
            let tool = manifest
                .tools
                .get(name)
                .with_context(|| format!("tool {name} is not configured"))?;

            let lockfile = Lockfile::try_from(lock_path)
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
                Some(Lockfile::try_from(lock_path)?)
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
            resolve_latest_tool(name, &tool.source, minimum_age, &client)?
        } else {
            resolve_tool(name, &tool.version, &tool.source, minimum_age, &client)?
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

    if latest {
        manifest::update_versions(
            Path::new("binloom.toml"),
            binloom_version.as_deref(),
            versions
                .iter()
                .map(|(name, version)| (name.as_str(), version.as_str())),
        )?;
    }
    lockfile.write(lock_path)?;

    println!("Updated {}", lock_path.display());

    Ok(())
}

fn resolve_latest_tool(
    name: &str,
    source: &GithubSource,
    minimum_age_minutes: u64,
    client: &Client,
) -> Result<LockedTool> {
    let release = github::fetch_latest_release(client, source)?;
    resolve_release(name, source, &release, minimum_age_minutes, client)
}

fn resolve_tool(
    name: &str,
    version: &str,
    source: &GithubSource,
    minimum_age_minutes: u64,
    client: &Client,
) -> Result<LockedTool> {
    let release = github::fetch_release(client, source, version)?;
    resolve_release(name, source, &release, minimum_age_minutes, client)
}

fn resolve_checksum(
    client: &Client,
    release: &github::Release,
    asset: &github::ReleaseAsset,
) -> Result<String> {
    match github::checksum_from_release(client, release, asset)? {
        Some(checksum) => Ok(checksum),
        None => download::sha256_url(client, &asset.browser_download_url),
    }
}

fn resolve_release(
    name: &str,
    source: &GithubSource,
    release: &github::Release,
    minimum_age_minutes: u64,
    client: &Client,
) -> Result<LockedTool> {
    println!("Found {} for {name}:", release.tag_name);
    ensure_minimum_release_age(release, minimum_age_minutes, OffsetDateTime::now_utc())?;

    let mut artifacts = BTreeMap::new();
    for platform in Platform::ALL {
        let asset = github::find_asset(release, name, platform)?;
        let checksum = resolve_checksum(client, release, asset)?;
        artifacts.insert(
            platform.to_string(),
            LockedArtifact {
                asset: asset.name.clone(),
                url: asset.browser_download_url.clone(),
                sha256: checksum,
                format: ArtifactFormat::try_from(asset.name.as_str())?,
            },
        );
    }

    Ok(LockedTool {
        version: release
            .tag_name
            .strip_prefix('v')
            .unwrap_or(&release.tag_name)
            .to_owned(),
        source: source.to_string(),
        tag: release.tag_name.clone(),
        artifacts,
    })
}

fn resolve_latest_binloom(
    source: &GithubSource,
    minimum_age_minutes: u64,
    client: &Client,
) -> Result<(LockedTool, LockedWrapper)> {
    let release = github::fetch_latest_release(client, source)?;

    resolve_binloom_release(source, &release, minimum_age_minutes, client)
}

fn resolve_binloom_version(
    source: &GithubSource,
    version: &str,
    minimum_age_minutes: u64,
    client: &Client,
) -> Result<(LockedTool, LockedWrapper)> {
    let release = github::fetch_release(client, source, version)?;

    resolve_binloom_release(source, &release, minimum_age_minutes, client)
}

fn ensure_minimum_release_age(
    release: &github::Release,
    minimum_minutes: u64,
    now: OffsetDateTime,
) -> Result<()> {
    let published_at = release
        .published_at
        .as_deref()
        .with_context(|| format!("release {} has no publication date", release.tag_name))?;

    let published_at = OffsetDateTime::parse(published_at, &Rfc3339)
        .with_context(|| format!("release {} has invalid publication date", release.tag_name))?;

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
            release.tag_name
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
    let manifest = Manifest::try_from(Path::new("binloom.toml"))?;
    let lock_path = Path::new("binloom.lock");

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
        Lockfile::try_from(lock_path)?
    } else {
        Lockfile::default()
    };

    let version = locked.version.clone();
    lockfile.binloom = Some(locked);
    lockfile.wrapper = Some(wrapper);
    manifest::update_versions(
        lock_path.with_file_name("binloom.toml").as_path(),
        Some(&version),
        [],
    )?;
    lockfile.write(lock_path)?;

    println!("Updated Binloom in {}", lock_path.display());

    Ok(())
}

fn binloom_source() -> GithubSource {
    GithubSource {
        owner: "KyrboForge".to_owned(),
        repository: "binloom".to_owned(),
    }
}

fn resolve_binloom_release(
    source: &GithubSource,
    release: &github::Release,
    minimum_age_minutes: u64,
    client: &Client,
) -> Result<(LockedTool, LockedWrapper)> {
    let binloom = resolve_release("binloom", source, release, minimum_age_minutes, client)?;

    let asset = github::find_asset_by_name(release, "binloomw")?;
    let sha256 = resolve_checksum(client, release, asset)?;

    let wrapper = LockedWrapper {
        version: binloom.version.clone(),
        url: asset.browser_download_url.clone(),
        sha256,
    };

    Ok((binloom, wrapper))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enforces_minimum_release_age() {
        let release = github::Release {
            tag_name: "v1.0.0".to_owned(),
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
            }),
            binloom: Some(LockedTool {
                version: "0.1.0".to_owned(),
                source: "github:KyrboForge/binloom".to_owned(),
                tag: "v0.1.0".to_owned(),
                artifacts: BTreeMap::new(),
            }),
            ..Lockfile::default()
        };

        let fresh = fresh_lockfile(Some(existing));

        assert_eq!(fresh.binloom.as_ref().unwrap().version, "0.1.0");
        assert_eq!(fresh.wrapper.as_ref().unwrap().version, "0.1.1");
        assert!(fresh.tools.is_empty());
    }
}
