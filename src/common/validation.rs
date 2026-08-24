use anyhow::{Result, ensure};

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
