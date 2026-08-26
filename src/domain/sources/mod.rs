use crate::domain::sources::gitlab::GitlabSource;
use crate::domain::sources::{github::GithubSource, release::Release};
use crate::download::Client;
use anyhow::Result;
use serde::Deserialize;
use std::fmt::{self, Display, Formatter};

pub(crate) mod github;
pub(crate) mod gitlab;
pub(crate) mod release;

pub trait ReleaseProvider {
    fn fetch_release(&self, client: &Client, version: &str) -> Result<Release>;

    fn fetch_latest_release(&self, client: &Client) -> Result<Release>;
}

#[derive(Debug, Deserialize)]
#[serde(try_from = "String")]
pub enum Source {
    GitHub(GithubSource),
    GitLab(GitlabSource),
}

impl Source {
    pub fn provider(&self) -> &dyn ReleaseProvider {
        match self {
            Self::GitHub(source) => source,
            Self::GitLab(source) => source,
        }
    }
}

impl TryFrom<String> for Source {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.split_once(':').map(|(kind, _)| kind) {
            Some("github") => GithubSource::try_from(value).map(Self::GitHub),
            Some("gitlab") => GitlabSource::try_from(value).map(Self::GitLab),
            _ => Err("unsupported source; expected github:owner/repository or gitlab:group[/subgroup]/project".to_owned()),
        }
    }
}

impl Display for Source {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::GitHub(source) => source.fmt(formatter),
            Self::GitLab(source) => source.fmt(formatter),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_displays_github_source() {
        let source = Source::try_from("github:owner/repository".to_owned()).unwrap();

        assert_eq!(source.to_string(), "github:owner/repository");
    }

    #[test]
    fn rejects_invalid_source() {
        let source = Source::try_from("https://github.com/owner/repo".to_owned());

        assert_eq!(
            source.unwrap_err(),
            "unsupported source; expected github:owner/repository or gitlab:group[/subgroup]/project"
        );
    }
    #[test]
    fn parses_and_displays_nested_gitlab_source() {
        let source = Source::try_from("gitlab:group/subgroup/project".to_owned()).unwrap();

        assert_eq!(source.to_string(), "gitlab:group/subgroup/project");
    }

    #[test]
    fn parses_and_displays_gitlab_source() {
        let source = Source::try_from("gitlab:group/project".to_owned()).unwrap();
        assert_eq!(source.to_string(), "gitlab:group/project");
    }
}
