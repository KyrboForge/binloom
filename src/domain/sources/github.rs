use crate::domain::sources::{
    ReleaseProvider,
    release::{Release, ReleaseAsset},
};
use crate::download::Client;
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use std::fmt;
use std::fmt::{Display, Formatter};

const GITHUB_API_URL: &str = "https://api.github.com";

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    assets: Vec<GithubReleaseAsset>,
    published_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubReleaseAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
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
pub(crate) struct GithubSource {
    owner: String,
    repository: String,
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

fn github_token() -> Option<String> {
    std::env::var("GITHUB_TOKEN")
        .or_else(|_| std::env::var("GH_TOKEN"))
        .ok()
}

fn authed_request(
    client: &Client,
    url: &str,
    token: Option<&str>,
) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
    let request = client.get(url);

    match token {
        Some(token) => request.header("Authorization", format!("Bearer {token}")),
        None => request,
    }
}

impl ReleaseProvider for GithubSource {
    fn fetch_release(&self, client: &Client, version: &str) -> Result<Release> {
        let token = github_token();

        self.fetch_release_from(client, version, GITHUB_API_URL, token.as_deref())
    }

    fn fetch_latest_release(&self, client: &Client) -> Result<Release> {
        let token = github_token();

        self.fetch_latest_release_from(client, GITHUB_API_URL, token.as_deref())
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

impl GithubSource {
    fn fetch_latest_release_from(
        &self,
        client: &Client,
        api_url: &str,
        token: Option<&str>,
    ) -> Result<Release> {
        let url = format!(
            "{api_url}/repos/{}/{}/releases/latest",
            self.owner, self.repository
        );
        let request = authed_request(client, &url, token);

        let mut response = request
            .call()
            .context("failed to fetch latest GitHub release")?;

        response
            .body_mut()
            .read_json::<GithubRelease>()
            .context("failed to parse latest GitHub release")?
            .try_into()
    }

    fn fetch_release_from(
        &self,
        client: &Client,
        version: &str,
        api_url: &str,
        token: Option<&str>,
    ) -> Result<Release> {
        let tags = if version.starts_with('v') {
            vec![version.to_owned()]
        } else {
            vec![format!("v{version}"), version.to_owned()]
        };

        for tag in tags {
            let url = format!(
                "{api_url}/repos/{}/{}/releases/tags/{tag}",
                self.owner, self.repository
            );
            let request = authed_request(client, &url, token);

            let mut response = match request.call() {
                Ok(response) => response,
                Err(ureq::Error::StatusCode(404)) => continue,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to fetch GitHub release {tag}"));
                }
            };

            let release = response
                .body_mut()
                .read_json::<GithubRelease>()
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download;
    use crate::http_fixture::{Response, Server};

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
    #[test]
    fn retries_tag_without_v_and_attaches_token() {
        let server = Server::start(vec![
            Response {
                status: 404,
                body: Vec::new(),
            },
            Response {
                status: 200,
                body: br#"{
                "tag_name": "1.2.3",
                "published_at": "2026-01-01T00:00:00Z",
                "assets": []
            }"#
                .to_vec(),
            },
        ]);

        let source = GithubSource::try_from("github:owner/repository".to_owned()).unwrap();
        let client = download::client();

        let release = source
            .fetch_release_from(&client, "1.2.3", server.url(), Some("secret-token"))
            .unwrap();

        assert_eq!(release.tag, "1.2.3");

        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].path,
            "/repos/owner/repository/releases/tags/v1.2.3"
        );
        assert_eq!(
            requests[1].path,
            "/repos/owner/repository/releases/tags/1.2.3"
        );
        assert!(
            requests
                .iter()
                .all(|request| request.authorization.as_deref() == Some("Bearer secret-token"))
        );
    }

    #[test]
    fn fetches_latest_release_and_attaches_token() {
        let server = Server::start(vec![Response {
            status: 200,
            body: br#"{
            "tag_name": "v1.2.3",
            "published_at": "2026-01-01T00:00:00Z",
            "assets": []
        }"#
            .to_vec(),
        }]);

        let source = GithubSource::try_from("github:owner/repository".to_owned()).unwrap();
        let client = download::client();

        let release = source
            .fetch_latest_release_from(&client, server.url(), Some("secret-token"))
            .unwrap();

        assert_eq!(release.tag, "v1.2.3");

        let requests = server.requests();

        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, "/repos/owner/repository/releases/latest");
        assert_eq!(
            requests[0].authorization.as_deref(),
            Some("Bearer secret-token")
        );
    }
}
