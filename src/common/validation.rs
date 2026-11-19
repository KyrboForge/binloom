use anyhow::{Result, ensure};
use serde::Deserialize;
use std::path::Path;

pub(crate) fn parse_versioned_toml<'a, T: Deserialize<'a>>(
    content: &'a str,
    path: &Path,
    version_key: &str,
) -> Result<T> {
    toml::from_str(content).map_err(|error| {
        let message = error.message();
        let context =
            if message.starts_with("unknown field") || message.starts_with("unknown variant") {
                newer_binloom_hint(path)
            } else if let Some(version) = toml::from_str::<toml::Table>(content)
                .ok()
                .and_then(|table| table.get(version_key).and_then(toml::Value::as_integer))
                .filter(|&version| version != 1)
            {
                format!(
                    "unsupported {version_key}: {version}. {}",
                    newer_binloom_hint(path)
                )
            } else {
                format!("failed to parse {}", path.display())
            };
        anyhow::Error::new(error).context(context)
    })
}

pub(crate) fn newer_binloom_hint(path: &Path) -> String {
    format!(
        "{} may use fields from a newer Binloom (this is Binloom {}). Upgrade the pinned Binloom first by running `./binloomw update --self`, then retry",
        path.display(),
        env!("CARGO_PKG_VERSION")
    )
}

pub(crate) fn validate_tool_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            }),
        "invalid tool name: {name}"
    );

    Ok(())
}

pub(crate) fn validate_version(version: &str) -> Result<()> {
    ensure!(
        !version.is_empty()
            && version.trim() == version
            && version != "."
            && version != ".."
            && !version.starts_with('.')
            && !version
                .chars()
                .any(|character| matches!(character, '/' | '\\' | '\0')),
        "invalid version: {version}"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_tool_names() {
        for name in ["lefthook", "protoc-gen-go", "tool_2"] {
            validate_tool_name(name).unwrap();
        }

        for name in [
            "",
            ".hidden",
            "../tool",
            "/tmp/tool",
            r"tool\name",
            "tool.name",
            "tool name",
        ] {
            assert!(validate_tool_name(name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn validates_versions_as_safe_path_components() {
        for version in ["1.2.3", "v1.2.3", "2026-08-24", "1.0.0-beta+build"] {
            validate_version(version).unwrap();
        }

        for version in [
            "", ".", "..", ".hidden", "/tmp/x", "../../x", r"..\..\x", " 1.0.0", "1.0.0 ",
        ] {
            assert!(validate_version(version).is_err(), "{version:?}");
        }
    }
}
