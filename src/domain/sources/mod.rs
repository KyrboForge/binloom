use crate::download::Client;
use anyhow::{Result, bail};
use serde::Deserialize;
use std::fmt::{self, Display, Formatter};

pub(crate) mod release;

use release::Release;
mod cargo;
mod github;
mod gitlab;

pub(crate) use {cargo::CargoSource, github::GithubSource, gitlab::GitlabSource};

pub(crate) trait ReleaseProvider {
    fn fetch_release(&self, client: &Client, name: &str, version: &str) -> Result<Release>;

    fn fetch_latest_release(&self, client: &Client) -> Result<Release>;
}

fn release_tags(name: &str, version: &str) -> Vec<String> {
    if version.starts_with('v') {
        vec![version.to_owned(), format!("{name}-{version}")]
    } else {
        vec![
            format!("v{version}"),
            version.to_owned(),
            format!("{name}-{version}"),
        ]
    }
}

#[derive(Debug, Deserialize)]
#[serde(try_from = "String")]
pub(crate) enum Source {
    GitHub(GithubSource),
    GitLab(GitlabSource),
    Cargo(CargoSource),
}

impl Source {
    pub(crate) fn release_provider(&self) -> Result<&dyn ReleaseProvider> {
        match self {
            Self::GitHub(source) => Ok(source),
            Self::GitLab(source) => Ok(source),
            Self::Cargo(_) => bail!("Cargo is not a release source"),
        }
    }
}

impl TryFrom<String> for Source {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.split_once(':').map(|(kind, _)| kind) {
            Some("github") => GithubSource::try_from(value).map(Self::GitHub),
            Some("gitlab") => GitlabSource::try_from(value).map(Self::GitLab),
            Some("cargo") => CargoSource::try_from(value).map(Self::Cargo),
            _ => Err("unsupported source; expected github:owner/repository, gitlab:group[/subgroup]/project, or cargo:package".to_owned()),
        }
    }
}

impl Display for Source {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::GitHub(source) => source.fmt(formatter),
            Self::GitLab(source) => source.fmt(formatter),
            Self::Cargo(source) => source.fmt(formatter),
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
            "unsupported source; expected github:owner/repository, gitlab:group[/subgroup]/project, or cargo:package"
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

    #[test]
    fn builds_common_release_tags() {
        assert_eq!(
            release_tags("cargo-nextest", "0.9.143"),
            ["v0.9.143", "0.9.143", "cargo-nextest-0.9.143"]
        );
    }

    #[test]
    fn builds_release_tags_for_v_prefixed_version() {
        assert_eq!(release_tags("tool", "v1.2.3"), ["v1.2.3", "tool-v1.2.3"]);
    }

    #[test]
    fn parses_and_displays_cargo_source() {
        let source = Source::try_from("cargo:cargo-nextest".to_owned()).unwrap();

        assert_eq!(source.to_string(), "cargo:cargo-nextest");
    }
}
