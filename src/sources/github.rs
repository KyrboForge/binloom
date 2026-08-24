use crate::sources::{
    ReleaseProvider,
    release::{Release, ReleaseAsset},
};
use anyhow::{Context, Result, bail, ensure};
use reqwest::{StatusCode, blocking::Client};
use serde::Deserialize;
use std::fmt;
use std::fmt::{Display, Formatter};

#[derive(Debug, Deserialize)]
pub struct GithubRelease {
    pub tag_name: String,
    pub assets: Vec<GithubReleaseAsset>,
    pub published_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GithubReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
    pub digest: Option<String>,
}

impl TryFrom<GithubReleaseAsset> for ReleaseAsset {
    type Error = anyhow::Error;

    fn try_from(value: GithubReleaseAsset) -> std::result::Result<Self, Self::Error> {
        Ok(Self {
            sha256: value.sha256_from_digest()?,
            name: value.name,
            download_url: value.browser_download_url,
        })
    }
}
impl TryFrom<GithubRelease> for Release {
    type Error = anyhow::Error;

    fn try_from(value: GithubRelease) -> std::result::Result<Self, Self::Error> {
        let assets = value
            .assets
            .into_iter()
            .map(ReleaseAsset::try_from)
            .collect::<Result<Vec<_>>>()?;

        Ok(Self {
            tag: value.tag_name,
            published_at: value.published_at,
            assets,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(try_from = "String")]
pub struct GithubSource {
    pub owner: String,
    pub repository: String,
}

impl TryFrom<String> for GithubSource {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let source = value
            .strip_prefix("github:")
            .ok_or_else(|| "source must use github:owner/repository".to_owned())?;

        let (owner, repository) = source
            .split_once('/')
            .ok_or_else(|| "source must use github:owner/repository".to_owned())?;

        if owner.is_empty()
            || repository.is_empty()
            || repository.contains('/')
            || value.chars().any(char::is_whitespace)
        {
            return Err("source must use github:owner/repository".to_owned());
        }

        Ok(Self {
            owner: owner.to_owned(),
            repository: repository.to_owned(),
        })
    }
}

impl Display for GithubSource {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "github:{}/{}", self.owner, self.repository)
    }
}

impl ReleaseProvider for GithubSource {
    fn fetch_release(&self, client: &Client, version: &str) -> Result<Release> {
        let tags = if version.starts_with('v') {
            vec![version.to_owned()]
        } else {
            vec![format!("v{version}"), version.to_owned()]
        };

        for tag in tags {
            let url = format!(
                "https://api.github.com/repos/{}/{}/releases/tags/{tag}",
                self.owner, self.repository
            );
            let mut request = client.get(&url);

            if let Ok(token) = std::env::var("GITHUB_TOKEN").or_else(|_| std::env::var("GH_TOKEN"))
            {
                request = request.bearer_auth(token);
            }

            let response = request
                .send()
                .with_context(|| format!("failed to fetch GitHub release {tag}"))?;

            if response.status() == StatusCode::NOT_FOUND {
                continue;
            }

            let release = response
                .error_for_status()
                .with_context(|| format!("GitHub rejected release request for {tag}"))?
                .json::<GithubRelease>()
                .with_context(|| format!("failed to parse GitHub release {tag}"))?;

            return release.try_into();
        }

        bail!(
            "release {} not found for {}/{}",
            version,
            self.owner,
            self.repository
        )
    }

    fn fetch_latest_release(&self, client: &Client) -> Result<Release> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/releases/latest",
            self.owner, self.repository
        );
        let mut request = client.get(&url);

        if let Ok(token) = std::env::var("GITHUB_TOKEN").or_else(|_| std::env::var("GH_TOKEN")) {
            request = request.bearer_auth(token);
        }

        let release = request
            .send()
            .context("failed to fetch latest GitHub release")?
            .error_for_status()
            .context("GitHub rejected latest release request")?
            .json::<GithubRelease>()
            .context("failed to parse latest GitHub release")?;

        release.try_into()
    }
}
impl GithubReleaseAsset {
    fn sha256_from_digest(&self) -> Result<Option<String>> {
        let Some(digest) = self.digest.as_deref() else {
            return Ok(None);
        };

        let checksum = digest
            .strip_prefix("sha256:")
            .with_context(|| format!("unsupported digest for {}", self.name))?;

        ensure!(
            checksum.len() == 64
                && checksum
                    .chars()
                    .all(|character| character.is_ascii_hexdigit()),
            "invalid SHA-256 digest for {}",
            self.name
        );

        Ok(Some(checksum.to_ascii_lowercase()))
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> GithubReleaseAsset {
        GithubReleaseAsset {
            name: name.to_owned(),
            browser_download_url: format!("https://example.com/{name}"),
            digest: None,
        }
    }

    #[test]
    fn reads_sha256_from_github_digest() {
        let mut release_asset = asset("tool_linux_x86_64.gz");
        release_asset.digest = Some(format!("sha256:{}", "a".repeat(64)));

        assert_eq!(
            release_asset.sha256_from_digest().unwrap(),
            Some("a".repeat(64))
        );
    }

    #[test]
    fn converts_github_release_to_neutral_release() {
        let mut release_asset = asset("tool_linux_x86_64.gz");
        release_asset.digest = Some(format!("sha256:{}", "A".repeat(64)));
        let release = GithubRelease {
            tag_name: "v1.2.3".to_owned(),
            published_at: Some("2026-01-01T00:00:00Z".to_owned()),
            assets: vec![release_asset],
        };

        let release = Release::try_from(release).unwrap();

        assert_eq!(release.tag, "v1.2.3");
        assert_eq!(
            release.published_at.as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
        assert_eq!(release.assets[0].name, "tool_linux_x86_64.gz");
        assert_eq!(
            release.assets[0].download_url,
            "https://example.com/tool_linux_x86_64.gz"
        );
        assert_eq!(release.assets[0].sha256, Some("a".repeat(64)));
    }

    #[test]
    fn rejects_unsupported_github_digest() {
        let mut release_asset = asset("tool_linux_x86_64.gz");
        release_asset.digest = Some(format!("sha1:{}", "a".repeat(40)));

        let error = ReleaseAsset::try_from(release_asset).unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported digest for tool_linux_x86_64.gz"
        );
    }
}
