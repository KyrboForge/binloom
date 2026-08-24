use crate::sources::Source;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::{collections::BTreeMap, fs, path::Path};
use toml_edit::{DocumentMut, Item, Value};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    #[serde(rename = "manifest-version")]
    pub version: u32,

    pub binloom: Binloom,

    #[serde(default)]
    pub tools: BTreeMap<String, Tool>,

    #[serde(default)]
    pub update: UpdateConfig,
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
    pub source: Source,
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

pub fn update_versions<'a>(
    path: &Path,
    binloom: Option<&str>,
    tools: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<()> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut document = content
        .parse::<DocumentMut>()
        .with_context(|| format!("failed to edit {}", path.display()))?;

    if let Some(version) = binloom {
        set_version(&mut document["binloom"]["version"], version)?;
    }
    for (name, version) in tools {
        set_version(&mut document["tools"][name]["version"], version)?;
    }

    fs::write(path, document.to_string())
        .with_context(|| format!("failed to write {}", path.display()))
}

fn set_version(item: &mut Item, version: &str) -> Result<()> {
    let value = item
        .as_value_mut()
        .context("manifest version must be a string")?;
    if !matches!(value, Value::String(_)) {
        bail!("manifest version must be a string");
    }

    let decor = value.decor().clone();
    *value = Value::from(version);
    *value.decor_mut() = decor;
    Ok(())
}
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct UpdateConfig {
    pub minimum_release_age_minutes: u64,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            minimum_release_age_minutes: 24 * 60,
        }
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
        assert_eq!(manifest.update.minimum_release_age_minutes, 24 * 60);

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
    fn updates_versions_without_losing_comments() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("binloom.toml");

        fs::write(
            &path,
            r#"# project tools
manifest-version = 1

[binloom]
version = "0.1.0" # wrapper

[tools.lefthook]
version = "2.1.10" # hooks
source = "github:evilmartians/lefthook"
"#,
        )
        .unwrap();

        update_versions(&path, Some("0.1.1"), [("lefthook", "2.1.11")]).unwrap();

        let content = fs::read_to_string(path).unwrap();
        assert!(content.contains("# project tools"));
        assert!(content.contains("version = \"0.1.1\" # wrapper"));
        assert!(content.contains("version = \"2.1.11\" # hooks"));
    }
}
