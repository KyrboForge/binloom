use crate::{
    common::{
        newer_binloom_hint, parse_versioned_toml, validate_tool_name, validate_version,
        write_manifest_and_lock, write_new_atomically,
    },
    domain::{
        platform::{Platform, canonical_targets},
        sources::Source,
    },
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::{fs, io, ops::Range};
use toml_edit::{Document, Table};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Lockfile {
    #[serde(rename = "lock-version")]
    pub(crate) version: u32,

    /// Declared project targets; absent in legacy locks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) platforms: Option<LockedPlatforms>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) binloom: Option<LockedTool>,

    #[serde(default)]
    pub(crate) tools: BTreeMap<String, LockedTool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) wrapper: Option<LockedWrapper>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LockedPlatforms {
    pub(crate) targets: Vec<Platform>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LockedTool {
    pub(crate) version: String,
    pub(crate) source: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tag: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) sha256: Option<String>,

    /// Effective targets of a tool in an explicit-target project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) targets: Option<Vec<Platform>>,

    /// Snapshot of the manifest asset pattern in an explicit-target project.
    #[serde(
        default,
        rename = "asset-pattern",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) asset_pattern: Option<String>,

    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        serialize_with = "serialize_artifacts"
    )]
    pub(crate) artifacts: BTreeMap<String, LockedArtifact>,
}

fn serialize_artifacts<S: serde::Serializer>(
    artifacts: &BTreeMap<String, LockedArtifact>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    if !Platform::ALL
        .iter()
        .any(|target| target.linux_abi().is_some() && artifacts.contains_key(&target.to_string()))
    {
        // Keep the original v1 artifact table ordering for existing locks.
        return artifacts.serialize(serializer);
    }
    let canonical = Platform::ALL.map(|target| target.to_string());
    let mut ordered: Vec<_> = artifacts.iter().collect();
    ordered.sort_by_key(|(key, _)| {
        canonical
            .iter()
            .position(|id| id == *key)
            .unwrap_or(canonical.len())
    });
    serializer.collect_map(ordered)
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ChecksumSource {
    Digest,
    Sidecar,
    Download,

    #[default]
    Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LockedArtifact {
    pub(crate) asset: String,
    pub(crate) url: String,
    pub(crate) sha256: String,
    pub(crate) format: ArtifactFormat,
    #[serde(default, rename = "checksum-source")]
    pub(crate) checksum_source: ChecksumSource,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ArtifactFormat {
    Raw,
    Gz,
    TarGz,
    TarXz,
    Zip,
    TarZst,
    TarBz2,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LockedWrapper {
    pub(crate) version: String,
    pub(crate) url: String,
    pub(crate) sha256: String,
    #[serde(default, rename = "checksum-source")]
    pub(crate) checksum_source: ChecksumSource,
}

#[derive(Debug, Eq, PartialEq)]
struct Toml(String);

impl Default for Lockfile {
    fn default() -> Self {
        Self {
            version: 1,
            platforms: None,
            binloom: None,
            tools: BTreeMap::new(),
            wrapper: None,
        }
    }
}

impl Toml {
    fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl TryFrom<&Lockfile> for Toml {
    type Error = anyhow::Error;

    fn try_from(lockfile: &Lockfile) -> Result<Self, Self::Error> {
        lockfile.validate()?;
        toml::to_string_pretty(lockfile)
            .map(Toml)
            .context("failed to serialize binloom.lock")
    }
}

impl TryFrom<&Path> for Lockfile {
    type Error = anyhow::Error;

    fn try_from(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Self::parse(&content, path)
    }
}

impl Lockfile {
    fn parse(content: &str, path: &Path) -> Result<Self> {
        let mut lockfile: Self = parse_versioned_toml(content, path, "lock-version")?;

        if lockfile.version != 1 {
            return Err(
                anyhow::anyhow!("unsupported lockfile version: {}", lockfile.version)
                    .context(newer_binloom_hint(path)),
            );
        }

        lockfile.validate()?;
        // Compare locked targets with the manifest in canonical order.
        if let Some(platforms) = &mut lockfile.platforms {
            platforms.targets.sort();
        }
        for tool in lockfile.tools.values_mut() {
            if let Some(targets) = &mut tool.targets {
                targets.sort();
            }
        }
        Ok(lockfile)
    }
    #[cfg(test)]
    pub(crate) fn write(&self, path: &Path) -> Result<()> {
        let content = Toml::try_from(self)?;

        crate::common::write_atomically(path, content.as_bytes())
    }

    pub(crate) fn write_new(&self, path: &Path) -> Result<()> {
        let content = Toml::try_from(self)?;
        write_new_atomically(path, content.as_bytes())
    }

    pub(crate) fn write_with_manifest(&self, root: &Path, manifest: &str) -> Result<()> {
        let content = Toml::try_from(self)?;
        write_manifest_and_lock(root, manifest.as_bytes(), content.as_bytes())
    }

    pub(crate) fn write_bootstrap_with_manifest(
        root: &Path,
        manifest: &str,
        binloom: LockedTool,
        wrapper: LockedWrapper,
    ) -> Result<()> {
        let path = root.join(crate::common::LOCKFILE);
        let existing = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Self {
                    binloom: Some(binloom),
                    wrapper: Some(wrapper),
                    ..Self::default()
                }
                .write_with_manifest(root, manifest);
            }
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        Self::parse(&existing, &path)?;

        #[derive(Serialize)]
        struct Bootstrap {
            binloom: LockedTool,
            wrapper: LockedWrapper,
        }
        let bootstrap = toml::to_string_pretty(&Bootstrap { binloom, wrapper })
            .context("failed to serialize Binloom bootstrap lock entries")?;
        let updated = replace_bootstrap_sections(&existing, &bootstrap)?;
        Self::parse(&updated, &path)?;
        write_manifest_and_lock(root, manifest.as_bytes(), updated.as_bytes())
    }

    fn validate(&self) -> Result<()> {
        let project = self
            .platforms
            .as_ref()
            .map(|platforms| canonical_targets(&platforms.targets))
            .transpose()
            .context("invalid [platforms] targets in lockfile")?;

        if let Some(binloom) = &self.binloom {
            LockedTool::validate_locked_tool(binloom).context("invalid Binloom lock entry")?;
            ensure!(
                !binloom.source.starts_with("cargo:"),
                "Binloom cannot use a Cargo source"
            );
            ensure!(
                binloom.targets.is_none() && binloom.asset_pattern.is_none(),
                "Binloom lock entry must not have targets or an asset pattern"
            );
        }
        if let Some(binloom) = &self.binloom {
            validate_version(&binloom.version).context("invalid Binloom version in lockfile")?;
        }

        if let Some(wrapper) = &self.wrapper {
            validate_version(&wrapper.version).context("invalid wrapper version in lockfile")?;
        }

        for (name, tool) in &self.tools {
            validate_tool_name(name)?;
            LockedTool::validate_locked_tool(tool)
                .with_context(|| format!("invalid lock entry for tool {name}"))?;
            tool.validate_targets(project.as_deref())
                .with_context(|| format!("invalid lock entry for tool {name}"))?;
        }

        Ok(())
    }
}

fn replace_bootstrap_sections(content: &str, bootstrap: &str) -> Result<String> {
    let document = Document::parse(content).context("failed to edit binloom.lock")?;
    let mut spans = Vec::new();
    collect_bootstrap_spans(document.as_table(), true, &mut spans);
    spans.sort_by_key(|span| span.start);

    // Copy untouched bytes instead of re-rendering their TOML tables.
    let mut updated = String::new();
    let mut previous_end = 0;
    for span in spans {
        let start = content[..span.start]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        let gap = &content[previous_end..start];
        if previous_end == 0 || !gap.trim().is_empty() {
            updated.push_str(gap);
        }
        previous_end = content[span.end..]
            .find('\n')
            .map_or(content.len(), |index| span.end + index + 1);
    }
    updated.push_str(&content[previous_end..]);
    if !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(bootstrap);
    Ok(updated)
}

fn collect_bootstrap_spans(table: &Table, root: bool, spans: &mut Vec<Range<usize>>) {
    if !root && !table.is_implicit() {
        spans.extend(table.span());
    }
    if !table.is_dotted() {
        for (keys, value) in table.get_values() {
            if !root || matches!(keys[0].get(), "binloom" | "wrapper") {
                let key = keys
                    .last()
                    .and_then(|key| key.span())
                    .expect("parsed key has a span");
                let value = value.span().expect("parsed value has a span");
                spans.push(key.start..value.end);
            }
        }
    }
    for (key, item) in table.iter() {
        if (!root || matches!(key, "binloom" | "wrapper"))
            && let Some(child) = item.as_table()
        {
            collect_bootstrap_spans(child, false, spans);
        }
    }
}

impl TryFrom<&str> for ArtifactFormat {
    type Error = anyhow::Error;

    fn try_from(asset: &str) -> Result<Self> {
        let ends_with = |suffix: &str| {
            asset
                .get(asset.len().saturating_sub(suffix.len())..)
                .is_some_and(|ending| ending.eq_ignore_ascii_case(suffix))
        };

        if ends_with(".tar.gz") || ends_with(".tgz") {
            Ok(Self::TarGz)
        } else if ends_with(".tar.xz") || ends_with(".txz") {
            Ok(Self::TarXz)
        } else if ends_with(".zip") {
            Ok(Self::Zip)
        } else if ends_with(".tar.zst") || ends_with(".tar.zstd") {
            Ok(Self::TarZst)
        } else if ends_with(".tar.bz2") || ends_with(".tbz2") {
            Ok(Self::TarBz2)
        } else if ends_with(".gz") {
            Ok(Self::Gz)
        } else if [
            ".tar", ".xz", ".zst", ".zstd", ".bz2", ".tbz", ".7z", ".rar", ".lz4", ".lz", ".lzma", ".br", ".z",
        ]
        .iter()
        .any(|suffix| ends_with(suffix))
            // Any other `.tar.<compression>` is unsupported, so new compressions
            // never parse as raw executables.
            || asset.to_ascii_lowercase().contains(".tar.")
        {
            bail!(
                "unsupported asset format: {asset}; supported formats: raw, .gz, .tar.gz, .tgz, .tar.xz, .txz, .zip, .tar.zst, .tar.zstd, .tar.bz2, .tbz2"
            );
        } else {
            Ok(Self::Raw)
        }
    }
}

impl LockedTool {
    fn validate_targets(&self, project: Option<&[Platform]>) -> Result<()> {
        let Some(project) = project else {
            ensure!(
                self.targets.is_none() && self.asset_pattern.is_none(),
                "targets and asset-pattern require [platforms] targets"
            );
            return Ok(());
        };

        let targets = self
            .targets
            .as_deref()
            .context("tool is missing targets required by [platforms]")?;
        let targets = canonical_targets(targets).context("invalid targets")?;
        if let Some(target) = targets.iter().find(|target| !project.contains(target)) {
            bail!("target `{target}` is not in [platforms] targets");
        }
        if self.tag.is_some() {
            let expected: BTreeSet<String> = targets.iter().map(ToString::to_string).collect();
            ensure!(
                self.artifacts.keys().eq(expected.iter()),
                "release artifacts must match targets"
            );
        }

        Ok(())
    }

    fn validate_locked_tool(tool: &Self) -> Result<()> {
        validate_version(&tool.version)?;

        let source = Source::try_from(tool.source.clone()).map_err(anyhow::Error::msg)?;

        match source {
            Source::Cargo(_) => {
                ensure!(tool.tag.is_none(), "Cargo tool must not have a release tag");
                ensure!(
                    tool.artifacts.is_empty(),
                    "Cargo tool must not have release artifacts"
                );

                let sha256 = tool
                    .sha256
                    .as_deref()
                    .context("Cargo tool is missing SHA-256")?;

                ensure!(
                    sha256.len() == 64
                        && sha256
                            .chars()
                            .all(|character| character.is_ascii_hexdigit()),
                    "Cargo tool has invalid SHA-256"
                );
            }
            Source::GitHub(_) | Source::GitLab(_) => {
                ensure!(tool.tag.is_some(), "release tool is missing its tag");
                ensure!(
                    tool.sha256.is_none(),
                    "release tool must not have a top-level SHA-256"
                );
            }
        }

        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_only_bootstrap_lines_in_mixed_toml_layouts() {
        let original = concat!(
            "# lock\r\n",
            "binloom.version = '1.0.0'\r\n",
            "lock-version = 1\r\n",
            "binloom.source = 'github:KyrboForge/binloom'\r\n",
            "wrapper = { version = '1.0.0', url = 'https://example.invalid/old' }\r\n",
            "[ binloom . artifacts . 'linux-x86_64' ]\r\n",
            "format = 'raw'\r\n",
            "# Preserve everything belonging to the tool.\r\n",
            "[tools.demo]\r\n",
            "version = '2.0.0' # pin",
        );
        let bootstrap = "[binloom]\nversion = '1.1.0'\n\n[wrapper]\nversion = '1.1.0'\n";
        let expected = format!(
            "# lock\r\nlock-version = 1\r\n# Preserve everything belonging to the tool.\r\n[tools.demo]\r\nversion = '2.0.0' # pin\n{bootstrap}"
        );
        let updated = replace_bootstrap_sections(original, bootstrap).unwrap();
        assert_eq!(updated, expected);
        assert_eq!(
            replace_bootstrap_sections(&updated, bootstrap).unwrap(),
            expected
        );
    }

    #[test]
    fn future_lock_schema_errors_suggest_upgrading_binloom() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("binloom.lock");
        let unknown_variant = format!(
            "lock-version = 1\n[wrapper]\nversion = '1.0.0'\nurl = 'https://example.invalid/binloomw'\nsha256 = '{}'\nchecksum-source = 'future-checksum'\n",
            "a".repeat(64)
        );
        for (content, cause) in [
            ("lock-version = 1\nfuture-field = true\n", "unknown field"),
            (unknown_variant.as_str(), "unknown variant"),
            ("lock-version = 2\n", "unsupported lockfile version: 2"),
        ] {
            fs::write(&path, content).unwrap();
            let error = format!("{:#}", Lockfile::try_from(path.as_path()).unwrap_err());
            assert!(error.contains(cause), "{error}");
            assert!(error.contains("binloom.lock"), "{error}");
            assert!(error.contains(env!("CARGO_PKG_VERSION")), "{error}");
            assert!(error.contains("./binloomw update --self"), "{error}");
        }
    }

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
                tag: Some("v2.1.10".to_owned()),
                sha256: None,
                artifacts,
                ..LockedTool::default()
            },
        );

        let first: Toml = (&lockfile).try_into().unwrap();
        let second: Toml = (&lockfile).try_into().unwrap();

        assert_eq!(first, second);
        assert!(first.0.contains("[tools.lefthook]"));
        assert!(first.0.contains("[tools.lefthook.artifacts.linux-x86_64]"));
        assert!(first.0.contains("format = \"gz\""));
        assert!(first.0.contains("checksum-source = \"digest\""));

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("binloom.lock");

        lockfile.write(&path).unwrap();

        let loaded = Lockfile::try_from(path.as_path()).unwrap();
        let legacy_content = first.0.replace("checksum-source = \"digest\"\n", "");
        let legacy: Lockfile = toml::from_str(legacy_content.as_str()).unwrap();

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
                tag: Some("v0.1.0".to_owned()),
                sha256: None,
                artifacts,
                ..LockedTool::default()
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

        let content = Toml::try_from(&lockfile).unwrap();

        assert!(content.0.contains("[wrapper]"));
        assert!(content.0.contains("version = \"0.2.0\""));
        assert!(content.0.contains("url = \"https://example.com/binloomw\""));
        assert!(
            content
                .0
                .contains(&format!("sha256 = \"{}\"", "a".repeat(64)))
        );
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
            tag: Some("v1.0.0".to_owned()),
            sha256: None,
            artifacts: BTreeMap::new(),
            ..LockedTool::default()
        };

        let mut lockfile = Lockfile::default();
        lockfile
            .tools
            .insert("example".to_owned(), locked_tool("../../x"));

        assert!(Toml::try_from(&lockfile).is_err());

        lockfile.tools.clear();
        lockfile
            .tools
            .insert("../../x".to_owned(), locked_tool("1.0.0"));

        assert!(Toml::try_from(&lockfile).is_err());
    }

    #[test]
    fn detects_artifact_formats_case_insensitively() {
        assert!(matches!(
            ArtifactFormat::try_from("tool.TAR.GZ").unwrap(),
            ArtifactFormat::TarGz
        ));
        assert!(matches!(
            ArtifactFormat::try_from("tool.GZ").unwrap(),
            ArtifactFormat::Gz
        ));
        assert!(matches!(
            ArtifactFormat::try_from("tool").unwrap(),
            ArtifactFormat::Raw
        ));
        assert!(matches!(
            ArtifactFormat::try_from("tool-darwin-arm64.tgz").unwrap(),
            ArtifactFormat::TarGz
        ));
        for (asset, expected) in [
            ("tool.ZIP", "zip"),
            ("tool.TAR.XZ", "tar-xz"),
            ("tool.TAR.ZST", "tar-zst"),
            ("tool.TAR.BZ2", "tar-bz2"),
            ("tool.txz", "tar-xz"),
            ("tool.tar.zstd", "tar-zst"),
            ("tool.tbz2", "tar-bz2"),
            ("shellcheck-v0.11.0.darwin.aarch64.tar.xz", "tar-xz"),
            ("tool.tar.gz", "tar-gz"),
        ] {
            let format = ArtifactFormat::try_from(asset).unwrap();
            let serialized = toml::Value::try_from(format).unwrap();
            assert_eq!(serialized.as_str(), Some(expected), "{asset}");
            let decoded: ArtifactFormat = serialized.try_into().unwrap();
            assert_eq!(format!("{decoded:?}"), format!("{format:?}"));
        }
        assert!(ArtifactFormat::try_from("tool.zstd").is_err());
        for unsupported in [
            "tool.tar.lz4",
            "tool.tbz",
            "tool.tar.LZMA",
            "tool.tar.br",
            "tool.lz4",
            "tool.Z",
        ] {
            assert!(
                ArtifactFormat::try_from(unsupported).is_err(),
                "{unsupported}"
            );
        }
        let error = ArtifactFormat::try_from("tool.7z").unwrap_err().to_string();
        assert!(error.contains("unsupported asset format: tool.7z"));
        for format in [
            "raw",
            ".gz",
            ".tar.gz",
            ".tgz",
            ".tar.xz",
            ".txz",
            ".zip",
            ".tar.zst",
            ".tar.zstd",
            ".tar.bz2",
            ".tbz2",
        ] {
            assert!(error.contains(format), "{error}");
        }
    }

    #[test]
    fn serializes_cargo_tool() {
        let mut lockfile = Lockfile::default();

        lockfile.tools.insert(
            "cargo-nextest".to_owned(),
            LockedTool {
                version: "0.9.143".to_owned(),
                source: "cargo:cargo-nextest".to_owned(),
                tag: None,
                sha256: Some("a".repeat(64)),
                artifacts: BTreeMap::new(),
                ..LockedTool::default()
            },
        );

        let content = Toml::try_from(&lockfile).unwrap();

        assert!(content.0.contains("source = \"cargo:cargo-nextest\""));
        assert!(
            content
                .0
                .contains(&format!("sha256 = \"{}\"", "a".repeat(64)))
        );
        assert!(!content.0.contains("tag ="));
        assert!(!content.0.contains("artifacts ="));
    }

    #[test]
    fn rejects_inconsistent_cargo_lock_entry() {
        let mut lockfile = Lockfile::default();

        lockfile.tools.insert(
            "tool".to_owned(),
            LockedTool {
                version: "1.0.0".to_owned(),
                source: "cargo:tool".to_owned(),
                tag: None,
                sha256: None,
                artifacts: BTreeMap::new(),
                ..LockedTool::default()
            },
        );

        assert!(Toml::try_from(&lockfile).is_err());

        {
            let tool = lockfile.tools.get_mut("tool").unwrap();
            tool.sha256 = Some("a".repeat(64));
            tool.tag = Some("v1.0.0".to_owned());
        }

        assert!(Toml::try_from(&lockfile).is_err());

        lockfile.tools.get_mut("tool").unwrap().tag = None;

        assert!(Toml::try_from(&lockfile).is_ok());
    }

    const EXPLICIT_LOCK: &str = r#"lock-version = 1

[platforms]
targets = [
    "macos-aarch64",
    "linux-x86_64",
]

[tools.tool]
version = "1.0.0"
source = "github:owner/tool"
tag = "v1.0.0"
targets = ["linux-x86_64"]
asset-pattern = "tool-{os}-{arch}"

[tools.tool.artifacts.linux-x86_64]
asset = "tool-linux-x86_64"
url = "https://example.com/tool"
sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
format = "raw"
checksum-source = "digest"
"#;

    #[test]
    fn roundtrips_explicit_targets_deterministically() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("binloom.lock");
        // Input order does not matter; output is canonical.
        fs::write(
            &path,
            EXPLICIT_LOCK.replace(
                "[\n    \"macos-aarch64\",\n    \"linux-x86_64\",\n]",
                "[\"linux-x86_64\", \"macos-aarch64\"]",
            ),
        )
        .unwrap();

        let lockfile = Lockfile::try_from(path.as_path()).unwrap();
        assert_eq!(
            lockfile.platforms.as_ref().unwrap().targets,
            [Platform::MacosAarch64, Platform::LinuxX86_64]
        );
        assert_eq!(
            lockfile.tools["tool"].asset_pattern.as_deref(),
            Some("tool-{os}-{arch}")
        );

        lockfile.write(&path).unwrap();
        let first = fs::read_to_string(&path).unwrap();
        assert_eq!(first, EXPLICIT_LOCK);
        for _ in 0..2 {
            Lockfile::try_from(path.as_path())
                .unwrap()
                .write(&path)
                .unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), first);
        }
    }

    #[test]
    fn legacy_release_lock_roundtrips_byte_for_byte() {
        let legacy = EXPLICIT_LOCK
            .replace(
                "[platforms]\ntargets = [\n    \"macos-aarch64\",\n    \"linux-x86_64\",\n]\n\n",
                "",
            )
            .replace(
                "targets = [\"linux-x86_64\"]\nasset-pattern = \"tool-{os}-{arch}\"\n",
                "",
            );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("binloom.lock");
        fs::write(&path, &legacy).unwrap();
        let loaded = Lockfile::try_from(path.as_path()).unwrap();
        assert!(loaded.platforms.is_none());
        assert_eq!(Toml::try_from(&loaded).unwrap().0, legacy);
    }

    #[test]
    fn abi_lock_serializes_target_arrays_and_artifacts_in_canonical_order() {
        let (header, artifact) = EXPLICIT_LOCK.split_once("[tools.tool.artifacts.").unwrap();
        let header = header
            .replace(
                "[\n    \"macos-aarch64\",\n    \"linux-x86_64\",\n]",
                "[\"linux-x86_64-musl\", \"linux-x86_64-gnu\", \"macos-aarch64\"]",
            )
            .replace(
                "[\"linux-x86_64\"]",
                "[\"linux-x86_64-musl\", \"macos-aarch64\", \"linux-x86_64-gnu\"]",
            );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("binloom.lock");
        let mut input = header;
        for target in ["linux-x86_64-musl", "linux-x86_64-gnu", "macos-aarch64"] {
            input.push_str(&format!(
                "[tools.tool.artifacts.{}\n",
                artifact.replace("linux-x86_64", target)
            ));
        }
        fs::write(&path, &input).unwrap();
        let loaded = Lockfile::try_from(path.as_path()).unwrap();
        let canonical = Toml::try_from(&loaded).unwrap().0;
        let targets =
            "[\n    \"macos-aarch64\",\n    \"linux-x86_64-gnu\",\n    \"linux-x86_64-musl\",\n]";
        assert_eq!(canonical.matches(targets).count(), 2);
        let tables = ["macos-aarch64", "linux-x86_64-gnu", "linux-x86_64-musl"].map(|target| {
            canonical
                .find(&format!("[tools.tool.artifacts.{target}]"))
                .unwrap()
        });
        assert!(tables.is_sorted());
        fs::write(&path, &canonical).unwrap();
        let loaded = Lockfile::try_from(path.as_path()).unwrap();
        assert_eq!(Toml::try_from(&loaded).unwrap().0, canonical);
    }

    #[test]
    fn rejects_inconsistent_lock_targets() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("binloom.lock");
        let cases = [
            (
                EXPLICIT_LOCK.replace(
                    "[platforms]\ntargets = [\n    \"macos-aarch64\",\n    \"linux-x86_64\",\n]\n\n",
                    "",
                ),
                "targets and asset-pattern require [platforms] targets",
            ),
            (
                EXPLICIT_LOCK.replace("targets = [\"linux-x86_64\"]\n", ""),
                "tool is missing targets required by [platforms]",
            ),
            (
                EXPLICIT_LOCK.replace(
                    "targets = [\"linux-x86_64\"]\n",
                    "targets = [\"linux-x86_64\", \"linux-aarch64\"]\n",
                ),
                "target `linux-aarch64` is not in [platforms] targets",
            ),
            (
                EXPLICIT_LOCK.replace(
                    "targets = [\"linux-x86_64\"]\n",
                    "targets = [\"macos-aarch64\"]\n",
                ),
                "release artifacts must match targets",
            ),
            (
                EXPLICIT_LOCK.replace(
                    "\"macos-aarch64\",\n    \"linux-x86_64\"",
                    "\"linux-x86_64\",\n    \"linux-x86_64\"",
                ),
                "duplicate target `linux-x86_64`",
            ),
            (
                EXPLICIT_LOCK.replace("[\"linux-x86_64\"]", "[\"linux-x86_64-gnu\"]"),
                "target `linux-x86_64-gnu` is not in [platforms] targets",
            ),
            (
                EXPLICIT_LOCK.replace(
                    "\"macos-aarch64\",\n    \"linux-x86_64\"",
                    "\"linux-x86_64\",\n    \"linux-x86_64-musl\"",
                ),
                "cannot mix legacy target `linux-x86_64` with `linux-x86_64-musl`",
            ),
            (
                EXPLICIT_LOCK.replace("linux-x86_64", "linux-x86_64-musl").replace(
                    "[tools.tool.artifacts.linux-x86_64-musl]",
                    "[tools.tool.artifacts.linux-x86_64]",
                ),
                "release artifacts must match targets",
            ),
        ];

        for (content, expected) in cases {
            fs::write(&path, &content).unwrap();
            let error = Lockfile::try_from(path.as_path()).unwrap_err();
            assert!(
                format!("{error:#}").contains(expected),
                "{content}: {error:#}"
            );
        }
    }
}
