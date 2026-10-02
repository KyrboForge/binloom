use crate::common::warn;
use crate::domain::platform::Platform;
use crate::download;
use crate::download::Client;
use anyhow::{Context, bail};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug)]
pub(crate) struct Release {
    pub(crate) tag: String,
    pub(crate) published_at: Option<String>,
    pub(crate) assets: Vec<ReleaseAsset>,
}

#[derive(Debug)]
pub(crate) struct ReleaseAsset {
    pub(crate) name: String,
    pub(crate) download_url: String,
    pub(crate) sha256: Option<String>,
}

impl Release {
    pub(crate) fn find_asset_by_pattern(
        &self,
        pattern: &str,
        version: &str,
        platform: Platform,
    ) -> anyhow::Result<&ReleaseAsset> {
        let candidates = platform
            .os_aliases()
            .iter()
            .flat_map(|os| {
                platform.arch_aliases().iter().map(move |arch| {
                    pattern
                        .replace("{version}", version)
                        .replace("{os}", os)
                        .replace("{arch}", arch)
                })
            })
            .collect::<Vec<_>>();

        let matches = self
            .assets
            .iter()
            .filter(|asset| {
                candidates
                    .iter()
                    .any(|candidate| asset.name.eq_ignore_ascii_case(candidate))
            })
            .collect::<Vec<_>>();

        match matches.as_slice() {
            [asset] => Ok(asset),
            [] => bail!(
                "release {} has no asset matching pattern {pattern} for {platform}",
                self.tag
            ),
            assets => {
                let names = assets
                    .iter()
                    .map(|asset| asset.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");

                bail!("multiple release assets matched pattern {pattern} for {platform}: {names}")
            }
        }
    }
    pub(crate) fn find_asset(
        &self,
        tool_name: &str,
        platform: Platform,
        emitted_warnings: &mut BTreeSet<String>,
    ) -> anyhow::Result<&ReleaseAsset> {
        let tool_name = tool_name.to_lowercase();

        let matches: Vec<_> = self
            .assets
            .iter()
            .filter(|asset| {
                let name = asset.name.to_lowercase();
                let is_metadata = [".sha256", ".sha256sum", ".sig", ".minisig"]
                    .iter()
                    .any(|suffix| name.ends_with(suffix));
                let is_package = [".deb", ".rpm", ".apk", ".pkg", ".dmg", ".msi"]
                    .iter()
                    .any(|suffix| name.ends_with(suffix));

                !is_metadata
                    && !is_package
                    && name.contains(&tool_name)
                    && platform
                        .os_aliases()
                        .iter()
                        .any(|alias| name.contains(alias))
                    && platform
                        .arch_aliases()
                        .iter()
                        .any(|alias| name.contains(alias))
            })
            .collect();
        let gzip_matches = matches
            .iter()
            .copied()
            .filter(|asset| {
                let name = asset.name.to_ascii_lowercase();
                name.ends_with(".gz") || name.ends_with(".tgz")
            })
            .collect::<Vec<_>>();

        let matches = if gzip_matches.len() == 1 {
            Self::warn_dropped(
                &matches,
                &gzip_matches,
                "the only gzip candidate",
                emitted_warnings,
            );
            gzip_matches
        } else {
            matches
        };

        let matches = Self::prefer(
            matches,
            &format!("OS alias {}", platform.os_aliases()[0]),
            |asset| {
                asset
                    .name
                    .to_ascii_lowercase()
                    .contains(platform.os_aliases()[0])
            },
            emitted_warnings,
        );

        let matches = Self::prefer(
            matches,
            &format!("architecture alias {}", platform.arch_aliases()[0]),
            |asset| {
                asset
                    .name
                    .to_ascii_lowercase()
                    .contains(platform.arch_aliases()[0])
            },
            emitted_warnings,
        );

        let matches = Self::prefer(
            matches,
            "plain asset name",
            |asset| self.is_plain_name(&asset.name, &tool_name, platform),
            emitted_warnings,
        );

        let matches = Self::prefer(
            matches,
            "gzip format",
            |asset| {
                let name = asset.name.to_ascii_lowercase();
                name.ends_with(".gz") || name.ends_with(".tgz")
            },
            emitted_warnings,
        );

        match matches.as_slice() {
            [asset] => Ok(asset),
            [] => bail!("no release asset matched {tool_name} for {platform}"),
            assets => {
                let names = assets
                    .iter()
                    .map(|asset| asset.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");

                bail!("multiple release assets matched {tool_name} for {platform}: {names}")
            }
        }
    }

    pub(crate) fn checksum_from_sidecar(
        &self,
        client: &Client,
        asset: &ReleaseAsset,
        cache: &mut BTreeMap<String, String>,
    ) -> anyhow::Result<Option<String>> {
        let exact_names = [
            format!("{}.sha256", asset.name),
            format!("{}.sha256sum", asset.name),
        ];

        for exact in [true, false] {
            for checksum_asset in &self.assets {
                let is_exact = exact_names
                    .iter()
                    .any(|name| checksum_asset.name.eq_ignore_ascii_case(name));

                let is_global = matches!(
                    checksum_asset.name.to_ascii_lowercase().as_str(),
                    "sha256sums" | "sha256sums.txt" | "checksums.txt" | "checksums.sha256"
                );

                if (exact && !is_exact) || (!exact && !is_global) {
                    continue;
                }

                let url = &checksum_asset.download_url;

                if !cache.contains_key(url) {
                    let content = download::text_url(client, url)?;
                    cache.insert(url.clone(), content);
                }

                let content = cache.get(url).expect("checksum sidecar was just cached");

                if let Some(checksum) = Self::checksum_from_text(content, &asset.name, is_exact) {
                    return Ok(Some(checksum));
                }
            }
        }

        Ok(None)
    }

    pub(crate) fn find_asset_by_name(&self, asset_name: &str) -> anyhow::Result<&ReleaseAsset> {
        self.assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .with_context(move || format!("release {} has no asset named {asset_name}", self.tag))
    }

    fn normalize_sha256(value: &str) -> Option<String> {
        let value = value.trim();

        if value.len() == 64 && value.chars().all(|character| character.is_ascii_hexdigit()) {
            Some(value.to_ascii_lowercase())
        } else {
            None
        }
    }

    fn checksum_from_text(
        content: &str,
        asset_name: &str,
        allow_bare_checksum: bool,
    ) -> Option<String> {
        let bsd_prefix = format!("SHA256 ({asset_name}) = ");

        for line in content.lines().map(str::trim) {
            if allow_bare_checksum && let Some(checksum) = Self::normalize_sha256(line) {
                return Some(checksum);
            }

            if let Some(checksum) = line
                .strip_prefix(&bsd_prefix)
                .and_then(Self::normalize_sha256)
            {
                return Some(checksum);
            }

            let mut fields = line.split_whitespace();
            let Some(checksum) = fields.next().and_then(Self::normalize_sha256) else {
                continue;
            };
            let Some(filename) = fields.next() else {
                continue;
            };

            if filename.trim_start_matches('*') == asset_name {
                return Some(checksum);
            }
        }

        None
    }
    fn prefer<'a>(
        assets: Vec<&'a ReleaseAsset>,
        reason: &str,
        predicate: impl Fn(&ReleaseAsset) -> bool,
        emitted_warnings: &mut BTreeSet<String>,
    ) -> Vec<&'a ReleaseAsset> {
        let preferred = assets
            .iter()
            .copied()
            .filter(|asset| predicate(asset))
            .collect::<Vec<_>>();

        Self::warn_dropped(&assets, &preferred, reason, emitted_warnings);

        if preferred.is_empty() {
            assets
        } else {
            preferred
        }
    }

    /// A plain name holds only the tool name, platform aliases, version
    /// components and archive extensions, unlike variants such as `tool-fips`.
    fn is_plain_name(&self, name: &str, tool_name: &str, platform: Platform) -> bool {
        let tag = self.tag.to_ascii_lowercase();
        let mut name = name
            .to_ascii_lowercase()
            .replace(&tag, " ")
            .replace(tag.strip_prefix('v').unwrap_or(&tag), " ")
            .replace(tool_name, " ");

        for alias in platform.arch_aliases().iter().chain(platform.os_aliases()) {
            name = name.replace(alias, " ");
        }

        name.split(|character: char| !character.is_ascii_alphanumeric())
            .all(|token| {
                matches!(token, "" | "tar" | "gz" | "tgz")
                    || token
                        .strip_prefix('v')
                        .unwrap_or(token)
                        .chars()
                        .all(|character| character.is_ascii_digit())
            })
    }

    fn warn_dropped(
        assets: &[&ReleaseAsset],
        preferred: &[&ReleaseAsset],
        reason: &str,
        emitted_warnings: &mut BTreeSet<String>,
    ) {
        if preferred.is_empty() || preferred.len() == assets.len() {
            return;
        }

        let dropped = assets
            .iter()
            .filter(|asset| {
                !preferred
                    .iter()
                    .any(|candidate| candidate.name == asset.name)
            })
            .map(|asset| asset.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");

        let message = format!("asset selection preferred {reason}; dropped: {dropped}");

        if emitted_warnings.insert(message.clone()) {
            warn(&message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> ReleaseAsset {
        ReleaseAsset {
            name: name.to_owned(),
            download_url: format!("https://example.com/{name}"),
            sha256: None,
        }
    }

    #[test]
    fn matches_lefthook_assets_for_every_platform() {
        let release = Release {
            tag: "v2.1.10".to_owned(),
            published_at: Some("2026-01-01T00:00:00Z".to_owned()),
            assets: vec![
                asset("lefthook_2.1.10_MacOS_arm64.gz"),
                asset("lefthook_2.1.10_MacOS_x86_64.gz"),
                asset("lefthook_2.1.10_Linux_aarch64.gz"),
                asset("lefthook_2.1.10_Linux_x86_64.gz"),
            ],
        };

        let expected = [
            (Platform::MacosAarch64, "lefthook_2.1.10_MacOS_arm64.gz"),
            (Platform::MacosX86_64, "lefthook_2.1.10_MacOS_x86_64.gz"),
            (Platform::LinuxAarch64, "lefthook_2.1.10_Linux_aarch64.gz"),
            (Platform::LinuxX86_64, "lefthook_2.1.10_Linux_x86_64.gz"),
        ];
        let mut emitted_warnings = BTreeSet::new();
        for (platform, expected_name) in expected {
            let matched = release
                .find_asset("lefthook", platform, &mut emitted_warnings)
                .unwrap();

            assert_eq!(matched.name, expected_name);
        }
    }

    #[test]
    fn rejects_ambiguous_assets() {
        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: Some("2026-01-01T00:00:00Z".to_owned()),
            assets: vec![
                asset("tool-lite_1.0.0_linux_x86_64.gz"),
                asset("tool-pro_1.0.0_linux_x86_64.gz"),
            ],
        };

        let mut emitted_warnings = BTreeSet::new();
        let error = release
            .find_asset("tool", Platform::LinuxX86_64, &mut emitted_warnings)
            .unwrap_err();

        assert!(
            error
                .to_string()
                .starts_with("multiple release assets matched")
        );
    }
    #[test]
    fn reads_checksum_from_sha256sums() {
        let checksum = "a".repeat(64);
        let content = format!(
            "{}  other-tool.gz\n{}  tool_linux_x86_64.gz\n",
            "b".repeat(64),
            checksum
        );

        assert_eq!(
            Release::checksum_from_text(&content, "tool_linux_x86_64.gz", false),
            Some(checksum)
        );
    }

    #[test]
    fn reads_bare_sidecar_checksum() {
        let checksum = "A".repeat(64);

        assert_eq!(
            Release::checksum_from_text(&checksum, "tool.gz", true),
            Some("a".repeat(64))
        );
    }

    #[test]
    fn returns_none_when_release_has_no_checksum() {
        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: None,
            assets: vec![asset("tool_linux_x86_64.gz")],
        };
        let client = download::client();

        assert_eq!(
            release
                .checksum_from_sidecar(&client, &release.assets[0], &mut BTreeMap::new())
                .unwrap(),
            None
        );
    }

    #[test]
    fn ignores_checksum_assets_when_matching_binary() {
        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: Some("2026-01-01T00:00:00Z".to_owned()),
            assets: vec![
                asset("tool_1.0.0_linux_x86_64.gz"),
                asset("tool_1.0.0_linux_x86_64.gz.sha256"),
            ],
        };
        let mut emitted_warnings = BTreeSet::new();

        let matched = release
            .find_asset("tool", Platform::LinuxX86_64, &mut emitted_warnings)
            .unwrap();

        assert_eq!(matched.name, "tool_1.0.0_linux_x86_64.gz");
    }
    #[test]
    fn ignores_system_packages_when_matching_binary() {
        let release = Release {
            tag: "2026.9.3".to_owned(),
            published_at: Some("2026-01-01T00:00:00Z".to_owned()),
            assets: vec![
                asset("cloudflared-arm64.pkg"),
                asset("cloudflared-darwin-arm64.tgz"),
                asset("cloudflared-linux-aarch64.rpm"),
                asset("cloudflared-linux-arm64"),
                asset("cloudflared-linux-arm64.deb"),
            ],
        };
        let mut emitted_warnings = BTreeSet::new();

        for (platform, expected_name) in [
            (Platform::MacosAarch64, "cloudflared-darwin-arm64.tgz"),
            (Platform::LinuxAarch64, "cloudflared-linux-arm64"),
        ] {
            let matched = release
                .find_asset("cloudflared", platform, &mut emitted_warnings)
                .unwrap();

            assert_eq!(matched.name, expected_name);
        }
    }

    #[test]
    fn prefers_plain_name_over_variants() {
        let release = Release {
            tag: "2026.9.3".to_owned(),
            published_at: Some("2026-01-01T00:00:00Z".to_owned()),
            assets: vec![
                asset("cloudflared-fips-linux-amd64"),
                asset("cloudflared-linux-amd64"),
                asset("cloudflared-linux-amd64.deb"),
                asset("cloudflared-linux-x86_64.rpm"),
            ],
        };
        let mut emitted_warnings = BTreeSet::new();

        let matched = release
            .find_asset("cloudflared", Platform::LinuxX86_64, &mut emitted_warnings)
            .unwrap();

        assert_eq!(matched.name, "cloudflared-linux-amd64");
        assert!(emitted_warnings.contains(
            "asset selection preferred plain asset name; dropped: cloudflared-fips-linux-amd64"
        ));
        assert!(release.is_plain_name(
            "tool_v1.2.3_MacOS_arm64.tar.gz",
            "tool",
            Platform::MacosAarch64
        ));
    }

    #[test]
    fn prefers_gzip_over_raw_asset() {
        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: Some("2026-01-01T00:00:00Z".to_owned()),
            assets: vec![
                asset("tool_1.0.0_MacOS_arm64"),
                asset("tool_1.0.0_MacOS_arm64.gz"),
            ],
        };
        let mut emitted_warnings = BTreeSet::new();

        let matched = release
            .find_asset("tool", Platform::MacosAarch64, &mut emitted_warnings)
            .unwrap();

        assert_eq!(matched.name, "tool_1.0.0_MacOS_arm64.gz");
    }

    #[test]
    fn prefers_tgz_over_raw_asset_in_both_preference_passes() {
        for other_gzip in [false, true] {
            let mut release = Release {
                tag: "v1.0.0".to_owned(),
                published_at: None,
                assets: vec![asset("tool-macos-arm64"), asset("tool-macos-arm64.tgz")],
            };
            if other_gzip {
                release.assets.push(asset("tool-darwin-arm64.gz"));
            }

            let matched = release
                .find_asset("tool", Platform::MacosAarch64, &mut BTreeSet::new())
                .unwrap();

            assert_eq!(matched.name, "tool-macos-arm64.tgz");
        }
    }

    #[test]
    fn keeps_equivalent_tgz_and_tar_gz_assets_ambiguous() {
        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: None,
            assets: vec![
                asset("tool-linux-amd64.tgz"),
                asset("tool-linux-amd64.tar.gz"),
            ],
        };

        let error = release
            .find_asset("tool", Platform::LinuxX86_64, &mut BTreeSet::new())
            .unwrap_err();

        assert!(
            error
                .to_string()
                .starts_with("multiple release assets matched")
        );
    }

    #[test]
    fn prefers_plain_prerelease_name_over_variants() {
        for version in ["1.0.0-beta", "1.0.0-beta+build", "1.0.0-beta+fips"] {
            for prefix in ["", "v"] {
                let plain = format!("tool-{prefix}{version}-linux-amd64");
                let release = Release {
                    tag: format!("v{version}"),
                    published_at: None,
                    assets: vec![
                        asset(&format!("tool-fips-{prefix}{version}-linux-amd64")),
                        asset(&plain),
                    ],
                };

                let matched = release
                    .find_asset("tool", Platform::LinuxX86_64, &mut BTreeSet::new())
                    .unwrap();

                assert_eq!(matched.name, plain);
            }
        }
    }

    #[test]
    fn finds_exact_asset_by_name() {
        let release = Release {
            tag: "v0.2.0".to_owned(),
            published_at: Some("2026-01-01T00:00:00Z".to_owned()),
            assets: vec![asset("binloom_macos_aarch64.gz"), asset("binloomw")],
        };

        let wrapper = release.find_asset_by_name("binloomw").unwrap();
        assert_eq!(wrapper.name, "binloomw");

        let error = release.find_asset_by_name("missing").unwrap_err();
        assert_eq!(
            error.to_string(),
            "release v0.2.0 has no asset named missing"
        );
    }

    #[test]
    fn expands_asset_pattern_for_every_platform() {
        let release = Release {
            tag: "v1.2.3".to_owned(),
            published_at: None,
            assets: vec![
                asset("tool_1.2.3_MacOS_arm64.gz"),
                asset("tool_1.2.3_MacOS_x86_64.gz"),
                asset("tool_1.2.3_Linux_aarch64.gz"),
                asset("tool_1.2.3_Linux_x86_64.gz"),
            ],
        };

        let expected = [
            (Platform::MacosAarch64, "tool_1.2.3_MacOS_arm64.gz"),
            (Platform::MacosX86_64, "tool_1.2.3_MacOS_x86_64.gz"),
            (Platform::LinuxAarch64, "tool_1.2.3_Linux_aarch64.gz"),
            (Platform::LinuxX86_64, "tool_1.2.3_Linux_x86_64.gz"),
        ];

        for (platform, expected_name) in expected {
            let matched = release
                .find_asset_by_pattern("tool_{version}_{os}_{arch}.gz", "1.2.3", platform)
                .unwrap();

            assert_eq!(matched.name, expected_name);
        }
    }

    #[test]
    fn emits_identical_selection_warning_only_once() {
        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: None,
            assets: vec![
                asset("tool_macos_linux_aarch64_x86_64"),
                asset("tool_macos_linux_aarch64_x86_64.gz"),
            ],
        };

        let mut emitted_warnings = BTreeSet::new();

        for platform in Platform::ALL {
            release
                .find_asset("tool", platform, &mut emitted_warnings)
                .unwrap();
        }

        assert_eq!(emitted_warnings.len(), 1);
    }
}
