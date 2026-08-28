use crate::common::validate_tool_name;
use crate::download::Client;
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::fmt::{self, Display, Formatter};

const CRATES_IO_API_URL: &str = "https://crates.io/api/v1";

#[derive(Deserialize)]
struct CrateResponse {
    #[serde(rename = "crate")]
    metadata: CrateMetadata,
    versions: Vec<CrateVersion>,
}

#[derive(Deserialize)]
struct CrateMetadata {
    max_version: String,
    max_stable_version: Option<String>,
}

#[derive(Deserialize)]
struct CrateVersion {
    num: String,
    checksum: String,
    created_at: String,
    yanked: bool,
}

pub(crate) struct ResolvedCargoPackage {
    pub(crate) version: String,
    pub(crate) checksum: String,
    pub(crate) published_at: String,
}
#[derive(Debug)]
pub(crate) struct CargoSource {
    package: String,
}

impl Display for CargoSource {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "cargo:{}", self.package)
    }
}

impl TryFrom<String> for CargoSource {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let package = value
            .strip_prefix("cargo:")
            .ok_or_else(|| "invalid Cargo source; expected cargo:package".to_owned())?;

        validate_tool_name(package).map_err(|_| format!("invalid Cargo package: {package}"))?;

        Ok(Self {
            package: package.to_owned(),
        })
    }
}

impl CargoSource {
    pub(crate) fn resolve(
        &self,
        client: &Client,
        version: Option<&str>,
    ) -> Result<ResolvedCargoPackage> {
        self.resolve_from(client, version, CRATES_IO_API_URL)
    }

    pub(crate) fn resolve_from(
        &self,
        client: &Client,
        version: Option<&str>,
        api_url: &str,
    ) -> Result<ResolvedCargoPackage> {
        let url = format!("{api_url}/crates/{}", self.package);

        let mut response = client
            .get(&url)
            .call()
            .with_context(|| format!("failed to fetch Cargo package {}", self.package))?;

        let response = response
            .body_mut()
            .read_json::<CrateResponse>()
            .with_context(|| format!("failed to parse Cargo package {}", self.package))?;

        let wanted = version
            .map(str::to_owned)
            .or(response.metadata.max_stable_version)
            .unwrap_or(response.metadata.max_version);

        let resolved = response
            .versions
            .into_iter()
            .find(|candidate| candidate.num == wanted)
            .with_context(|| {
                format!(
                    "version {wanted} not found for Cargo package {}",
                    self.package
                )
            })?;

        ensure!(
            !resolved.yanked,
            "version {wanted} of Cargo package {} is yanked",
            self.package
        );

        ensure!(
            resolved.checksum.len() == 64
                && resolved
                    .checksum
                    .chars()
                    .all(|character| character.is_ascii_hexdigit()),
            "invalid checksum for Cargo package {}@{wanted}",
            self.package
        );

        Ok(ResolvedCargoPackage {
            version: resolved.num,
            checksum: resolved.checksum,
            published_at: resolved.created_at,
        })
    }

    pub(crate) fn package(&self) -> &str {
        &self.package
    }

    pub(crate) fn download_url(&self, version: &str) -> String {
        format!(
            "https://crates.io/api/v1/crates/{}/{version}/download",
            self.package
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_displays_cargo_source() {
        let source = CargoSource::try_from("cargo:cargo-nextest".to_owned()).unwrap();

        assert_eq!(source.to_string(), "cargo:cargo-nextest");
    }

    #[test]
    fn rejects_invalid_cargo_sources() {
        for source in ["cargo:", "cargo:../tool", "github:owner/tool"] {
            assert!(
                CargoSource::try_from(source.to_owned()).is_err(),
                "{source}"
            );
        }
    }

    use crate::http_fixture::{Response, Server};

    #[test]
    fn resolves_latest_stable_cargo_package() {
        let server = Server::start(vec![Response {
            status: 200,
            body: format!(
                r#"{{
                "crate": {{
                    "max_version": "2.0.0-beta.1",
                    "max_stable_version": "1.2.3"
                }},
                "versions": [{{
                    "num": "1.2.3",
                    "checksum": "{}",
                    "created_at": "2026-01-01T00:00:00Z",
                    "yanked": false
                }}]
            }}"#,
                "a".repeat(64)
            )
            .into_bytes(),
        }]);

        let source = CargoSource::try_from("cargo:tool".to_owned()).unwrap();
        let resolved = source
            .resolve_from(&crate::download::client(), None, server.url())
            .unwrap();

        assert_eq!(resolved.version, "1.2.3");
        assert_eq!(resolved.checksum, "a".repeat(64));
        assert_eq!(server.requests()[0].path, "/crates/tool");
    }
}
