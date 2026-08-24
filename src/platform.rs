use anyhow::{Result, bail};
use std::fmt::{self, Display};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Platform {
    MacosAarch64,
    MacosX86_64,
    LinuxAarch64,
    LinuxX86_64,
}

impl Platform {
    pub const ALL: [Self; 4] = [
        Self::MacosAarch64,
        Self::MacosX86_64,
        Self::LinuxAarch64,
        Self::LinuxX86_64,
    ];

    pub const fn os_aliases(self) -> &'static [&'static str] {
        match self {
            Self::MacosAarch64 | Self::MacosX86_64 => &["macos", "darwin"],
            Self::LinuxAarch64 | Self::LinuxX86_64 => &["linux"],
        }
    }

    pub const fn arch_aliases(self) -> &'static [&'static str] {
        match self {
            Self::MacosAarch64 | Self::LinuxAarch64 => &["aarch64", "arm64"],
            Self::MacosX86_64 | Self::LinuxX86_64 => &["x86_64", "amd64"],
        }
    }
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
    fn rejects_unsupported_platform() {
        let error = Platform::try_from(("windows", "x86_64")).unwrap_err();

        assert_eq!(error.to_string(), "unsupported platform: windows-x86_64");
    }
}
