use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::fmt::{self, Display};

/// Release target id. Variant order is the canonical target order used for
/// sorting and serialization; new ids (for example ABI variants) are new variants.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "String", into = "String")]
pub(crate) enum Platform {
    MacosAarch64,
    MacosX86_64,
    LinuxAarch64,
    LinuxX86_64,
}

impl Platform {
    pub(crate) const ALL: [Self; 4] = [
        Self::MacosAarch64,
        Self::MacosX86_64,
        Self::LinuxAarch64,
        Self::LinuxX86_64,
    ];

    pub(crate) fn current() -> Result<Self> {
        Self::try_from((std::env::consts::OS, std::env::consts::ARCH))
    }

    pub(crate) const fn os_aliases(self) -> &'static [&'static str] {
        match self {
            Self::MacosAarch64 | Self::MacosX86_64 => &["macos", "darwin"],
            Self::LinuxAarch64 | Self::LinuxX86_64 => &["linux"],
        }
    }

    pub(crate) const fn arch_aliases(self) -> &'static [&'static str] {
        match self {
            Self::MacosAarch64 | Self::LinuxAarch64 => &["aarch64", "arm64"],
            Self::MacosX86_64 | Self::LinuxX86_64 => &["x86_64", "amd64", "x64", "x86-64"],
        }
    }

    /// True when `name` contains one of this platform's OS aliases as a token.
    pub(crate) fn os_in(self, name: &str) -> bool {
        self.os_aliases()
            .iter()
            .any(|alias| contains_token(name, alias))
    }

    /// True when `name` contains one of this platform's architecture aliases as a token.
    pub(crate) fn arch_in(self, name: &str) -> bool {
        self.arch_aliases()
            .iter()
            .any(|alias| contains_token(name, alias))
    }

    pub(crate) const fn target(self) -> &'static str {
        match self {
            Self::MacosAarch64 | Self::MacosX86_64 => "universal-apple-darwin",
            Self::LinuxAarch64 => "aarch64-unknown-linux-musl",
            Self::LinuxX86_64 => "x86_64-unknown-linux-musl",
        }
    }
}

/// Returns `targets` in canonical order, rejecting empty and duplicate lists
/// instead of silently de-duplicating them.
pub(crate) fn canonical_targets(targets: &[Platform]) -> Result<Vec<Platform>> {
    ensure!(!targets.is_empty(), "targets must not be empty");

    let mut canonical = targets.to_vec();
    canonical.sort();
    if let Some(pair) = canonical.windows(2).find(|pair| pair[0] == pair[1]) {
        bail!("duplicate target `{}`", pair[0]);
    }

    Ok(canonical)
}

/// Case-insensitive token match: `alias` must not be glued to other
/// alphanumerics, so `x64` never matches inside `x64ish` and `arm64` never
/// matches `aarch64`-only names by accident.
pub(crate) fn contains_token(name: &str, alias: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let alias = alias.to_ascii_lowercase();
    let bytes = name.as_bytes();

    name.match_indices(alias.as_str()).any(|(start, _)| {
        let end = start + alias.len();
        let before = start.checked_sub(1).map(|index| bytes[index]);
        let after = bytes.get(end).copied();

        !before.is_some_and(|byte| byte.is_ascii_alphanumeric())
            && !after.is_some_and(|byte| byte.is_ascii_alphanumeric())
    })
}

impl TryFrom<(&str, &str)> for Platform {
    type Error = anyhow::Error;

    fn try_from((os, arch): (&str, &str)) -> Result<Self> {
        match (os, arch) {
            ("macos", "aarch64") => Ok(Self::MacosAarch64),
            ("macos", "x86_64") => Ok(Self::MacosX86_64),
            ("linux", "aarch64") => Ok(Self::LinuxAarch64),
            ("linux", "x86_64") => Ok(Self::LinuxX86_64),
            _ => bail!("unsupported platform: {os}-{arch}"),
        }
    }
}

impl TryFrom<String> for Platform {
    type Error = anyhow::Error;

    fn try_from(id: String) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|platform| platform.to_string() == id)
            .ok_or_else(|| {
                let supported = Self::ALL.map(|platform| platform.to_string()).join(", ");
                anyhow::anyhow!("unknown target `{id}`; supported targets: {supported}")
            })
    }
}

impl From<Platform> for String {
    fn from(platform: Platform) -> Self {
        platform.to_string()
    }
}

impl Display for Platform {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::MacosAarch64 => "macos-aarch64",
            Self::MacosX86_64 => "macos-x86_64",
            Self::LinuxAarch64 => "linux-aarch64",
            Self::LinuxX86_64 => "linux-x86_64",
        };

        formatter.write_str(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_supported_platforms() {
        assert_eq!(
            Platform::try_from(("macos", "aarch64")).unwrap(),
            Platform::MacosAarch64
        );
        assert_eq!(
            Platform::try_from(("linux", "x86_64")).unwrap(),
            Platform::LinuxX86_64
        );
        assert_eq!(Platform::MacosAarch64.to_string(), "macos-aarch64");
    }

    #[test]
    fn parses_canonical_target_ids_in_canonical_order() {
        let ids = Platform::ALL.map(|platform| platform.to_string());

        assert_eq!(
            ids,
            [
                "macos-aarch64",
                "macos-x86_64",
                "linux-aarch64",
                "linux-x86_64"
            ]
        );
        for (platform, id) in Platform::ALL.into_iter().zip(ids) {
            assert_eq!(Platform::try_from(id).unwrap(), platform);
        }
        assert!(Platform::ALL.is_sorted());
        for id in ["windows-x86_64", "linux-x86_64-gnu", "Linux-x86_64", ""] {
            let error = Platform::try_from(id.to_owned()).unwrap_err();
            assert_eq!(
                error.to_string(),
                format!(
                    "unknown target `{id}`; supported targets: macos-aarch64, macos-x86_64, linux-aarch64, linux-x86_64"
                )
            );
        }
    }

    #[test]
    fn canonicalizes_target_lists() {
        assert_eq!(
            canonical_targets(&[Platform::LinuxX86_64, Platform::MacosAarch64]).unwrap(),
            [Platform::MacosAarch64, Platform::LinuxX86_64]
        );
        assert_eq!(
            canonical_targets(&[]).unwrap_err().to_string(),
            "targets must not be empty"
        );
        assert_eq!(
            canonical_targets(&[
                Platform::LinuxX86_64,
                Platform::MacosAarch64,
                Platform::LinuxX86_64
            ])
            .unwrap_err()
            .to_string(),
            "duplicate target `linux-x86_64`"
        );
    }

    #[test]
    fn rejects_unsupported_platform() {
        let error = Platform::try_from(("windows", "x86_64")).unwrap_err();

        assert_eq!(error.to_string(), "unsupported platform: windows-x86_64");
    }

    #[test]
    fn matches_architecture_alias_matrix_as_tokens() {
        for alias in ["x64", "amd64", "x86-64", "x86_64", "X64", "AMD64"] {
            let name = format!("tool_1.0.0_linux_{alias}.tar.gz");
            assert!(Platform::LinuxX86_64.arch_in(&name), "{name}");
            assert!(!Platform::LinuxAarch64.arch_in(&name), "{name}");
        }
        for alias in ["arm64", "aarch64", "ARM64"] {
            let name = format!("tool_1.0.0_darwin_{alias}.tar.gz");
            assert!(Platform::MacosAarch64.arch_in(&name), "{name}");
            assert!(!Platform::MacosX86_64.arch_in(&name), "{name}");
        }
    }

    #[test]
    fn rejects_aliases_glued_to_other_tokens() {
        assert!(!contains_token("tool_linux_x64ish", "x64"));
        assert!(!contains_token("tool_linux_ax64", "x64"));
        assert!(!contains_token("tool_linux_x32", "x64"));
        assert!(!contains_token("tool_linux_armv6", "arm64"));
        assert!(!Platform::LinuxX86_64.os_in("tool_linuxish_x64"));
        assert!(Platform::MacosAarch64.os_in("tool.darwin.arm64.tar.gz"));
        assert!(contains_token("tool-x86-64", "x86-64"));
    }

    #[test]
    fn maps_platforms_to_portable_release_targets() {
        assert_eq!(Platform::MacosAarch64.target(), "universal-apple-darwin");
        assert_eq!(Platform::MacosX86_64.target(), "universal-apple-darwin");
        assert_eq!(
            Platform::LinuxAarch64.target(),
            "aarch64-unknown-linux-musl"
        );
        assert_eq!(Platform::LinuxX86_64.target(), "x86_64-unknown-linux-musl");
    }
}
