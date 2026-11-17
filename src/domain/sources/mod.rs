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

/// Strips the `{name}-` and `v` release-tag prefixes so `v4.54.3`, `jq-1.8.2`
/// and `jq-v1.8.2` all yield a bare version. The tool name is stripped first
/// so a tool called `vtool` keeps its digits; `release_tags` still tries the
/// prefixed spellings when resolving.
pub(crate) fn bare_version<'a>(name: &str, version: &'a str) -> &'a str {
    let version = version
        .strip_prefix(name)
        .and_then(|rest| rest.strip_prefix('-'))
        .unwrap_or(version);
    version
        .strip_prefix('v')
        .filter(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        .unwrap_or(version)
}

/// Only tags that strip back to the same bare version are tried, so the locked
/// version matches the manifest (`nightly` never resolves `vnightly`), while a
/// `v0.3.2` Binloom pin still finds its tag.
pub(crate) fn release_tags(name: &str, version: &str) -> Vec<String> {
    let bare = bare_version(name, version);
    [
        format!("v{version}"),
        version.to_owned(),
        format!("{name}-{version}"),
        format!("{name}-v{version}"),
    ]
    .into_iter()
    .filter(|tag| bare_version(name, tag) == bare)
    .collect()
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
            [
                "v0.9.143",
                "0.9.143",
                "cargo-nextest-0.9.143",
                "cargo-nextest-v0.9.143"
            ]
        );
    }

    #[test]
    fn builds_only_unprefixed_release_tags_for_nonnumeric_versions() {
        assert_eq!(release_tags("tool", "nightly"), ["nightly", "tool-nightly"]);
        assert_eq!(
            release_tags("binloom", "v0.3.2"),
            ["v0.3.2", "binloom-v0.3.2"]
        );
        assert_eq!(
            release_tags("tool", "vnightly"),
            ["vnightly", "tool-vnightly"]
        );
        assert_eq!(
            release_tags("vtool", "1.2.3"),
            ["v1.2.3", "1.2.3", "vtool-1.2.3", "vtool-v1.2.3"]
        );
    }

    #[test]
    fn strips_release_tag_prefixes_from_versions() {
        assert_eq!(bare_version("yq", "v4.54.3"), "4.54.3");
        assert_eq!(bare_version("jq", "jq-1.8.2"), "1.8.2");
        assert_eq!(bare_version("jq", "jq-v1.8.2"), "1.8.2");
        assert_eq!(bare_version("vtool", "vtool-1.2.3"), "1.2.3");
        assert_eq!(bare_version("vtool", "v1.2.3"), "1.2.3");
        assert_eq!(bare_version("yq", "4.54.3"), "4.54.3");
        assert_eq!(bare_version("tool", "vendor-1.0"), "vendor-1.0");
        assert_eq!(bare_version("tool", "v"), "v");
    }

    #[test]
    fn parses_and_displays_cargo_source() {
        let source = Source::try_from("cargo:cargo-nextest".to_owned()).unwrap();

        assert_eq!(source.to_string(), "cargo:cargo-nextest");
    }
}
