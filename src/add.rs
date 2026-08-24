use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use anyhow::{Context, Result, ensure};

use crate::{
    common::{validate_tool_name, validate_version},
    install,
    manifest::Manifest,
    sources::Source,
    update,
};

pub fn add(name: &str, source: &str, version: &str) -> Result<()> {
    write_tool(Path::new("binloom.toml"), name, source, version)?;

    update::lock().context("tool was added to binloom.toml, but lockfile update failed")?;

    install::install()
}

fn write_tool(path: &Path, name: &str, source: &str, version: &str) -> Result<()> {
    validate_tool_name(name)?;
    validate_version(version)?;

    let manifest = Manifest::try_from(path)?;
    ensure!(
        !manifest.tools.contains_key(name),
        "tool {name} is already configured"
    );

    let source = Source::try_from(source.to_owned()).map_err(anyhow::Error::msg)?;

    let content = fs::read_to_string(path)?;
    let mut file = OpenOptions::new().append(true).open(path)?;

    if !content.ends_with('\n') {
        writeln!(file)?;
    }

    if !content.ends_with("\n\n") {
        writeln!(file)?;
    }

    writeln!(file, "[tools.{name}]")?;
    writeln!(
        file,
        "version = {}",
        toml::Value::String(version.to_owned())
    )?;
    writeln!(file, "source = {}", toml::Value::String(source.to_string()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_tool_without_rewriting_manifest() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("binloom.toml");

        fs::write(
            &path,
            "manifest-version = 1\n\n[binloom]\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        write_tool(&path, "lefthook", "github:evilmartians/lefthook", "2.1.10").unwrap();

        let manifest = Manifest::try_from(path.as_path()).unwrap();

        assert_eq!(manifest.tools["lefthook"].version, "2.1.10");

        let before = fs::read_to_string(&path).unwrap();
        assert!(write_tool(&path, "lefthook", "github:evilmartians/lefthook", "2.1.10",).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), before);
    }
}
