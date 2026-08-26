use crate::domain::sources::{
    ReleaseProvider,
    release::{Release, ReleaseAsset},
};
use crate::download::Client;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::fmt;
use std::fmt::{Display, Formatter};

const GITLAB_API_URL: &str = "https://gitlab.com/api/v4";

#[derive(Debug, Deserialize)]
struct GitlabRelease {
    tag_name: String,
    released_at: Option<String>,
    assets: GitlabAssets,
}

#[derive(Debug, Deserialize)]
struct GitlabAssets {
    #[serde(default)]
    links: Vec<GitlabReleaseAsset>,
}

#[derive(Debug, Deserialize)]
struct GitlabReleaseAsset {
    name: String,
    url: String,
    direct_asset_url: Option<String>,
}

impl From<GitlabReleaseAsset> for ReleaseAsset {
    fn from(value: GitlabReleaseAsset) -> Self {
        Self {
            name: value.name,
            download_url: value.direct_asset_url.unwrap_or(value.url),
            sha256: None,
        }
    }
}

impl From<GitlabRelease> for Release {
    fn from(value: GitlabRelease) -> Self {
        let assets = value
            .assets
            .links
            .into_iter()
            .map(ReleaseAsset::from)
            .collect();

        Self {
            tag: value.tag_name,
            published_at: value.released_at,
            assets,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(try_from = "String")]
pub(crate) struct GitlabSource {
    project: String,
}

impl TryFrom<String> for GitlabSource {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let project = value
            .strip_prefix("gitlab:")
            .ok_or_else(|| "source must use gitlab:group[/subgroup]/project".to_owned())?;

        let valid = project.contains('/')
            && project.split('/').all(|segment| {
                !segment.is_empty()
                    && segment.chars().all(|character| {
                        character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
                    })
            });

        if !valid {
            return Err("source must use gitlab:group[/subgroup]/project".to_owned());
        }

        Ok(Self {
            project: project.to_owned(),
        })
    }
}

impl Display for GitlabSource {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "gitlab:{}", self.project)
    }
}

fn gitlab_token() -> Option<String> {
    std::env::var("GITLAB_TOKEN").ok()
}

fn authed_request(
    client: &Client,
    url: &str,
    token: Option<&str>,
) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
    let request = client.get(url);

    match token {
        Some(token) => request.header("PRIVATE-TOKEN", token),
        None => request,
    }
}

impl ReleaseProvider for GitlabSource {
    fn fetch_release(&self, client: &Client, version: &str) -> Result<Release> {
        let token = gitlab_token();

        self.fetch_release_from(client, version, GITLAB_API_URL, token.as_deref())
    }

    fn fetch_latest_release(&self, client: &Client) -> Result<Release> {
        let token = gitlab_token();

        self.fetch_latest_release_from(client, GITLAB_API_URL, token.as_deref())
    }
}

impl GitlabSource {
    fn encoded_project(&self) -> String {
        self.project.replace('/', "%2F")
    }

    fn fetch_latest_release_from(
        &self,
        client: &Client,
        api_url: &str,
        token: Option<&str>,
    ) -> Result<Release> {
        let url = format!(
            "{api_url}/projects/{}/releases/permalink/latest",
            self.encoded_project()
        );
        let request = authed_request(client, &url, token);

        let mut response = request
            .call()
            .context("failed to fetch latest GitLab release")?;

        let release = response
            .body_mut()
            .read_json::<GitlabRelease>()
            .context("failed to parse latest GitLab release")?;

        Ok(release.into())
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
                "{api_url}/projects/{}/releases/{tag}",
                self.encoded_project(),
            );
            let request = authed_request(client, &url, token);

            let mut response = match request.call() {
                Ok(response) => response,
                Err(ureq::Error::StatusCode(404)) => continue,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to fetch GitLab release {tag}"));
                }
            };

            let release = response
                .body_mut()
                .read_json::<GitlabRelease>()
                .with_context(|| format!("failed to parse GitLab release {tag}"))?;

            return Ok(release.into());
        }

        bail!("release {} not found for {}", version, self.project,)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download;
    use crate::http_fixture::{Response, Server};

    fn asset(name: &str) -> GitlabReleaseAsset {
        GitlabReleaseAsset {
            name: name.to_owned(),
            url: format!("https://example.com/{name}"),
            direct_asset_url: None,
        }
    }

    #[test]
    fn rejects_invalid_gitlab_source() {
        let invalids = vec![
            "gitlab:group",
            "gitlab:/project",
            "gitlab:group/",
            "gitlab:group//project",
            "gitlab:group/project name",
            "gitlab:group/project?",
        ];
        for invalid in invalids {
            let source = GitlabSource::try_from(invalid.to_owned());
            assert_eq!(
                source.unwrap_err(),
                "source must use gitlab:group[/subgroup]/project"
            );
        }
    }

    #[test]
    fn converts_gitlab_release_to_neutral_release() {
        let release_asset = asset("tool_linux_x86_64.gz");
        let release = GitlabRelease {
            tag_name: "v1.2.3".to_owned(),
            released_at: Some("2026-01-01T00:00:00Z".to_owned()),
            assets: GitlabAssets {
                links: vec![release_asset],
            },
        };

        let release = Release::from(release);

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
        assert_eq!(release.assets[0].sha256, None);
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
                      "released_at": "2026-01-01T00:00:00Z",
                      "assets": {
                        "links": []
                      }
                    }"#
                .to_vec(),
            },
        ]);

        let source = GitlabSource::try_from("gitlab:owner/repository".to_owned()).unwrap();
        let client = download::client();

        let release = source
            .fetch_release_from(&client, "1.2.3", server.url(), Some("secret-token"))
            .unwrap();

        assert_eq!(release.tag, "1.2.3");

        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].path,
            "/projects/owner%2Frepository/releases/v1.2.3"
        );
        assert_eq!(
            requests[1].path,
            "/projects/owner%2Frepository/releases/1.2.3"
        );
        assert!(
            requests
                .iter()
                .all(|request| request.private_token.as_deref() == Some("secret-token"))
        );
    }

    #[test]
    fn fetches_latest_release_and_attaches_token() {
        let server = Server::start(vec![Response {
            status: 200,
            body: br#"{
                  "tag_name": "1.2.3",
                  "released_at": "2026-01-01T00:00:00Z",
                  "assets": {
                    "links": []
                  }
                }"#
            .to_vec(),
        }]);

        let source = GitlabSource::try_from("gitlab:owner/repository".to_owned()).unwrap();
        let client = download::client();

        let release = source
            .fetch_latest_release_from(&client, server.url(), Some("secret-token"))
            .unwrap();

        assert_eq!(release.tag, "1.2.3");

        let requests = server.requests();

        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].path,
            "/projects/owner%2Frepository/releases/permalink/latest"
        );
        assert_eq!(requests[0].private_token.as_deref(), Some("secret-token"));
    }

    #[test]
    fn prefers_direct_asset_url() {
        let mut asset = asset("tool.gz");
        asset.direct_asset_url = Some("https://gitlab.example/direct/tool.gz".to_owned());

        let asset = ReleaseAsset::from(asset);

        assert_eq!(asset.download_url, "https://gitlab.example/direct/tool.gz");
    }
}
