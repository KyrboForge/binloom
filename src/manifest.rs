use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fmt::{self, Display, Formatter},
    fs,
    path::Path,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    #[serde(rename = "manifest-version")]
    pub version: u32,

    pub binloom: Binloom,

    #[serde(default)]
    pub tools: BTreeMap<String, Tool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binloom {
    pub version: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    pub version: String,
    pub source: GithubSource,
    pub asset: Option<String>,
}

impl TryFrom<&Path> for Manifest {
    type Error = anyhow::Error;

    fn try_from(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;

        let manifest: Self = toml::from_str(&content)
            .with_context(|| format!("failed to parse {}", path.display()))?;

        if manifest.version != 1 {
            bail!("unsupported manifest version: {}", manifest.version);
        }
        Ok(manifest)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn loads_manifest_with_tools() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("binloom.toml");

        fs::write(
            &path,
            r#"manifest-version = 1

[binloom]
version = "0.1.0"

[tools.lefthook]
version = "2.1.10"
source = "github:evilmartians/lefthook"
asset = "lefthook_{version}_{os}_{arch}.gz"
"#,
        )
        .unwrap();

        let manifest = Manifest::try_from(path.as_path()).unwrap();

        assert_eq!(manifest.version, 1);
        assert_eq!(manifest.binloom.version, "0.1.0");

        let lefthook = &manifest.tools["lefthook"];

        assert_eq!(lefthook.version, "2.1.10");
        assert_eq!(lefthook.source.to_string(), "github:evilmartians/lefthook");
        assert_eq!(
            lefthook.asset.as_deref(),
            Some("lefthook_{version}_{os}_{arch}.gz")
        );
    }

    #[test]
    fn rejects_unsupported_manifest_version() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("binloom.toml");

        fs::write(
            &path,
            r#"manifest-version = 2

[binloom]
version = "0.1.0"
"#,
        )
        .unwrap();

        let error = Manifest::try_from(path.as_path()).unwrap_err();

        assert_eq!(error.to_string(), "unsupported manifest version: 2");
    }

    #[test]
    fn rejects_invalid_source() {
        let source = GithubSource::try_from("https://github.com/owner/repo".to_owned());

        assert_eq!(
            source.unwrap_err(),
            "source must use github:owner/repository"
        );
    }
}
