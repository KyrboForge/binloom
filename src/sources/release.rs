use crate::download;
use crate::platform::Platform;
use anyhow::{Context, bail};
use reqwest::blocking::Client;

#[derive(Debug)]
pub struct Release {
    pub tag: String,
    pub published_at: Option<String>,
    pub assets: Vec<ReleaseAsset>,
}

#[derive(Debug)]
pub struct ReleaseAsset {
    pub name: String,
    pub download_url: String,
    pub sha256: Option<String>,
}

impl Release {
    pub fn find_asset(&self, tool_name: &str, platform: Platform) -> anyhow::Result<&ReleaseAsset> {
        let tool_name = tool_name.to_lowercase();

        let matches: Vec<_> = self
            .assets
            .iter()
            .filter(|asset| {
                let name = asset.name.to_lowercase();
                let is_metadata = [".sha256", ".sha256sum", ".sig", ".minisig"]
                    .iter()
                    .any(|suffix| name.ends_with(suffix));

                !is_metadata
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
            .filter(|asset| asset.name.to_ascii_lowercase().ends_with(".gz"))
            .collect::<Vec<_>>();

        let matches = if gzip_matches.len() == 1 {
            gzip_matches
        } else {
            matches
        };
        let matches = Self::prefer(matches, |asset| {
            asset
                .name
                .to_ascii_lowercase()
                .contains(platform.os_aliases()[0])
        });

        let matches = Self::prefer(matches, |asset| {
            asset
                .name
                .to_ascii_lowercase()
                .contains(platform.arch_aliases()[0])
        });

        let matches = Self::prefer(matches, |asset| {
            asset.name.to_ascii_lowercase().ends_with(".gz")
        });

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

    pub fn checksum_from_release(
        &self,
        client: &Client,
        asset: &ReleaseAsset,
    ) -> anyhow::Result<Option<String>> {
        if let Some(checksum) = &asset.sha256 {
            return Ok(Some(checksum.clone()));
        }

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

                let content = download::text_url(client, &checksum_asset.download_url)?;

                if let Some(checksum) = Self::checksum_from_text(&content, &asset.name, is_exact) {
                    return Ok(Some(checksum));
                }
            }
        }

        Ok(None)
    }

    pub fn find_asset_by_name(&self, asset_name: &str) -> anyhow::Result<&ReleaseAsset> {
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
    fn prefer(
        assets: Vec<&ReleaseAsset>,
        predicate: impl Fn(&ReleaseAsset) -> bool,
    ) -> Vec<&ReleaseAsset> {
        let preferred = assets
            .iter()
            .copied()
            .filter(|asset| predicate(asset))
            .collect::<Vec<_>>();

        if preferred.is_empty() {
            assets
        } else {
            preferred
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

        for (platform, expected_name) in expected {
            let matched = release.find_asset("lefthook", platform).unwrap();

            assert_eq!(matched.name, expected_name);
        }
    }

    #[test]
    fn rejects_ambiguous_assets() {
        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: Some("2026-01-01T00:00:00Z".to_owned()),
            assets: vec![
                asset("tool_1.0.0_linux_x86_64.gz"),
                asset("tool-pro_1.0.0_linux_x86_64.gz"),
            ],
        };

        let error = release
            .find_asset("tool", Platform::LinuxX86_64)
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
    fn prefers_checksum_embedded_in_asset() {
        let checksum = "a".repeat(64);
        let mut release_asset = asset("tool_linux_x86_64.gz");
        release_asset.sha256 = Some(checksum.clone());
        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: None,
            assets: vec![release_asset],
        };
        let client = Client::builder().build().unwrap();

        assert_eq!(
            release
                .checksum_from_release(&client, &release.assets[0])
                .unwrap(),
            Some(checksum)
        );
    }

    #[test]
    fn returns_none_when_release_has_no_checksum() {
        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: None,
            assets: vec![asset("tool_linux_x86_64.gz")],
        };
        let client = Client::builder().build().unwrap();

        assert_eq!(
            release
                .checksum_from_release(&client, &release.assets[0])
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

        let matched = release.find_asset("tool", Platform::LinuxX86_64).unwrap();

        assert_eq!(matched.name, "tool_1.0.0_linux_x86_64.gz");
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

        let matched = release.find_asset("tool", Platform::MacosAarch64).unwrap();

        assert_eq!(matched.name, "tool_1.0.0_MacOS_arm64.gz");
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
}
