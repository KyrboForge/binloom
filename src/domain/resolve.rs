use crate::common::{validate_version, warn};
use crate::domain::lockfile::{
    ArtifactFormat, ChecksumSource, LockedArtifact, LockedTool, LockedWrapper,
};
use crate::domain::manifest::Tool;
use crate::domain::platform::{Platform, contains_token};
use crate::domain::sources::{Source, bare_version, release};
use crate::download;
use crate::download::Client;
use anyhow::{Context, bail, ensure};
use std::collections::{BTreeMap, BTreeSet};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

struct ReleaseRequest<'a> {
    name: &'a str,
    source: &'a Source,
    asset_pattern: Option<&'a str>,
    targets: &'a [Platform],
}

pub(crate) fn resolve_tool(
    name: &str,
    tool: &Tool,
    targets: &[Platform],
    version: Option<&str>,
    minimum_age_minutes: u64,
    client: &Client,
) -> anyhow::Result<LockedTool> {
    if let Source::Cargo(source) = &tool.source {
        anyhow::ensure!(
            tool.asset.is_none(),
            "Cargo tools do not support release asset patterns"
        );

        let resolved = source.resolve(client, version)?;
        let subject = format!("{source}@{}", resolved.version);

        ensure_minimum_age(
            &subject,
            &resolved.published_at,
            minimum_age_minutes,
            OffsetDateTime::now_utc(),
        )?;

        println!("Found {} for {name}:", resolved.version);

        return Ok(LockedTool {
            version: resolved.version,
            source: source.to_string(),
            tag: None,
            sha256: Some(resolved.checksum),
            ..LockedTool::default()
        });
    }

    let provider = tool.source.release_provider()?;
    let mut checksum_cache = BTreeMap::new();

    let release = match version {
        Some(version) => provider.fetch_release(client, name, version)?,
        None => provider.fetch_latest_release(client)?,
    };

    resolve_release(
        ReleaseRequest {
            name,
            source: &tool.source,
            asset_pattern: tool.asset.as_deref(),
            targets,
        },
        &release,
        minimum_age_minutes,
        client,
        &mut checksum_cache,
    )
}

pub(crate) fn resolve_binloom(
    source: &Source,
    version: Option<&str>,
    minimum_age_minutes: u64,
    client: &Client,
) -> anyhow::Result<(LockedTool, LockedWrapper)> {
    let provider = source.release_provider()?;

    let release = match version {
        Some(version) => provider.fetch_release(client, "binloom", version)?,
        None => provider.fetch_latest_release(client)?,
    };

    resolve_binloom_release(source, &release, minimum_age_minutes, client)
}

fn resolve_checksum(
    client: &Client,
    release: &release::Release,
    asset: &release::ReleaseAsset,
    checksum_cache: &mut BTreeMap<String, String>,
) -> anyhow::Result<(String, ChecksumSource)> {
    if let Some(checksum) = &asset.sha256 {
        return Ok((checksum.clone(), ChecksumSource::Digest));
    }

    if let Some(checksum) = release.checksum_from_sidecar(client, asset, checksum_cache)? {
        return Ok((checksum, ChecksumSource::Sidecar));
    }

    warn(&format!(
        "{} has no published checksum; hashing downloaded bytes (TOFU)",
        asset.name
    ));

    Ok((
        download::sha256_url(client, &asset.download_url)?,
        ChecksumSource::Download,
    ))
}

fn version_from_tag(tag: &str, name: &str) -> anyhow::Result<String> {
    // Same stripping as `binloom add`, so the lock matches the manifest.
    let version = bare_version(name, tag);

    validate_version(version)
        .with_context(|| format!("release tag {tag} contains an unsafe version"))?;
    // `tool-tool-nightly` strips to `tool-nightly`, which the manifest would reject.
    ensure!(
        bare_version(name, version) == version,
        "release tag {tag} does not name a stable version for {name}"
    );

    Ok(version.to_owned())
}

fn resolve_release(
    request: ReleaseRequest<'_>,
    release: &release::Release,
    minimum_age_minutes: u64,
    client: &Client,
    checksum_cache: &mut BTreeMap<String, String>,
) -> anyhow::Result<LockedTool> {
    println!("Found {} for {}:", release.tag, request.name);
    ensure_minimum_release_age(release, minimum_age_minutes, OffsetDateTime::now_utc())?;
    let version = version_from_tag(&release.tag, request.name)?;
    let mut artifacts = BTreeMap::new();
    let mut emitted_warnings = BTreeSet::new();
    let mut selected = Vec::with_capacity(request.targets.len());
    for &platform in request.targets {
        let asset = match request.asset_pattern {
            Some(pattern) => release.find_asset_by_pattern(pattern, &version, platform)?,
            None => release.find_asset(request.name, platform, &mut emitted_warnings)?,
        };
        selected.push((platform, asset));
    }
    ensure_platform_specific_assets(request.name, &version, &selected)?;

    for (platform, asset) in selected {
        let (sha256, checksum_source) = resolve_checksum(client, release, asset, checksum_cache)?;
        artifacts.insert(
            platform.to_string(),
            LockedArtifact {
                asset: asset.name.clone(),
                url: asset.download_url.clone(),
                sha256,
                format: ArtifactFormat::try_from(asset.name.as_str())?,
                checksum_source,
            },
        );
    }

    Ok(LockedTool {
        version,
        source: request.source.to_string(),
        tag: Some(release.tag.clone()),
        artifacts,
        ..LockedTool::default()
    })
}

/// Fail closed when a lock would hand one platform another platform's asset.
///
/// An asset is rejected for a platform when its name carries an OS or
/// architecture token of a different platform, or when the same asset is
/// locked for several platforms. The one tolerated sharing is a macOS
/// universal binary: both macOS targets, a `universal` token and no
/// architecture token in the name; an unlabelled archive is not evidence.
/// Platform tokens are read with the tool name and version blanked out, so a
/// tool called `linux-tool` can still lock a macOS asset.
fn ensure_platform_specific_assets(
    name: &str,
    version: &str,
    selected: &[(Platform, &release::ReleaseAsset)],
) -> anyhow::Result<()> {
    const HINT: &str =
        "use {os}, {arch} or {target} in the asset pattern so every platform locks its own asset";
    // OSes Binloom cannot target yet; naming one never matches any platform.
    const UNSUPPORTED_OS_ALIASES: [&str; 3] = ["windows", "win32", "win64"];
    // `v1.0.0` must go before `1.0.0`: the `v` would otherwise glue the bare
    // version to an alphanumeric and keep it visible.
    let tagged = if version.is_empty() {
        String::new()
    } else {
        format!("v{version}")
    };
    let platform_part = |asset: &release::ReleaseAsset| {
        [tagged.as_str(), version, name]
            .iter()
            .filter(|part| !part.is_empty())
            .fold(asset.name.to_ascii_lowercase(), |rest, part| {
                blank_token(&rest, &part.to_ascii_lowercase())
            })
    };

    for (platform, asset) in selected {
        let platform_part = platform_part(asset);
        let foreign_os = Platform::ALL.iter().any(|other| {
            other.os_aliases() != platform.os_aliases() && other.os_in(&platform_part)
        }) || UNSUPPORTED_OS_ALIASES
            .iter()
            .any(|alias| contains_token(&platform_part, alias));
        let foreign_arch = Platform::ALL.iter().any(|other| {
            other.arch_aliases() != platform.arch_aliases() && other.arch_in(&platform_part)
        });
        // Name a whole platform only when both its OS and architecture appear
        // in the name, so a Linux asset is never blamed on macOS.
        let foreign = Platform::ALL.iter().find(|other| {
            other != &platform && other.os_in(&platform_part) && other.arch_in(&platform_part)
        });
        let reason = match (foreign, foreign_os, foreign_arch) {
            (Some(foreign), _, _) => format!("is named for {foreign}"),
            (None, true, _) => "names a different OS".to_owned(),
            (None, false, true) => "names a different architecture".to_owned(),
            (None, false, false) => continue,
        };
        bail!(
            "asset {} selected for {platform} {reason}; {HINT}",
            asset.name
        );
    }

    for (index, (platform, asset)) in selected.iter().enumerate() {
        for (other, shared) in &selected[index + 1..] {
            if shared.name != asset.name {
                continue;
            }
            let shared_part = platform_part(asset);
            let universal_macos = platform.os_aliases() == other.os_aliases()
                && platform.os_aliases()[0] == "macos"
                && contains_token(&shared_part, "universal")
                && !platform.arch_in(&shared_part)
                && !other.arch_in(&shared_part);
            if !universal_macos {
                bail!(
                    "asset {} is selected for both {platform} and {other}; {HINT}",
                    asset.name
                );
            }
        }
    }

    Ok(())
}

/// Replaces `token` with `-` wherever it is not glued to ASCII alphanumerics,
/// so a tool named `x` never blanks the `x` out of `x86_64`.
fn blank_token(name: &str, token: &str) -> String {
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
        }
    }
    blanked.push_str(&name[copied..]);
    blanked
}

fn ensure_minimum_age(
    subject: &str,
    published_at: &str,
    minimum_minutes: u64,
    now: OffsetDateTime,
) -> anyhow::Result<()> {
    let published_at = OffsetDateTime::parse(published_at, &Rfc3339)
        .with_context(|| format!("{subject} has invalid publication date"))?;

    let minimum_seconds = minimum_minutes
        .checked_mul(60)
        .and_then(|seconds| i64::try_from(seconds).ok())
        .context("minimum release age is too large")?;

    let minimum_age = Duration::seconds(minimum_seconds);
    let actual_age = now - published_at;

    if actual_age < minimum_age {
        let remaining_seconds = (minimum_age - actual_age).whole_seconds();
        let remaining_minutes = (remaining_seconds + 59) / 60;

        bail!("{subject} is too new; wait {remaining_minutes} more minute(s)");
    }

    Ok(())
}

fn ensure_minimum_release_age(
    release: &release::Release,
    minimum_minutes: u64,
    now: OffsetDateTime,
) -> anyhow::Result<()> {
    let subject = format!("release {}", release.tag);
    let published_at = release
        .published_at
        .as_deref()
        .with_context(|| format!("{subject} has no publication date"))?;

    ensure_minimum_age(&subject, published_at, minimum_minutes, now)
}

fn resolve_binloom_release(
    source: &Source,
    release: &release::Release,
    minimum_age_minutes: u64,
    client: &Client,
) -> anyhow::Result<(LockedTool, LockedWrapper)> {
    let mut checksum_cache = BTreeMap::new();
    let binloom = resolve_release(
        ReleaseRequest {
            name: "binloom",
            source,
            asset_pattern: None,
            targets: &Platform::ALL,
        },
        release,
        minimum_age_minutes,
        client,
        &mut checksum_cache,
    )?;
    for artifact in binloom.artifacts.values() {
        if !matches!(artifact.format, ArtifactFormat::Raw | ArtifactFormat::Gz) {
            bail!(
                "unsupported Binloom bootstrap asset: {}; binloomw supports only raw and gz",
                artifact.asset
            );
        }
    }
    let asset = release.find_asset_by_name("binloomw")?;
    let (sha256, checksum_source) = resolve_checksum(client, release, asset, &mut checksum_cache)?;

    let wrapper = LockedWrapper {
        version: binloom.version.clone(),
        url: asset.download_url.clone(),
        sha256,
        checksum_source,
    };

    Ok((binloom, wrapper))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::manifest::Manifest;
    use crate::domain::sources::release::{Release, ReleaseAsset};
    use crate::domain::sources::{CargoSource, release_tags};
    use crate::http_fixture::{Response, Server};
    use std::fs;

    fn asset(name: &str) -> ReleaseAsset {
        ReleaseAsset {
            name: name.to_owned(),
            download_url: format!("https://example.com/{name}"),
            sha256: None,
        }
    }

    #[test]
    fn enforces_minimum_release_age() {
        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: Some("2026-01-01T12:00:00Z".to_owned()),
            assets: vec![],
        };

        let now = OffsetDateTime::parse("2026-01-02T00:00:00Z", &Rfc3339).unwrap();

        let error = ensure_minimum_release_age(&release, 24 * 60, now).unwrap_err();

        assert_eq!(
            error.to_string(),
            "release v1.0.0 is too new; wait 720 more minute(s)"
        );

        ensure_minimum_release_age(&release, 12 * 60, now).unwrap();
    }

    #[test]
    fn added_versions_round_trip_through_tag_lookup_to_the_lock() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("binloom.toml");

        for (name, input) in [
            ("tool", "nightly"),
            ("tool", "tool-nightly"),
            ("tool", "vnightly"),
            ("tool", "latest-build"),
            ("tool", "2024-01-01"),
            ("tool", "v1.2.3"),
            ("tool", "tool-v1.2.3"),
            ("jq", "jq-1.8.2"),
            ("vtool", "vtool-1.2.3"),
        ] {
            // What `binloom add` writes to binloom.toml.
            let manifest_version = bare_version(name, input);
            fs::write(
                &path,
                format!(
                    "manifest-version = 1\n\n[binloom]\nversion = \"0.1.0\"\n\n[tools.{name}]\nversion = \"{manifest_version}\"\nsource = \"github:owner/{name}\"\n"
                ),
            )
            .unwrap();
            let manifest = Manifest::try_from(path.as_path()).unwrap();
            let tags = release_tags(name, &manifest.tools[name].version);

            assert!(tags.contains(&input.to_owned()), "{input}: {tags:?}");
            for tag in tags {
                assert_eq!(
                    version_from_tag(&tag, name).unwrap(),
                    manifest.tools[name].version,
                    "{input} resolved as {tag}"
                );
            }
        }
    }

    #[test]
    fn rejects_unsafe_release_tags() {
        assert_eq!(version_from_tag("v1.2.3", "tool").unwrap(), "1.2.3");
        assert_eq!(version_from_tag("1.2.3", "tool").unwrap(), "1.2.3");

        for tag in [
            "v/tmp/x",
            "v../../x",
            r"v..\..\x",
            ".hidden",
            "tool-tool-nightly",
            "tool-tool-v1.2.3",
        ] {
            assert!(version_from_tag(tag, "tool").is_err(), "{tag:?}");
        }
    }

    #[test]
    fn rejects_tar_archives_only_for_binloom_bootstrap() {
        let source = Source::try_from("github:KyrboForge/binloom".to_owned()).unwrap();
        let client = download::client();

        for suffix in [
            "", ".gz", ".tar.gz", ".tgz", ".tar.xz", ".zip", ".tar.zst", ".tar.bz2",
        ] {
            let mut assets: Vec<_> = Platform::ALL
                .iter()
                .map(|platform| asset(&format!("binloom-{platform}{suffix}")))
                .collect();
            assets.push(asset("binloomw"));
            for asset in &mut assets {
                asset.sha256 = Some("a".repeat(64));
            }
            let release = Release {
                tag: "v1.0.0".to_owned(),
                published_at: Some("2026-01-01T00:00:00Z".to_owned()),
                assets,
            };

            resolve_release(
                ReleaseRequest {
                    name: "binloom",
                    source: &source,
                    asset_pattern: None,
                    targets: &Platform::ALL,
                },
                &release,
                0,
                &client,
                &mut BTreeMap::new(),
            )
            .unwrap();

            let result = resolve_binloom_release(&source, &release, 0, &client);
            if !matches!(suffix, "" | ".gz") {
                let error = result.unwrap_err().to_string();
                assert!(
                    error.contains("unsupported Binloom bootstrap asset"),
                    "{error}"
                );
                assert!(
                    error.contains("binloomw supports only raw and gz"),
                    "{error}"
                );
            } else {
                result.unwrap();
            }
        }
    }

    fn gitleaks_release() -> Release {
        let checksum = "a".repeat(64);
        let assets = [
            "gitleaks_8.30.1_darwin_arm64.tar.gz",
            "gitleaks_8.30.1_darwin_x64.tar.gz",
            "gitleaks_8.30.1_linux_arm64.tar.gz",
            "gitleaks_8.30.1_linux_x64.tar.gz",
            "gitleaks_8.30.1_windows_x64.zip",
        ];

        Release {
            tag: "v8.30.1".to_owned(),
            published_at: Some("2025-11-01T00:00:00Z".to_owned()),
            assets: assets
                .iter()
                .map(|name| ReleaseAsset {
                    sha256: Some(checksum.clone()),
                    ..asset(name)
                })
                .collect(),
        }
    }

    fn resolve_gitleaks(pattern: Option<&str>) -> anyhow::Result<LockedTool> {
        resolve_release(
            ReleaseRequest {
                name: "gitleaks",
                source: &Source::try_from("github:gitleaks/gitleaks".to_owned()).unwrap(),
                asset_pattern: pattern,
                targets: &Platform::ALL,
            },
            &gitleaks_release(),
            0,
            &download::client(),
            &mut BTreeMap::new(),
        )
    }

    #[test]
    fn rejects_single_platform_asset_pattern_for_every_platform() {
        let error = resolve_gitleaks(Some("gitleaks_{version}_darwin_arm64.tar.gz"))
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("selected for macos-x86_64 is named for macos-aarch64"),
            "{error}"
        );
        assert!(error.contains("{os}, {arch} or {target}"), "{error}");

        let error = resolve_gitleaks(Some("gitleaks_{version}_linux_arm64.tar.gz"))
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("selected for macos-aarch64 is named for linux-aarch64"),
            "{error}"
        );

        // A Linux x64 asset is blamed on linux-x86_64, never on macos-x86_64.
        let error = resolve_gitleaks(Some("gitleaks_{version}_linux_x64.tar.gz"))
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("selected for macos-aarch64 is named for linux-x86_64"),
            "{error}"
        );
    }

    #[test]
    fn ignores_platform_words_in_the_tool_name_and_version() {
        let macos = asset("linux-tool-1.0.0-macos-arm64.gz");

        ensure_platform_specific_assets("linux-tool", "1.0.0", &[(Platform::MacosAarch64, &macos)])
            .unwrap();

        let tagged = asset("tool-v1.0.0-linux-macos-arm64.gz");
        ensure_platform_specific_assets(
            "tool",
            "1.0.0-linux",
            &[(Platform::MacosAarch64, &tagged)],
        )
        .unwrap();

        // Only whole tokens are blanked: a tool named `x` keeps `x86_64` visible.
        let linux = asset("x-1.0.0-linux-x86_64.tar.gz");
        assert!(
            ensure_platform_specific_assets("x", "1.0.0", &[(Platform::MacosAarch64, &linux)])
                .is_err()
        );
    }

    #[test]
    fn names_only_the_conflicting_dimension_without_a_full_platform_match() {
        let linux = asset("tool-linux-armv7.tar.gz");
        let error =
            ensure_platform_specific_assets("tool", "1.0.0", &[(Platform::MacosAarch64, &linux)])
                .unwrap_err()
                .to_string();
        assert!(error.contains("names a different OS"), "{error}");

        let x64 = asset("tool-freebsd-x64.tar.gz");
        let error =
            ensure_platform_specific_assets("tool", "1.0.0", &[(Platform::MacosAarch64, &x64)])
                .unwrap_err()
                .to_string();
        assert!(error.contains("names a different architecture"), "{error}");
    }

    #[test]
    fn locks_platform_specific_assets_from_pattern_and_auto_matching() {
        for pattern in [Some("gitleaks_{version}_{os}_{arch}.tar.gz"), None] {
            let locked = resolve_gitleaks(pattern).unwrap();
            let names: Vec<_> = locked
                .artifacts
                .iter()
                .map(|(platform, artifact)| (platform.as_str(), artifact.asset.as_str()))
                .collect();

            assert_eq!(
                names,
                [
                    ("linux-aarch64", "gitleaks_8.30.1_linux_arm64.tar.gz"),
                    ("linux-x86_64", "gitleaks_8.30.1_linux_x64.tar.gz"),
                    ("macos-aarch64", "gitleaks_8.30.1_darwin_arm64.tar.gz"),
                    ("macos-x86_64", "gitleaks_8.30.1_darwin_x64.tar.gz"),
                ]
            );
        }
    }

    #[test]
    fn rejects_platform_invariant_asset_shared_by_all_platforms() {
        let shared = asset("tool-1.0.0.tar.gz");
        let selected: Vec<_> = Platform::ALL.iter().map(|p| (*p, &shared)).collect();

        let error = ensure_platform_specific_assets("tool", "1.0.0", &selected)
            .unwrap_err()
            .to_string();

        assert!(error.contains("is selected for both"), "{error}");
    }

    #[test]
    fn rejects_unlabelled_archive_shared_by_both_macos_targets() {
        let unlabelled = asset("tool-1.0.0.tar.gz");

        let error = ensure_platform_specific_assets(
            "tool",
            "1.0.0",
            &[
                (Platform::MacosAarch64, &unlabelled),
                (Platform::MacosX86_64, &unlabelled),
            ],
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("is selected for both"), "{error}");
    }

    #[test]
    fn allows_universal_macos_asset_but_not_shared_linux_asset() {
        let universal = asset("tool-universal-apple-darwin.tar.gz");
        let linux = asset("tool-linux.tar.gz");
        let aarch64 = asset("tool-aarch64-unknown-linux-musl.tar.gz");
        let x86_64 = asset("tool-x86_64-unknown-linux-musl.tar.gz");

        ensure_platform_specific_assets(
            "tool",
            "1.0.0",
            &[
                (Platform::MacosAarch64, &universal),
                (Platform::MacosX86_64, &universal),
                (Platform::LinuxAarch64, &aarch64),
                (Platform::LinuxX86_64, &x86_64),
            ],
        )
        .unwrap();

        assert!(
            ensure_platform_specific_assets(
                "tool",
                "1.0.0",
                &[
                    (Platform::LinuxAarch64, &linux),
                    (Platform::LinuxX86_64, &linux),
                ]
            )
            .is_err()
        );
        assert!(
            ensure_platform_specific_assets("tool", "1.0.0", &[(Platform::MacosAarch64, &x86_64)])
                .is_err()
        );
    }

    fn resolve_targets(
        assets: &[&str],
        pattern: Option<&str>,
        targets: &[Platform],
    ) -> anyhow::Result<LockedTool> {
        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: Some("2026-01-01T00:00:00Z".to_owned()),
            assets: assets
                .iter()
                .map(|name| ReleaseAsset {
                    sha256: Some("a".repeat(64)),
                    ..asset(name)
                })
                .collect(),
        };
        resolve_release(
            ReleaseRequest {
                name: "tool",
                source: &Source::try_from("github:owner/tool".to_owned()).unwrap(),
                asset_pattern: pattern,
                targets,
            },
            &release,
            0,
            &download::client(),
            &mut BTreeMap::new(),
        )
    }

    #[test]
    fn resolves_exactly_the_requested_targets() {
        let linux = ["tool-linux-arm64.tar.gz", "tool-linux-x64.tar.gz"];
        let locked = resolve_targets(
            &linux,
            None,
            &[Platform::LinuxAarch64, Platform::LinuxX86_64],
        )
        .unwrap();
        assert_eq!(
            locked.artifacts.keys().collect::<Vec<_>>(),
            ["linux-aarch64", "linux-x86_64"]
        );

        // A required target without an artifact fails instead of being dropped.
        let error = resolve_targets(
            &linux,
            None,
            &[Platform::MacosAarch64, Platform::LinuxX86_64],
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("macos-aarch64"), "{error:#}");
    }

    #[test]
    fn rejects_windows_asset_for_a_single_linux_target() {
        let error = resolve_targets(
            &["tool-1.0.0-windows-x86_64.zip"],
            Some("tool-{version}-windows-{arch}.zip"),
            &[Platform::LinuxX86_64],
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("names a different OS"),
            "{error:#}"
        );
    }

    #[test]
    fn resolves_universal_macos_asset_for_macos_only_targets() {
        let locked = resolve_targets(
            &["tool-universal-apple-darwin.tar.gz"],
            Some("tool-{target}.tar.gz"),
            &[Platform::MacosAarch64, Platform::MacosX86_64],
        )
        .unwrap();
        assert!(
            locked
                .artifacts
                .values()
                .all(|artifact| artifact.asset == "tool-universal-apple-darwin.tar.gz")
        );
        assert_eq!(locked.artifacts.len(), 2);
    }

    #[test]
    fn resolves_version_from_tool_prefixed_tag() {
        assert_eq!(
            version_from_tag("cargo-nextest-0.9.143", "cargo-nextest").unwrap(),
            "0.9.143"
        );
        assert_eq!(version_from_tag("tool-v1.2.3", "tool").unwrap(), "1.2.3");
        assert_eq!(version_from_tag("vtool-1.2.3", "vtool").unwrap(), "1.2.3");
    }

    #[test]
    fn records_embedded_digest_provenance() {
        let checksum = "a".repeat(64);
        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: None,
            assets: vec![ReleaseAsset {
                name: "tool.gz".to_owned(),
                download_url: "https://example.com/tool.gz".to_owned(),
                sha256: Some(checksum.clone()),
            }],
        };
        let client = download::client();
        let mut checksum_cache = BTreeMap::new();

        let resolved =
            resolve_checksum(&client, &release, &release.assets[0], &mut checksum_cache).unwrap();

        assert_eq!(resolved, (checksum, ChecksumSource::Digest));
    }

    #[test]
    fn records_sidecar_checksum_provenance() {
        let checksum = "b".repeat(64);
        let server = Server::start(vec![Response {
            status: 200,
            body: format!("{checksum}  tool.gz\n").into_bytes(),
        }]);
        let mut checksum_cache = BTreeMap::new();

        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: None,
            assets: vec![
                ReleaseAsset {
                    name: "tool.gz".to_owned(),
                    download_url: format!("{}/tool.gz", server.url()),
                    sha256: None,
                },
                ReleaseAsset {
                    name: "tool.gz.sha256".to_owned(),
                    download_url: format!("{}/tool.gz.sha256", server.url()),
                    sha256: None,
                },
            ],
        };
        let client = download::client();

        let resolved =
            resolve_checksum(&client, &release, &release.assets[0], &mut checksum_cache).unwrap();

        assert_eq!(resolved, (checksum, ChecksumSource::Sidecar));
        assert_eq!(server.requests()[0].path, "/tool.gz.sha256");
    }

    #[test]
    fn records_downloaded_checksum_provenance() {
        let server = Server::start(vec![Response {
            status: 200,
            body: b"hello".to_vec(),
        }]);

        let mut checksum_cache = BTreeMap::new();

        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: None,
            assets: vec![ReleaseAsset {
                name: "tool.gz".to_owned(),
                download_url: format!("{}/tool.gz", server.url()),
                sha256: None,
            }],
        };
        let client = download::client();

        let resolved =
            resolve_checksum(&client, &release, &release.assets[0], &mut checksum_cache).unwrap();

        assert_eq!(
            resolved,
            (
                "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".to_owned(),
                ChecksumSource::Download,
            )
        );
        assert_eq!(server.requests()[0].path, "/tool.gz");
    }

    #[test]
    fn reuses_global_checksum_sidecar() {
        let first_checksum = "a".repeat(64);
        let second_checksum = "b".repeat(64);

        let server = Server::start(vec![Response {
            status: 200,
            body: format!(
                "{first_checksum}  tool_linux_x86_64.gz\n\
             {second_checksum}  tool_macos_aarch64.gz\n"
            )
            .into_bytes(),
        }]);

        let release = Release {
            tag: "v1.0.0".to_owned(),
            published_at: None,
            assets: vec![
                asset("tool_linux_x86_64.gz"),
                asset("tool_macos_aarch64.gz"),
                ReleaseAsset {
                    name: "SHA256SUMS".to_owned(),
                    download_url: format!("{}/SHA256SUMS", server.url()),
                    sha256: None,
                },
            ],
        };

        let client = download::client();
        let mut checksum_cache = BTreeMap::new();

        assert_eq!(
            release
                .checksum_from_sidecar(&client, &release.assets[0], &mut checksum_cache)
                .unwrap(),
            Some(first_checksum)
        );

        assert_eq!(
            release
                .checksum_from_sidecar(&client, &release.assets[1], &mut checksum_cache)
                .unwrap(),
            Some(second_checksum)
        );

        assert_eq!(server.requests().len(), 1);
    }

    #[test]
    fn resolves_cargo_package_into_lock_entry() {
        let checksum = "a".repeat(64);
        let server = Server::start(vec![Response {
            status: 200,
            body: format!(
                r#"{{
                "crate": {{
                    "max_version": "1.2.3",
                    "max_stable_version": "1.2.3"
                }},
                "versions": [{{
                    "num": "1.2.3",
                    "checksum": "{checksum}",
                    "created_at": "2026-01-01T00:00:00Z",
                    "yanked": false
                }}]
            }}"#
            )
            .into_bytes(),
        }]);

        let source = CargoSource::try_from("cargo:tool".to_owned()).unwrap();
        let resolved = source
            .resolve_from(&download::client(), Some("1.2.3"), server.url())
            .unwrap();

        ensure_minimum_age(
            "cargo:tool@1.2.3",
            &resolved.published_at,
            0,
            OffsetDateTime::now_utc(),
        )
        .unwrap();

        let locked = LockedTool {
            version: resolved.version,
            source: source.to_string(),
            tag: None,
            sha256: Some(resolved.checksum),
            artifacts: BTreeMap::new(),
            ..LockedTool::default()
        };

        assert_eq!(locked.version, "1.2.3");
        assert_eq!(locked.source, "cargo:tool");
        assert_eq!(locked.sha256.as_deref(), Some(checksum.as_str()));
        assert!(locked.tag.is_none());
        assert!(locked.artifacts.is_empty());
    }
}
