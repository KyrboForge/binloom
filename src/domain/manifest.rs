use crate::{
    common::{validate_tool_name, validate_version},
    domain::{
        lockfile::Lockfile,
        platform::{Platform, canonical_targets},
        sources::{Source, bare_version},
    },
};
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use std::{collections::BTreeMap, fs, path::Path};
use toml_edit::{DocumentMut, Item, Value};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    #[serde(rename = "manifest-version")]
    pub(crate) version: u32,

    pub(crate) binloom: Binloom,

    /// Explicit project targets; `None` keeps the legacy four-target matrix.
    pub(crate) platforms: Option<Platforms>,

    #[serde(default)]
    pub(crate) tools: BTreeMap<String, Tool>,

    #[serde(default)]
    pub(crate) update: UpdateConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Binloom {
    pub(crate) version: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Platforms {
    pub(crate) targets: Vec<Platform>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Tool {
    pub(crate) version: String,
    pub(crate) source: Source,
    pub(crate) asset: Option<String>,
    pub(crate) targets: Option<Vec<Platform>>,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct UpdateConfig {
    pub(crate) minimum_release_age_minutes: u64,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            minimum_release_age_minutes: 24 * 60,
        }
    }
}

impl TryFrom<&Path> for Manifest {
    type Error = anyhow::Error;

    fn try_from(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;

        Self::parse(&content, path)
    }
}

impl Manifest {
    pub(crate) fn parse(content: &str, path: &Path) -> Result<Self> {
        let mut manifest: Self = toml::from_str(content)
            .with_context(|| format!("failed to parse {}", path.display()))?;

        if manifest.version != 1 {
            bail!("unsupported manifest version: {}", manifest.version);
        }
        validate_version(&manifest.binloom.version).context("invalid Binloom version")?;

        if let Some(platforms) = &mut manifest.platforms {
            platforms.targets =
                canonical_targets(&platforms.targets).context("invalid [platforms] targets")?;
        }
        let project = manifest.project_targets().map(<[Platform]>::to_vec);
        for (name, tool) in &mut manifest.tools {
            let Some(targets) = &mut tool.targets else {
                continue;
            };
            let project = project.as_deref().with_context(|| {
                format!("tool {name} declares targets, but binloom.toml has no [platforms] targets")
            })?;
            *targets = canonical_targets(targets)
                .with_context(|| format!("invalid targets for tool {name}"))?;
            if let Some(target) = targets.iter().find(|target| !project.contains(target)) {
                bail!("tool {name} target `{target}` is not in [platforms] targets");
            }
        }

        for (name, tool) in &manifest.tools {
            validate_tool_name(name)?;
            validate_version(&tool.version)
                .with_context(|| format!("invalid version for tool {name}"))?;
            let bare = bare_version(name, &tool.version);
            ensure!(
                matches!(tool.source, Source::Cargo(_)) || bare == tool.version,
                "version {} for tool {name} must be written as {bare} to match binloom.lock",
                tool.version
            );
            if matches!(tool.source, Source::Cargo(_)) {
                ensure!(
                    tool.asset.is_none(),
                    "Cargo tool {name} does not support release asset patterns"
                );
            }
        }
        Ok(manifest)
    }

    /// Declared project targets in canonical order, or `None` for legacy projects.
    pub(crate) fn project_targets(&self) -> Option<&[Platform]> {
        self.platforms
            .as_ref()
            .map(|platforms| platforms.targets.as_slice())
    }

    /// Targets a tool must resolve for: its own, the project's, or every
    /// legacy platform when the project declares none.
    pub(crate) fn tool_targets<'a>(&'a self, tool: &'a Tool) -> &'a [Platform] {
        tool.targets
            .as_deref()
            .or(self.project_targets())
            .unwrap_or(&Platform::ALL)
    }

    pub(crate) fn ensure_matches_lockfile(&self, lockfile: &Lockfile) -> Result<()> {
        let explicit = self.platforms.is_some();
        let matches = self.project_targets()
            == lockfile
                .platforms
                .as_ref()
                .map(|platforms| platforms.targets.as_slice())
            && self.tools.len() == lockfile.tools.len()
            && self.tools.iter().all(|(name, tool)| {
                lockfile.tools.get(name).is_some_and(|locked| {
                    locked.version == tool.version
                        && locked.source == tool.source.to_string()
                        && locked.targets.as_deref() == explicit.then(|| self.tool_targets(tool))
                        && (!explicit || locked.asset_pattern == tool.asset)
                })
            });

        ensure!(
            matches,
            "binloom.toml and binloom.lock are out of sync; run `binloom update`"
        );

        Ok(())
    }
}

/// Returns the manifest with updated versions without writing it, so callers
/// can surface edit errors before changing any file.
pub(crate) fn render_versions<'a>(
    path: &Path,
    binloom: Option<&str>,
    tools: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<String> {
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

    Ok(document.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::lockfile::LockedTool;
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
    fn rejects_cargo_release_asset_patterns() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("binloom.toml");
        let content = r#"manifest-version = 1

[binloom]
version = "0.1.0"

[tools.example]
version = "1.0.0"
source = "cargo:example"
"#;

        fs::write(&path, content).unwrap();
        assert!(Manifest::try_from(path.as_path()).is_ok());

        fs::write(&path, format!("{content}asset = \"example.gz\"\n")).unwrap();
        let error = Manifest::try_from(path.as_path()).unwrap_err();

        assert_eq!(
            error.to_string(),
            "Cargo tool example does not support release asset patterns"
        );
    }

    #[test]
    fn rejects_release_tag_prefixed_versions_but_not_nonnumeric_ones() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("binloom.toml");
        let manifest = |version: &str| {
            format!(
                "manifest-version = 1\n\n[binloom]\nversion = \"0.1.0\"\n\n[tools.tool]\nversion = \"{version}\"\nsource = \"github:owner/tool\"\n"
            )
        };

        for version in ["nightly", "vnightly", "1.2.3"] {
            fs::write(&path, manifest(version)).unwrap();
            assert!(Manifest::try_from(path.as_path()).is_ok(), "{version}");
        }
        for (version, bare) in [("v1.2.3", "1.2.3"), ("tool-nightly", "nightly")] {
            fs::write(&path, manifest(version)).unwrap();
            let error = Manifest::try_from(path.as_path()).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(&format!("must be written as {bare}")),
                "{error}"
            );
        }
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

        let content = render_versions(&path, Some("0.1.1"), [("lefthook", "2.1.11")]).unwrap();

        assert!(content.contains("# project tools"));
        assert!(content.contains("version = \"0.1.1\" # wrapper"));
        assert!(content.contains("version = \"2.1.11\" # hooks"));
    }

    #[test]
    fn rejects_unsafe_names_and_versions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("binloom.toml");

        let invalid_manifests = [
            r#"manifest-version = 1

[binloom]
version = "0.2.2"

[tools."../../x"]
version = "1.0.0"
source = "github:owner/repo"
"#,
            r#"manifest-version = 1

[binloom]
version = "0.2.2"

[tools.example]
version = "/tmp/x"
source = "github:owner/repo"
"#,
            r#"manifest-version = 1

[binloom]
version = "../../x"
"#,
        ];

        for content in invalid_manifests {
            fs::write(&path, content).unwrap();
            assert!(Manifest::try_from(path.as_path()).is_err(), "{content}");
        }
    }

    #[test]
    fn rejects_manifest_and_lockfile_drift() {
        let manifest = Manifest {
            version: 1,
            binloom: Binloom {
                version: "0.2.2".to_owned(),
            },
            tools: BTreeMap::from([(
                "tool".to_owned(),
                Tool {
                    version: "1.0.0".to_owned(),
                    source: Source::try_from("github:owner/tool".to_owned()).unwrap(),
                    asset: None,
                    targets: None,
                },
            )]),
            update: UpdateConfig::default(),
            platforms: None,
        };

        let mut lockfile = Lockfile::default();
        lockfile.tools.insert(
            "tool".to_owned(),
            LockedTool {
                version: "1.0.0".to_owned(),
                source: "github:owner/tool".to_owned(),
                tag: Some("v1.0.0".to_owned()),
                sha256: None,
                artifacts: BTreeMap::new(),
                ..LockedTool::default()
            },
        );

        assert!(manifest.ensure_matches_lockfile(&lockfile).is_ok());

        lockfile.tools.get_mut("tool").unwrap().version = "2.0.0".to_owned();

        let error = manifest.ensure_matches_lockfile(&lockfile).unwrap_err();
        assert_eq!(
            error.to_string(),
            "binloom.toml and binloom.lock are out of sync; run `binloom update`"
        );

        lockfile.tools.get_mut("tool").unwrap().version = "1.0.0".to_owned();
        lockfile.tools.insert(
            "extra".to_owned(),
            LockedTool {
                version: "1.0.0".to_owned(),
                source: "github:owner/extra".to_owned(),
                tag: Some("v1.0.0".to_owned()),
                sha256: None,
                artifacts: BTreeMap::new(),
                ..LockedTool::default()
            },
        );

        assert!(manifest.ensure_matches_lockfile(&lockfile).is_err());
    }

    fn parse(content: &str) -> Result<Manifest> {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("binloom.toml");
        fs::write(&path, content).unwrap();
        Manifest::try_from(path.as_path())
    }

    const HEADER: &str = "manifest-version = 1\n\n[binloom]\nversion = \"0.1.0\"\n";

    #[test]
    fn resolves_inherited_and_narrowed_targets_in_canonical_order() {
        let manifest = parse(&format!(
            r#"{HEADER}
[platforms]
targets = ["linux-x86_64", "macos-aarch64", "linux-aarch64"]

[tools.wide]
version = "1.0.0"
source = "github:owner/wide"

[tools.narrow]
version = "1.0.0"
source = "github:owner/narrow"
targets = ["linux-x86_64", "linux-aarch64"]
"#
        ))
        .unwrap();

        let project = [
            Platform::MacosAarch64,
            Platform::LinuxAarch64,
            Platform::LinuxX86_64,
        ];
        assert_eq!(manifest.project_targets(), Some(&project[..]));
        assert_eq!(manifest.tool_targets(&manifest.tools["wide"]), project);
        assert_eq!(
            manifest.tool_targets(&manifest.tools["narrow"]),
            [Platform::LinuxAarch64, Platform::LinuxX86_64]
        );
    }

    #[test]
    fn legacy_manifest_targets_every_platform() {
        let manifest = parse(&format!(
            "{HEADER}\n[tools.tool]\nversion = \"1.0.0\"\nsource = \"github:owner/tool\"\n"
        ))
        .unwrap();

        assert_eq!(manifest.project_targets(), None);
        assert_eq!(
            manifest.tool_targets(&manifest.tools["tool"]),
            Platform::ALL
        );
    }

    #[test]
    fn rejects_invalid_target_declarations() {
        let tool = "[tools.tool]\nversion = \"1.0.0\"\nsource = \"github:owner/tool\"\n";
        let cases = [
            ("[platforms]\ntargets = []\n", "targets must not be empty"),
            (
                "[platforms]\ntargets = [\"windows-x86_64\"]\n",
                "unknown target `windows-x86_64`",
            ),
            (
                "[platforms]\ntargets = [\"linux-x86_64-musl\"]\n",
                "unknown target `linux-x86_64-musl`",
            ),
            (
                "[platforms]\ntargets = [\"linux-x86_64\", \"linux-x86_64\"]\n",
                "duplicate target `linux-x86_64`",
            ),
            (
                "[platforms]\ntargets = [\"linux-x86_64\"]\nos = \"linux\"\n",
                "unknown field `os`",
            ),
            (
                &format!("[platforms]\ntargets = [\"linux-x86_64\"]\n\n{tool}targets = []\n"),
                "targets must not be empty",
            ),
            (
                &format!(
                    "[platforms]\ntargets = [\"linux-x86_64\"]\n\n{tool}targets = [\"macos-aarch64\"]\n"
                ),
                "tool tool target `macos-aarch64` is not in [platforms] targets",
            ),
            (
                &format!("{tool}targets = [\"linux-x86_64\"]\n"),
                "tool tool declares targets, but binloom.toml has no [platforms] targets",
            ),
        ];

        for (body, expected) in cases {
            let error = parse(&format!("{HEADER}\n{body}")).unwrap_err();
            assert!(format!("{error:#}").contains(expected), "{body}: {error:#}");
        }
    }

    #[test]
    fn detects_target_and_asset_pattern_drift() {
        let manifest = parse(&format!(
            r#"{HEADER}
[platforms]
targets = ["linux-aarch64", "linux-x86_64"]

[tools.tool]
version = "1.0.0"
source = "github:owner/tool"
asset = "tool-{{os}}-{{arch}}"
"#
        ))
        .unwrap();
        let targets = vec![Platform::LinuxAarch64, Platform::LinuxX86_64];
        let lockfile = || {
            let mut lockfile = Lockfile {
                platforms: Some(crate::domain::lockfile::LockedPlatforms {
                    targets: targets.clone(),
                }),
                ..Lockfile::default()
            };
            lockfile.tools.insert(
                "tool".to_owned(),
                LockedTool {
                    version: "1.0.0".to_owned(),
                    source: "github:owner/tool".to_owned(),
                    targets: Some(targets.clone()),
                    asset_pattern: Some("tool-{os}-{arch}".to_owned()),
                    ..LockedTool::default()
                },
            );
            lockfile
        };
        assert!(manifest.ensure_matches_lockfile(&lockfile()).is_ok());

        let drifted = |edit: &dyn Fn(&mut Lockfile)| {
            let mut lockfile = lockfile();
            edit(&mut lockfile);
            manifest.ensure_matches_lockfile(&lockfile).is_err()
        };
        assert!(drifted(&|lock| lock.platforms = None));
        assert!(drifted(&|lock| {
            lock.platforms.as_mut().unwrap().targets = vec![Platform::LinuxX86_64];
        }));
        assert!(drifted(&|lock| {
            lock.tools.get_mut("tool").unwrap().targets = Some(vec![Platform::LinuxX86_64]);
        }));
        assert!(drifted(&|lock| {
            lock.tools.get_mut("tool").unwrap().asset_pattern = None;
        }));
        assert!(drifted(&|lock| {
            lock.tools.get_mut("tool").unwrap().asset_pattern = Some("other".to_owned());
        }));
    }
}
