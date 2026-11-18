use crate::common::warn;
use crate::domain::lockfile::ArtifactFormat;
use crate::domain::platform::{LinuxAbi, Platform, contains_token};
use crate::domain::sources::bare_version;
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

/// Signatures, checksums, attestations and OS packages ship next to binaries
/// but never install as one; `ArtifactFormat` would otherwise read them as raw.
/// A trailing `.gz` is ignored so compressed sidecars never pass as gzip binaries.
fn is_sidecar_or_package(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let name = name.strip_suffix(".gz").unwrap_or(&name);
    [
        ".asc",
        ".sig",
        ".minisig",
        ".sigstore",
        ".bundle",
        ".pem",
        ".crt",
        ".cert",
        ".pub",
        ".md5",
        ".md5sum",
        ".md5sums",
        ".debug",
        ".b3",
        ".sbom",
        ".spdx",
        ".json",
        ".jsonl",
        ".txt",
        ".deb",
        ".rpm",
        ".apk",
        ".pkg",
        ".dmg",
        ".msi",
    ]
    .iter()
    .any(|suffix| name.ends_with(suffix))
        || is_sha_digest(name)
}

/// Matches every SHA checksum spelling (`.sha384`, `.sha3-256`, `.sha512sum`,
/// `.sha256sums`) so a digest size missing from a list never passes as a raw
/// binary.
fn is_sha_digest(name: &str) -> bool {
    name.rsplit_once('.')
        .and_then(|(_, extension)| extension.strip_prefix("sha"))
        .map(|rest| {
            rest.strip_suffix("sums")
                .or_else(|| rest.strip_suffix("sum"))
                .unwrap_or(rest)
        })
        .is_some_and(|rest| {
            !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit() || c == '-')
        })
}

/// Archives are every format that is neither a raw executable nor a
/// single-file gzip, so new archive formats rank below raw binaries unchanged.
fn is_archive(name: &str) -> bool {
    !matches!(
        ArtifactFormat::try_from(name),
        Ok(ArtifactFormat::Raw | ArtifactFormat::Gz)
    )
}

fn is_single_file_gzip(name: &str) -> bool {
    matches!(ArtifactFormat::try_from(name), Ok(ArtifactFormat::Gz))
}

/// Ignore the tool name and version when interpreting platform labels.
pub(crate) fn asset_platform_part(
    asset: &str,
    name: &str,
    version: &str,
    platform: Platform,
) -> String {
    // `v1.0.0` must go before `1.0.0`: the `v` would otherwise glue the bare
    // version to an alphanumeric and keep it visible.
    let tagged = if version.is_empty() {
        String::new()
    } else {
        format!("v{version}")
    };
    if platform.linux_abi().is_some() {
        // Remove one version spelling and one tool label. Further occurrences
        // can be real ABI/architecture tokens and must stay visible.
        let mut part = asset.to_ascii_lowercase();
        if !version.is_empty() {
            let without_tag = blank_token(&part, &tagged.to_ascii_lowercase(), true);
            part = if without_tag == part {
                blank_token(&part, &version.to_ascii_lowercase(), true)
            } else {
                without_tag
            };
        }
        return if name.is_empty() {
            part
        } else {
            blank_token(&part, &name.to_ascii_lowercase(), true)
        };
    }
    [tagged.as_str(), version, name]
        .iter()
        .filter(|part| !part.is_empty())
        .fold(asset.to_ascii_lowercase(), |rest, part| {
            blank_token(&rest, &part.to_ascii_lowercase(), false)
        })
}

pub(crate) fn foreign_platform_reason(platform: Platform, platform_part: &str) -> Option<String> {
    // OSes Binloom cannot target yet; naming one never matches any platform.
    const UNSUPPORTED_OS_ALIASES: [&str; 3] = ["windows", "win32", "win64"];
    // Likewise for architectures: `armv7` must never be locked as an x86_64 asset.
    const UNSUPPORTED_ARCH_ALIASES: [&str; 14] = [
        "arm", "armv5", "armv6", "armv7", "armv7l", "armhf", "i386", "i686", "386", "ppc64",
        "ppc64le", "s390x", "riscv64", "mips64",
    ];
    let foreign_os = Platform::LEGACY_DEFAULT
        .iter()
        .any(|other| other.os_aliases() != platform.os_aliases() && other.os_in(platform_part))
        || UNSUPPORTED_OS_ALIASES
            .iter()
            .any(|alias| contains_token(platform_part, alias));
    let foreign_arch = Platform::LEGACY_DEFAULT.iter().any(|other| {
        other.arch_aliases() != platform.arch_aliases() && other.arch_in(platform_part)
    }) || UNSUPPORTED_ARCH_ALIASES
        .iter()
        .any(|alias| contains_token(platform_part, alias));
    // Name a whole platform only when both its OS and architecture appear
    // in the name, so a Linux asset is never blamed on macOS.
    let foreign = Platform::LEGACY_DEFAULT.iter().find(|other| {
        **other != platform.base_platform()
            && other.os_in(platform_part)
            && other.arch_in(platform_part)
    });
    match (foreign, foreign_os, foreign_arch) {
        (Some(foreign), _, _) => Some(format!("is named for {foreign}")),
        (None, true, _) => Some("names a different OS".to_owned()),
        (None, false, true) => Some("names a different architecture".to_owned()),
        (None, false, false) => None,
    }
}

/// Replaces `token` with `-` wherever it is not glued to ASCII alphanumerics,
/// so a tool named `x` never blanks the `x` out of `x86_64`.
fn blank_token(name: &str, token: &str, first_only: bool) -> String {
    let bytes = name.as_bytes();
    let mut blanked = String::with_capacity(name.len());
    let mut copied = 0;
    for (start, _) in name.match_indices(token) {
        let end = start + token.len();
        let before = start.checked_sub(1).map(|index| bytes[index]);
        let after = bytes.get(end).copied();
        if !before.is_some_and(|byte| byte.is_ascii_alphanumeric())
            && !after.is_some_and(|byte| byte.is_ascii_alphanumeric())
        {
            blanked.push_str(&name[copied..start]);
            blanked.push('-');
            copied = end;
            if first_only {
                break;
            }
        }
    }
    blanked.push_str(&name[copied..]);
    blanked
}

pub(crate) fn linux_abi_rejection(
    platform: Platform,
    part: &str,
    require_abi: bool,
) -> Option<&'static str> {
    let abi = platform.linux_abi()?;
    let gnu = contains_token(part, "gnu") || contains_token(part, "glibc");
    let musl = contains_token(part, "musl");
    match (gnu, musl, abi) {
        (true, true, _) => Some("names both GNU/glibc and musl"),
        (true, false, LinuxAbi::Musl) => Some("names GNU/glibc, but this target requires musl"),
        (false, true, LinuxAbi::Gnu) => Some("names musl, but this target requires GNU/glibc"),
        (false, false, _) if require_abi => Some("has no GNU/glibc or musl token"),
        _ => None,
    }
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
                        .replace("{target}", platform.rust_target())
                })
            })
            .collect::<Vec<_>>();

        // Candidates follow alias declaration order, so the first alias with a
        // match wins instead of making `amd64` and `x64` ambiguous.
        let matches = candidates
            .iter()
            .map(|candidate| {
                self.assets
                    .iter()
                    .filter(|asset| asset.name.eq_ignore_ascii_case(candidate))
                    .collect::<Vec<_>>()
            })
            .find(|matches| !matches.is_empty())
            .unwrap_or_default();

        match matches.as_slice() {
            [asset] => Ok(asset),
            [] if platform.linux_abi().is_some() => {
                let mut names: Vec<_> = self
                    .assets
                    .iter()
                    .map(|asset| asset.name.as_str())
                    .collect();
                names.sort_unstable();
                let seen = if names.is_empty() {
                    "(none)".to_owned()
                } else {
                    names.join(", ")
                };
                bail!(
                    "release {} has no asset matching pattern {pattern} for {platform}; candidate assets that did not match: {seen}",
                    self.tag
                )
            }
            [] => bail!(
                "release {} has no asset matching pattern {pattern} for {platform}",
                self.tag
            ),
            assets => {
                let mut names = assets
                    .iter()
                    .map(|asset| asset.name.as_str())
                    .collect::<Vec<_>>();
                if platform.linux_abi().is_some() {
                    names.sort_unstable();
                }
                let names = names.join(", ");

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
        let version = bare_version(tool_name, &self.tag);
        let tool_name = tool_name.to_lowercase();

        let candidates = self.assets.iter().filter_map(|asset| {
            let name = asset.name.to_lowercase();
            (!is_sidecar_or_package(&name)).then_some((asset, name))
        });
        let matches = if platform.linux_abi().is_some() {
            let mut matches = Vec::new();
            let mut rejected = Vec::new();
            for (asset, name) in candidates {
                if !name.contains(&tool_name) {
                    rejected.push(format!(
                        "{} (name does not contain {tool_name})",
                        asset.name
                    ));
                    continue;
                }
                let part = asset_platform_part(&asset.name, &tool_name, version, platform);
                let reason = foreign_platform_reason(platform, &part)
                    .or_else(|| {
                        if !platform.os_in(&part) || !platform.arch_in(&part) {
                            Some("lacks a Linux OS or matching architecture token".to_owned())
                        } else {
                            linux_abi_rejection(platform, &part, true).map(str::to_owned)
                        }
                    })
                    .or_else(|| {
                        ArtifactFormat::try_from(asset.name.as_str())
                            .err()
                            .map(|err| err.to_string())
                    });
                if let Some(reason) = reason {
                    rejected.push(format!("{} ({reason})", asset.name));
                } else {
                    matches.push(asset);
                }
            }
            if matches.is_empty() {
                rejected.sort_unstable();
                let seen = if rejected.is_empty() {
                    "(none)".to_owned()
                } else {
                    rejected.join(", ")
                };
                bail!(
                    "no release asset matched {tool_name} for {platform}; candidates: {seen}. Choose a release with a compatible asset or set an explicit asset pattern"
                );
            }
            matches.sort_unstable_by(|left, right| left.name.cmp(&right.name));
            matches
        } else {
            candidates
                .filter_map(|(asset, name)| {
                    (name.contains(&tool_name) && platform.os_in(&name) && platform.arch_in(&name))
                        .then_some(asset)
                })
                .collect::<Vec<_>>()
        };

        // Formats the installer cannot unpack must not win the later
        // preferences over an installable `.tar.gz`.
        let matches = Self::prefer(
            matches,
            "supported asset format",
            |asset| ArtifactFormat::try_from(asset.name.as_str()).is_ok(),
            emitted_warnings,
        );
        // Variants are dropped before the gzip shortcut and alias preferences,
        // so a lone `-fips.gz` never beats a plain archive and aliases only
        // break ties among equally plain names.
        let matches = Self::prefer(
            matches,
            "plain asset name",
            |asset| self.is_plain_name(&asset.name, &tool_name, platform),
            emitted_warnings,
        );
        let gzip_matches = matches
            .iter()
            .copied()
            .filter(|asset| is_single_file_gzip(&asset.name))
            .collect::<Vec<_>>();

        let matches = if gzip_matches.len() == 1 {
            Self::warn_dropped(
                &matches,
                &gzip_matches,
                "the only single-file gzip candidate",
                emitted_warnings,
            );
            gzip_matches
        } else {
            matches
        };

        // A raw executable always installs; an archive may not contain a
        // file named after the tool (yq ships `yq_linux_amd64` plus man pages).
        // It runs before alias preferences so an `amd64` raw binary is not
        // dropped in favour of an `x86_64` archive.
        let matches = Self::prefer(
            matches,
            "directly installable asset over an archive",
            |asset| !is_archive(&asset.name),
            emitted_warnings,
        );

        let matches = Self::prefer(
            matches,
            &format!("OS alias {}", platform.os_aliases()[0]),
            |asset| contains_token(&asset.name, platform.os_aliases()[0]),
            emitted_warnings,
        );

        // Aliases are ranked in declaration order so `amd64` keeps winning over
        // the later `x64`/`x86-64` spellings when a release ships several.
        let matches = platform
            .arch_aliases()
            .iter()
            .fold(matches, |matches, alias| {
                Self::prefer(
                    matches,
                    &format!("architecture alias {alias}"),
                    |asset| contains_token(&asset.name, alias),
                    emitted_warnings,
                )
            });

        let matches = Self::prefer(
            matches,
            "single-file gzip format",
            |asset| is_single_file_gzip(&asset.name),
            emitted_warnings,
        );

        // Releases often ship one archive in several compressions; `.tar.gz`
        // is the long-standing choice, so the others never make it ambiguous.
        let matches = Self::prefer(
            matches,
            "tar.gz over other archive formats",
            |asset| {
                matches!(
                    ArtifactFormat::try_from(asset.name.as_str()),
                    Ok(ArtifactFormat::Raw | ArtifactFormat::Gz | ArtifactFormat::TarGz)
                )
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
        if platform.linux_abi().is_some() {
            for alias in ["gnu", "glibc", "musl", "unknown"] {
                name = blank_token(&name, alias, false);
            }
        }

        name.split(|character: char| !character.is_ascii_alphanumeric())
            .all(|token| {
                matches!(
                    token,
                    "" | "tar"
                        | "gz"
                        | "tgz"
                        | "xz"
                        | "txz"
                        | "zip"
                        | "zst"
                        | "zstd"
                        | "bz2"
                        | "tbz2"
                ) || token
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
    fn matches_gitleaks_x64_assets_for_every_platform() {
        let release = Release {
            tag: "v8.30.1".to_owned(),
            published_at: None,
            assets: vec![
                asset("gitleaks_8.30.1_checksums.txt"),
                asset("gitleaks_8.30.1_darwin_arm64.tar.gz"),
                asset("gitleaks_8.30.1_darwin_x64.tar.gz"),
                asset("gitleaks_8.30.1_linux_arm64.tar.gz"),
                asset("gitleaks_8.30.1_linux_armv6.tar.gz"),
                asset("gitleaks_8.30.1_linux_armv7.tar.gz"),
                asset("gitleaks_8.30.1_linux_x32.tar.gz"),
                asset("gitleaks_8.30.1_linux_x64.tar.gz"),
                asset("gitleaks_8.30.1_windows_arm64.zip"),
                asset("gitleaks_8.30.1_windows_x32.zip"),
                asset("gitleaks_8.30.1_windows_x64.zip"),
            ],
        };

        let expected = [
            (
                Platform::MacosAarch64,
                "gitleaks_8.30.1_darwin_arm64.tar.gz",
            ),
            (Platform::MacosX86_64, "gitleaks_8.30.1_darwin_x64.tar.gz"),
            (Platform::LinuxAarch64, "gitleaks_8.30.1_linux_arm64.tar.gz"),
            (Platform::LinuxX86_64, "gitleaks_8.30.1_linux_x64.tar.gz"),
        ];

        for (platform, expected_name) in expected {
            let auto = release
                .find_asset("gitleaks", platform, &mut BTreeSet::new())
                .unwrap();
            let pattern = release
                .find_asset_by_pattern("gitleaks_{version}_{os}_{arch}.tar.gz", "8.30.1", platform)
                .unwrap();

            assert_eq!(auto.name, expected_name);
            assert_eq!(pattern.name, expected_name);
        }
    }

    #[test]
    fn expands_every_architecture_alias_in_patterns() {
        for alias in ["x64", "amd64", "x86-64", "x86_64"] {
            let release = Release {
                tag: "v1.0.0".to_owned(),
                published_at: None,
                assets: vec![asset(&format!("tool-linux-{alias}"))],
            };

            assert!(
                release
                    .find_asset_by_pattern("tool-{os}-{arch}", "1.0.0", Platform::LinuxX86_64)
                    .is_ok(),
                "{alias}"
            );
            assert!(
                release
                    .find_asset_by_pattern("tool-{os}-{arch}", "1.0.0", Platform::LinuxAarch64)
                    .is_err(),
                "{alias}"
            );
        }
    }

    #[test]
    fn ranks_architecture_aliases_in_declaration_order() {
        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: None,
            assets: vec![
                asset("tool-linux-x64"),
                asset("tool-linux-amd64"),
                asset("tool-linux-x86-64"),
            ],
        };

        let by_pattern = release
            .find_asset_by_pattern("tool-{os}-{arch}", "1.0.0", Platform::LinuxX86_64)
            .unwrap();
        let automatic = release
            .find_asset("tool", Platform::LinuxX86_64, &mut BTreeSet::new())
            .unwrap();

        assert_eq!(by_pattern.name, "tool-linux-amd64");
        assert_eq!(automatic.name, "tool-linux-amd64");
    }

    #[test]
    fn does_not_match_aliases_inside_other_tokens() {
        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: None,
            assets: vec![asset("tool-linux-x64ish"), asset("tool-linux-armv6")],
        };

        for platform in [Platform::LinuxX86_64, Platform::LinuxAarch64] {
            assert!(
                release
                    .find_asset("tool", platform, &mut BTreeSet::new())
                    .is_err()
            );
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
    fn prefers_plain_name_before_canonical_aliases() {
        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: None,
            assets: vec![asset("tool-linux-amd64"), asset("tool-fips-linux-x86_64")],
        };

        let matched = release
            .find_asset("tool", Platform::LinuxX86_64, &mut BTreeSet::new())
            .unwrap();

        assert_eq!(matched.name, "tool-linux-amd64");
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
    fn prefers_raw_asset_over_tar_archive_but_single_file_gzip_over_both() {
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

            let expected = if other_gzip {
                "tool-darwin-arm64.gz"
            } else {
                "tool-macos-arm64"
            };
            assert_eq!(matched.name, expected);
        }
    }

    #[test]
    fn selects_yq_raw_binary_over_tar_archive_for_every_platform() {
        let release = Release {
            tag: "v4.54.3".to_owned(),
            published_at: None,
            assets: vec![
                asset("checksums"),
                asset("checksums-bsd"),
                asset("checksums_hashes_order"),
                asset("extract-checksum.sh"),
                asset("yq_darwin_amd64"),
                asset("yq_darwin_amd64.tar.gz"),
                asset("yq_darwin_arm64"),
                asset("yq_darwin_arm64.tar.gz"),
                asset("yq_freebsd_amd64"),
                asset("yq_freebsd_amd64.tar.gz"),
                asset("yq_linux_386"),
                asset("yq_linux_386.tar.gz"),
                asset("yq_linux_amd64"),
                asset("yq_linux_amd64.tar.gz"),
                asset("yq_linux_arm64"),
                asset("yq_linux_arm64.tar.gz"),
                asset("yq_linux_mips"),
                asset("yq_man_page_only.tar.gz"),
                asset("yq_windows_amd64.exe"),
                asset("yq_windows_amd64.zip"),
            ],
        };
        let expected = [
            (Platform::MacosAarch64, "yq_darwin_arm64"),
            (Platform::MacosX86_64, "yq_darwin_amd64"),
            (Platform::LinuxAarch64, "yq_linux_arm64"),
            (Platform::LinuxX86_64, "yq_linux_amd64"),
        ];

        for (platform, name) in expected {
            let mut emitted_warnings = BTreeSet::new();
            let matched = release
                .find_asset("yq", platform, &mut emitted_warnings)
                .unwrap();

            assert_eq!(matched.name, name);
            assert!(
                emitted_warnings
                    .iter()
                    .any(|warning| warning.contains("over an archive")
                        && warning.contains(&format!("{name}.tar.gz"))),
                "{emitted_warnings:?}"
            );
        }
    }

    #[test]
    fn still_selects_archive_only_releases() {
        let release = Release {
            tag: "v0.11.0".to_owned(),
            published_at: None,
            assets: vec![
                asset("shellcheck-v0.11.0.darwin.aarch64.tar.gz"),
                asset("shellcheck-v0.11.0.linux.x86_64.tar.gz"),
            ],
        };

        let matched = release
            .find_asset("shellcheck", Platform::LinuxX86_64, &mut BTreeSet::new())
            .unwrap();

        assert_eq!(matched.name, "shellcheck-v0.11.0.linux.x86_64.tar.gz");
    }

    fn sidecars_for(name: &str) -> Vec<ReleaseAsset> {
        [
            ".asc",
            ".sig",
            ".minisig",
            ".sha256",
            ".sha384",
            ".sha3-256",
            ".sha512",
            ".sha512sum",
            ".pem",
            ".md5sum",
            ".sha1sum",
            ".sbom.json",
            ".sbom.json.gz",
            ".intoto.jsonl",
        ]
        .iter()
        .map(|suffix| asset(&format!("{name}{suffix}")))
        .collect()
    }

    #[test]
    fn never_selects_any_checksum_or_debug_file_over_a_variant_archive() {
        for suffix in [
            ".sha224",
            ".sha384",
            ".sha384sum",
            ".sha256sums",
            ".sha3-256",
            ".md5sums",
            ".debug",
        ] {
            let release = Release {
                tag: "v1.0.0".to_owned(),
                published_at: None,
                assets: vec![
                    asset("tool-linux-amd64-musl.tar.gz"),
                    asset(&format!("tool-linux-amd64-musl{suffix}")),
                ],
            };

            let matched = release
                .find_asset("tool", Platform::LinuxX86_64, &mut BTreeSet::new())
                .unwrap();

            assert_eq!(matched.name, "tool-linux-amd64-musl.tar.gz", "{suffix}");
        }
    }

    #[test]
    fn never_selects_sidecars_in_mixed_raw_archive_releases() {
        let cases: [(&[&str], &str); 4] = [
            (&["tool-linux-amd64.tar.gz"], "tool-linux-amd64.tar.gz"),
            (
                &["tool-linux-amd64", "tool-linux-amd64.tar.gz"],
                "tool-linux-amd64",
            ),
            (
                &["tool-linux-amd64.gz", "tool-linux-amd64.tar.gz"],
                "tool-linux-amd64.gz",
            ),
            (&["tool-linux-amd64.tgz"], "tool-linux-amd64.tgz"),
        ];

        for (installables, expected) in cases {
            let mut assets = vec![asset("checksums.txt"), asset("SHA512SUMS.asc")];
            for name in installables {
                assets.push(asset(name));
                assets.extend(sidecars_for(name));
            }
            let release = Release {
                tag: "v1.0.0".to_owned(),
                published_at: None,
                assets,
            };

            let matched = release
                .find_asset("tool", Platform::LinuxX86_64, &mut BTreeSet::new())
                .unwrap();

            assert_eq!(matched.name, expected, "{installables:?}");
        }
    }

    #[test]
    fn prefers_plain_archive_over_lone_gzip_variant() {
        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: None,
            assets: vec![
                asset("tool-linux-amd64.tar.gz"),
                asset("tool-fips-linux-amd64.gz"),
            ],
        };

        let matched = release
            .find_asset("tool", Platform::LinuxX86_64, &mut BTreeSet::new())
            .unwrap();

        assert_eq!(matched.name, "tool-linux-amd64.tar.gz");
    }

    #[test]
    fn never_installs_unsupported_tar_compressions_next_to_tar_gz() {
        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: None,
            assets: vec![
                asset("tool-x86_64-unknown-linux-musl.tar.bz2"),
                asset("tool-x86_64-unknown-linux-musl.tar.lz4"),
                asset("tool-x86_64-unknown-linux-musl.tar.gz"),
            ],
        };

        let matched = release
            .find_asset("tool", Platform::LinuxX86_64, &mut BTreeSet::new())
            .unwrap();

        assert_eq!(matched.name, "tool-x86_64-unknown-linux-musl.tar.gz");
    }

    #[test]
    fn rejects_releases_with_only_sidecars_for_the_platform() {
        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: None,
            assets: sidecars_for("tool-linux-amd64"),
        };

        let error = release
            .find_asset("tool", Platform::LinuxX86_64, &mut BTreeSet::new())
            .unwrap_err();

        assert!(error.to_string().starts_with("no release asset matched"));
    }

    #[test]
    fn ranks_every_archive_suffix_below_raw_binaries() {
        for suffix in [
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
            let name = format!("tool-linux-amd64{suffix}");
            assert!(is_archive(&name), "{name}");
            assert!(!is_single_file_gzip(&name), "{name}");
        }
        assert!(!is_archive("tool-linux-amd64"));
        assert!(!is_archive("tool-linux-amd64.gz"));
        assert!(is_single_file_gzip("tool-linux-amd64.GZ"));
    }

    #[test]
    fn prefers_supported_tar_gz_over_unsupported_zip_and_tar_xz() {
        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: None,
            assets: vec![
                asset("tool-x86_64-unknown-linux-musl.zip"),
                asset("tool-x86_64-unknown-linux-musl.tar.xz"),
                asset("tool-x86_64-unknown-linux-musl.tar.gz"),
            ],
        };
        let mut warnings = BTreeSet::new();

        let matched = release
            .find_asset("tool", Platform::LinuxX86_64, &mut warnings)
            .unwrap();

        // Holds whether the other formats are unsupported or merely ranked lower.
        assert_eq!(matched.name, "tool-x86_64-unknown-linux-musl.tar.gz");
        for dropped in ["musl.zip", "musl.tar.xz"] {
            assert!(warnings.iter().any(|w| w.contains(dropped)), "{warnings:?}");
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
    fn expands_target_pattern_for_cargo_nextest() {
        let release = Release {
            tag: "cargo-nextest-0.9.143".to_owned(),
            published_at: None,
            assets: vec![
                asset("cargo-nextest-0.9.143-universal-apple-darwin.tar.gz"),
                asset("cargo-nextest-0.9.143-aarch64-unknown-linux-musl.tar.gz"),
                asset("cargo-nextest-0.9.143-x86_64-unknown-linux-musl.tar.gz"),
            ],
        };

        let matched = release
            .find_asset_by_pattern(
                "cargo-nextest-{version}-{target}.tar.gz",
                "0.9.143",
                Platform::MacosAarch64,
            )
            .unwrap();

        assert_eq!(
            matched.name,
            "cargo-nextest-0.9.143-universal-apple-darwin.tar.gz"
        );
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

        for platform in Platform::LEGACY_DEFAULT {
            release
                .find_asset("tool", platform, &mut emitted_warnings)
                .unwrap();
        }

        assert_eq!(emitted_warnings.len(), 1);
    }

    #[test]
    fn prefers_raw_binary_over_archive_with_preferred_alias() {
        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: None,
            assets: vec![asset("tool-linux-amd64"), asset("tool-linux-x86_64.tar.gz")],
        };

        let matched = release
            .find_asset("tool", Platform::LinuxX86_64, &mut BTreeSet::new())
            .unwrap();

        assert_eq!(matched.name, "tool-linux-amd64");
    }

    #[test]
    fn prefers_plain_archive_over_variant_in_every_format() {
        for suffix in [".tar.xz", ".txz", ".zip", ".tar.zst", ".tar.bz2", ".tbz2"] {
            let plain = format!("tool-linux-amd64{suffix}");
            let release = Release {
                tag: "v1.0.0".to_owned(),
                published_at: None,
                assets: vec![
                    asset(&plain),
                    asset(&format!("tool-fips-linux-amd64{suffix}")),
                ],
            };

            let matched = release
                .find_asset("tool", Platform::LinuxX86_64, &mut BTreeSet::new())
                .unwrap();

            assert_eq!(matched.name, plain);
        }

        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: None,
            assets: vec![
                asset("tool-linux-amd64.tar.xz"),
                asset("tool-fips-linux-amd64.tar.gz"),
            ],
        };
        let matched = release
            .find_asset("tool", Platform::LinuxX86_64, &mut BTreeSet::new())
            .unwrap();
        assert_eq!(matched.name, "tool-linux-amd64.tar.xz");
    }
}
