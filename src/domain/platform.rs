use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::fmt::{self, Display};

#[cfg(any(target_os = "linux", test))]
mod linux;

/// Binloom target id. Keep the four legacy variants in their original order.
/// Explicit Linux targets follow, with aarch64 before x86_64 and GNU before musl.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "String", into = "String")]
pub(crate) enum Platform {
    MacosAarch64,
    MacosX86_64,
    LinuxAarch64,
    LinuxX86_64,
    LinuxAarch64Gnu,
    LinuxAarch64Musl,
    LinuxX86_64Gnu,
    LinuxX86_64Musl,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LinuxAbi {
    Gnu,
    Musl,
}

pub(crate) struct Host<'a> {
    pub(crate) os: &'a str,
    pub(crate) arch: &'a str,
}

impl Host<'_> {
    pub(crate) fn current_target(targets: &[Platform]) -> Result<Platform> {
        Self {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
        }
        .target(targets, || {
            #[cfg(target_os = "linux")]
            {
                linux::detect_libc()
            }
            #[cfg(not(target_os = "linux"))]
            {
                None
            }
        })
    }

    /// Match a host using injected libc evidence. Legacy targets need only OS and CPU.
    pub(crate) fn target(
        &self,
        targets: &[Platform],
        libc: impl FnOnce() -> Option<LinuxAbi>,
    ) -> Result<Platform> {
        let legacy = Platform::try_from((self.os, self.arch))?;
        let host = if !targets.contains(&legacy)
            && targets
                .iter()
                .any(|target| target.base_platform() == legacy)
        {
            let abi = libc().ok_or_else(|| {
                anyhow::anyhow!(
                    "could not determine libc for {legacy}; see the Linux target docs or declare `{legacy}` and run `binloom update` to use legacy matching"
                )
            })?;
            match (legacy, abi) {
                (Platform::LinuxAarch64, LinuxAbi::Gnu) => Platform::LinuxAarch64Gnu,
                (Platform::LinuxAarch64, LinuxAbi::Musl) => Platform::LinuxAarch64Musl,
                (Platform::LinuxX86_64, LinuxAbi::Gnu) => Platform::LinuxX86_64Gnu,
                (Platform::LinuxX86_64, LinuxAbi::Musl) => Platform::LinuxX86_64Musl,
                _ => unreachable!("only explicit Linux targets have a distinct base platform"),
            }
        } else {
            legacy
        };
        ensure!(
            targets.contains(&host),
            "this project does not support {host}; [platforms] targets are {}",
            join_targets(targets)
        );
        Ok(host)
    }
}

impl Platform {
    pub(crate) const LEGACY_DEFAULT: [Self; 4] = [
        Self::MacosAarch64,
        Self::MacosX86_64,
        Self::LinuxAarch64,
        Self::LinuxX86_64,
    ];
    pub(crate) const ALL: [Self; 8] = [
        Self::MacosAarch64,
        Self::MacosX86_64,
        Self::LinuxAarch64,
        Self::LinuxX86_64,
        Self::LinuxAarch64Gnu,
        Self::LinuxAarch64Musl,
        Self::LinuxX86_64Gnu,
        Self::LinuxX86_64Musl,
    ];

    /// OS and CPU identity shared by legacy and explicit targets.
    pub(crate) const fn base_platform(self) -> Self {
        match self {
            Self::LinuxAarch64Gnu | Self::LinuxAarch64Musl => Self::LinuxAarch64,
            Self::LinuxX86_64Gnu | Self::LinuxX86_64Musl => Self::LinuxX86_64,
            _ => self,
        }
    }

    pub(crate) const fn linux_abi(self) -> Option<LinuxAbi> {
        match self {
            Self::LinuxAarch64Gnu | Self::LinuxX86_64Gnu => Some(LinuxAbi::Gnu),
            Self::LinuxAarch64Musl | Self::LinuxX86_64Musl => Some(LinuxAbi::Musl),
            _ => None,
        }
    }

    pub(crate) const fn os_aliases(self) -> &'static [&'static str] {
        match self {
            Self::MacosAarch64 | Self::MacosX86_64 => &["macos", "darwin"],
            _ => &["linux"],
        }
    }

    pub(crate) const fn arch_aliases(self) -> &'static [&'static str] {
        match self {
            Self::MacosAarch64
            | Self::LinuxAarch64
            | Self::LinuxAarch64Gnu
            | Self::LinuxAarch64Musl => &["aarch64", "arm64"],
            _ => &["x86_64", "amd64", "x64", "x86-64"],
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

    pub(crate) const fn rust_target(self) -> &'static str {
        match self {
            Self::MacosAarch64 | Self::MacosX86_64 => "universal-apple-darwin",
            Self::LinuxAarch64 | Self::LinuxAarch64Musl => "aarch64-unknown-linux-musl",
            Self::LinuxX86_64 | Self::LinuxX86_64Musl => "x86_64-unknown-linux-musl",
            Self::LinuxAarch64Gnu => "aarch64-unknown-linux-gnu",
            Self::LinuxX86_64Gnu => "x86_64-unknown-linux-gnu",
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

    for target in &canonical {
        let legacy = target.base_platform();
        if *target != legacy && canonical.contains(&legacy) {
            bail!(
                "cannot mix legacy target `{legacy}` with `{target}`; choose either the legacy target or explicit ABI targets for that architecture"
            );
        }
    }
    Ok(canonical)
}

pub(crate) fn join_targets(targets: &[Platform]) -> String {
    targets
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
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
                let supported = join_targets(&Self::ALL);
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
            Self::LinuxAarch64Gnu => "linux-aarch64-gnu",
            Self::LinuxAarch64Musl => "linux-aarch64-musl",
            Self::LinuxX86_64Gnu => "linux-x86_64-gnu",
            Self::LinuxX86_64Musl => "linux-x86_64-musl",
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
                "linux-x86_64",
                "linux-aarch64-gnu",
                "linux-aarch64-musl",
                "linux-x86_64-gnu",
                "linux-x86_64-musl",
            ]
        );
        for (platform, id) in Platform::ALL.into_iter().zip(ids) {
            assert_eq!(Platform::try_from(id).unwrap(), platform);
        }
        assert!(Platform::ALL.is_sorted());
        for id in ["windows-x86_64", "linux-x86_64-uclibc", "Linux-x86_64", ""] {
            let error = Platform::try_from(id.to_owned()).unwrap_err();
            assert_eq!(
                error.to_string(),
                format!(
                    "unknown target `{id}`; supported targets: macos-aarch64, macos-x86_64, linux-aarch64, linux-x86_64, linux-aarch64-gnu, linux-aarch64-musl, linux-x86_64-gnu, linux-x86_64-musl"
                )
            );
        }
    }

    #[test]
    fn canonicalizes_target_lists() {
        use Platform::*;
        assert_eq!(
            canonical_targets(&[LinuxX86_64Musl, LinuxAarch64, MacosX86_64, LinuxX86_64Gnu])
                .unwrap(),
            [MacosX86_64, LinuxAarch64, LinuxX86_64Gnu, LinuxX86_64Musl],
        );
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
    fn simulated_hosts_select_the_exact_linux_abi() {
        use Platform::*;
        for (arch, gnu, musl) in [
            ("x86_64", LinuxX86_64Gnu, LinuxX86_64Musl),
            ("aarch64", LinuxAarch64Gnu, LinuxAarch64Musl),
        ] {
            let host = Host { os: "linux", arch };
            assert_eq!(
                host.target(&[gnu, musl], || Some(LinuxAbi::Gnu)).unwrap(),
                gnu
            );
            assert_eq!(
                host.target(&[gnu, musl], || Some(LinuxAbi::Musl)).unwrap(),
                musl
            );
            let error = host.target(&[gnu], || Some(LinuxAbi::Musl)).unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("this project does not support {musl}; [platforms] targets are {gnu}")
            );
            let error = host.target(&[gnu], || None).unwrap_err();
            assert!(error.to_string().contains("could not determine libc"));
            assert!(error.to_string().contains(&gnu.base_platform().to_string()));
        }
    }

    #[test]
    fn simulated_legacy_macos_and_unsupported_hosts_skip_libc_detection() {
        use Platform::*;
        for (os, arch, target) in [
            ("linux", "x86_64", LinuxX86_64),
            ("linux", "aarch64", LinuxAarch64),
            ("macos", "aarch64", MacosAarch64),
            ("macos", "x86_64", MacosX86_64),
        ] {
            let host = Host { os, arch };
            let other = if target == LinuxAarch64 {
                LinuxX86_64Musl
            } else {
                LinuxAarch64Gnu
            };
            let targets = [target, other];
            assert_eq!(
                host.target(&targets, || panic!("no libc check needed"))
                    .unwrap(),
                target
            );
        }
        let host = Host {
            os: "linux",
            arch: "riscv64",
        };
        let error = host
            .target(&[LinuxX86_64Gnu], || panic!("unsupported arch"))
            .unwrap_err();
        assert_eq!(error.to_string(), "unsupported platform: linux-riscv64");
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
        assert_eq!(
            Platform::MacosAarch64.rust_target(),
            "universal-apple-darwin"
        );
        assert_eq!(
            Platform::MacosX86_64.rust_target(),
            "universal-apple-darwin"
        );
        assert_eq!(
            Platform::LinuxAarch64.rust_target(),
            "aarch64-unknown-linux-musl"
        );
        assert_eq!(
            Platform::LinuxX86_64.rust_target(),
            "x86_64-unknown-linux-musl"
        );
    }
}
