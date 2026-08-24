use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use tempfile::NamedTempFile;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Lockfile {
    #[serde(rename = "lock-version")]
    pub version: u32,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binloom: Option<LockedTool>,

    #[serde(default)]
    pub tools: BTreeMap<String, LockedTool>,
}

impl Default for Lockfile {
    fn default() -> Self {
        Self {
            version: 1,
            binloom: None,
            tools: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LockedTool {
    pub version: String,
    pub source: String,
    pub tag: String,
    pub artifacts: BTreeMap<String, LockedArtifact>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LockedArtifact {
    pub asset: String,
    pub url: String,
    pub sha256: String,
    pub format: ArtifactFormat,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactFormat {
    Raw,
    Gz,
}

impl TryFrom<&Lockfile> for String {
    type Error = anyhow::Error;

    fn try_from(lockfile: &Lockfile) -> Result<Self, Self::Error> {
        toml::to_string_pretty(lockfile).context("failed to serialize binloom.lock")
    }
}

impl TryFrom<&Path> for Lockfile {
    type Error = anyhow::Error;

    fn try_from(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;

        let lockfile: Self = toml::from_str(&content)
            .with_context(|| format!("failed to parse {}", path.display()))?;

        if lockfile.version != 1 {
            bail!("unsupported lockfile version: {}", lockfile.version);
        }

        Ok(lockfile)
    }
}

impl Lockfile {
    pub fn write(&self, path: &Path) -> Result<()> {
        let content = String::try_from(self)?;

        let parent = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));

        let mut temporary = NamedTempFile::new_in(parent).with_context(|| {
            format!(
                "failed to create temporary lockfile in {}",
                parent.display()
            )
        })?;

        temporary
            .write_all(content.as_bytes())
            .context("failed to write temporary lockfile")?;

        temporary
            .as_file()
            .sync_all()
            .context("failed to sync temporary lockfile")?;

        temporary
            .persist(path)
            .map_err(|error| error.error)
            .with_context(|| format!("failed to replace {}", path.display()))?;

        Ok(())
    }
}

impl TryFrom<&str> for ArtifactFormat {
    type Error = anyhow::Error;

    fn try_from(asset: &str) -> Result<Self> {
        let asset = asset.to_ascii_lowercase();

        if [".tar.gz", ".tgz", ".zip", ".tar.xz", ".tar.zst"]
            .iter()
            .any(|suffix| asset.ends_with(suffix))
        {
            bail!("unsupported asset format: {asset}");
        }

        if asset.ends_with(".gz") {
            Ok(Self::Gz)
        } else {
            Ok(Self::Raw)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_and_persists_lockfile() {
        let mut artifacts = BTreeMap::new();

        artifacts.insert(
            "linux-x86_64".to_owned(),
            LockedArtifact {
                asset: "lefthook_2.1.10_Linux_x86_64.gz".to_owned(),
                url: "https://example.com/lefthook.gz".to_owned(),
                sha256: "a".repeat(64),
                format: ArtifactFormat::Gz,
            },
        );

        let lockfile = Lockfile {
            binloom: Some(LockedTool {
                version: "0.1.0".to_owned(),
                source: "github:KyrboForge/binloom".to_owned(),
                tag: "v0.1.0".to_owned(),
                artifacts,
            }),
            ..Lockfile::default()
        };

        let first: String = (&lockfile).try_into().unwrap();
        let second: String = (&lockfile).try_into().unwrap();

        assert_eq!(first, second);
        assert!(first.contains("[tools.lefthook]"));
        assert!(first.contains("[tools.lefthook.artifacts.linux-x86_64]"));
        assert!(first.contains("format = \"gz\""));

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("binloom.lock");

        lockfile.write(&path).unwrap();

        let loaded = Lockfile::try_from(path.as_path()).unwrap();

        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.tools["lefthook"].version, "2.1.10");
        assert_eq!(
            loaded.tools["lefthook"].artifacts["linux-x86_64"].sha256,
            "a".repeat(64)
        );
    }

    #[test]
    fn roundtrips_locked_binloom_binary() {
        let mut artifacts = BTreeMap::new();

        artifacts.insert(
            "macos-aarch64".to_owned(),
            LockedArtifact {
                asset: "binloom_macos_aarch64.gz".to_owned(),
                url: "https://example.com/binloom.gz".to_owned(),
                sha256: "b".repeat(64),
                format: ArtifactFormat::Gz,
            },
        );

        let mut lockfile = Lockfile::default();

        lockfile.binloom = Some(LockedTool {
            version: "0.1.0".to_owned(),
            source: "github:KyrboForge/binloom".to_owned(),
            tag: "v0.1.0".to_owned(),
            artifacts,
        });

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("binloom.lock");

        lockfile.write(&path).unwrap();

        let loaded = Lockfile::try_from(path.as_path()).unwrap();
        let binloom = loaded.binloom.unwrap();

        assert_eq!(binloom.version, "0.1.0");
        assert_eq!(binloom.artifacts["macos-aarch64"].sha256, "b".repeat(64));
    }
}
