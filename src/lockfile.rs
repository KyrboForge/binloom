use crate::common::{validate_tool_name, validate_version};
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

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapper: Option<LockedWrapper>,
}

impl Default for Lockfile {
    fn default() -> Self {
        Self {
            version: 1,
            binloom: None,
            tools: BTreeMap::new(),
            wrapper: None,
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

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChecksumSource {
    Digest,
    Sidecar,
    Download,

    #[default]
    Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LockedArtifact {
    pub asset: String,
    pub url: String,
    pub sha256: String,
    pub format: ArtifactFormat,
    #[serde(default, rename = "checksum-source")]
    pub checksum_source: ChecksumSource,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactFormat {
    Raw,
    Gz,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LockedWrapper {
    pub version: String,
    pub url: String,
    pub sha256: String,
    #[serde(default, rename = "checksum-source")]
    pub checksum_source: ChecksumSource,
}

impl TryFrom<&Lockfile> for String {
    type Error = anyhow::Error;

    fn try_from(lockfile: &Lockfile) -> Result<Self, Self::Error> {
        lockfile.validate()?;
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

        lockfile.validate()?;
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

    fn validate(&self) -> Result<()> {
        if let Some(binloom) = &self.binloom {
            validate_version(&binloom.version).context("invalid Binloom version in lockfile")?;
        }

        if let Some(wrapper) = &self.wrapper {
            validate_version(&wrapper.version).context("invalid wrapper version in lockfile")?;
        }

        for (name, tool) in &self.tools {
            validate_tool_name(name)?;
            validate_version(&tool.version)
                .with_context(|| format!("invalid version for tool {name} in lockfile"))?;
        }

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
                checksum_source: ChecksumSource::Digest,
            },
        );
        let mut lockfile = Lockfile::default();

        lockfile.tools.insert(
            "lefthook".to_owned(),
            LockedTool {
                version: "2.1.10".to_owned(),
                source: "github:evilmartians/lefthook".to_owned(),
                tag: "v2.1.10".to_owned(),
                artifacts,
            },
        );

        let first: String = (&lockfile).try_into().unwrap();
        let second: String = (&lockfile).try_into().unwrap();

        assert_eq!(first, second);
        assert!(first.contains("[tools.lefthook]"));
        assert!(first.contains("[tools.lefthook.artifacts.linux-x86_64]"));
        assert!(first.contains("format = \"gz\""));
        assert!(first.contains("checksum-source = \"digest\""));

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("binloom.lock");

        lockfile.write(&path).unwrap();

        let loaded = Lockfile::try_from(path.as_path()).unwrap();
        let legacy_content = first.replace("checksum-source = \"digest\"\n", "");
        let legacy: Lockfile = toml::from_str(&legacy_content).unwrap();

        assert_eq!(
            legacy.tools["lefthook"].artifacts["linux-x86_64"].checksum_source,
            ChecksumSource::Unknown
        );
        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.tools["lefthook"].version, "2.1.10");
        assert_eq!(
            loaded.tools["lefthook"].artifacts["linux-x86_64"].sha256,
            "a".repeat(64)
        );
        assert_eq!(
            loaded.tools["lefthook"].artifacts["linux-x86_64"].checksum_source,
            ChecksumSource::Digest
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
                checksum_source: ChecksumSource::Digest,
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

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("binloom.lock");

        lockfile.write(&path).unwrap();

        let loaded = Lockfile::try_from(path.as_path()).unwrap();
        let binloom = loaded.binloom.unwrap();

        assert_eq!(binloom.version, "0.1.0");
        assert_eq!(binloom.artifacts["macos-aarch64"].sha256, "b".repeat(64));
    }

    #[test]
    fn serializes_wrapper_metadata() {
        let lockfile = Lockfile {
            wrapper: Some(LockedWrapper {
                version: "0.2.0".to_owned(),
                url: "https://example.com/binloomw".to_owned(),
                sha256: "a".repeat(64),
                checksum_source: ChecksumSource::Digest,
            }),
            ..Lockfile::default()
        };

        let content = String::try_from(&lockfile).unwrap();

        assert!(content.contains("[wrapper]"));
        assert!(content.contains("version = \"0.2.0\""));
        assert!(content.contains("url = \"https://example.com/binloomw\""));
        assert!(content.contains(&format!("sha256 = \"{}\"", "a".repeat(64))));
    }
    #[test]
    fn rejects_unsafe_lockfile_components() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("binloom.lock");

        let invalid_lockfiles = [
            r#"lock-version = 1

[tools."../../x"]
version = "1.0.0"
source = "github:owner/repo"
tag = "v1.0.0"
artifacts = {}
"#,
            r#"lock-version = 1

[tools.example]
version = "/tmp/x"
source = "github:owner/repo"
tag = "v1.0.0"
artifacts = {}
"#,
            r#"lock-version = 1

[binloom]
version = "../../x"
source = "github:KyrboForge/binloom"
tag = "v0.2.2"
artifacts = {}
"#,
        ];

        for content in invalid_lockfiles {
            fs::write(&path, content).unwrap();
            assert!(Lockfile::try_from(path.as_path()).is_err(), "{content}");
        }
    }

    #[test]
    fn refuses_to_serialize_unsafe_components() {
        let locked_tool = |version: &str| LockedTool {
            version: version.to_owned(),
            source: "github:owner/repo".to_owned(),
            tag: "v1.0.0".to_owned(),
            artifacts: BTreeMap::new(),
        };

        let mut lockfile = Lockfile::default();
        lockfile
            .tools
            .insert("example".to_owned(), locked_tool("../../x"));

        assert!(String::try_from(&lockfile).is_err());

        lockfile.tools.clear();
        lockfile
            .tools
            .insert("../../x".to_owned(), locked_tool("1.0.0"));

        assert!(String::try_from(&lockfile).is_err());
    }
}
