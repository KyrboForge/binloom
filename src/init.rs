use anyhow::{Context, Result, bail};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
};

pub fn init() -> Result<()> {
    println!("Initializing Binloom...");

    match generate_manifest(Path::new("binloom.toml")) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            bail!("binloom.toml already exists; project is already initialized");
        }
        Err(error) => {
            return Err(error).context("failed to create binloom.toml");
        }
    }

    println!("Created binloom.toml");

    if add_to_gitignore(Path::new(".gitignore")).context("failed to update .gitignore")? {
        println!("Added .tools/ to .gitignore");
    }

    Ok(())
}

fn generate_manifest(path: &Path) -> io::Result<()> {
    let manifest = format!(
        r#"manifest-version = 1

[binloom]
version = "{}"
"#,
        env!("CARGO_PKG_VERSION")
    );
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;

    file.write_all(manifest.as_bytes())
}
fn add_to_gitignore(path: &Path) -> io::Result<bool> {
    let existing = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };

    if existing.lines().any(|line| line.trim() == ".tools/") {
        return Ok(false);
    }

    let mut file = OpenOptions::new().create(true).append(true).open(path)?;

    if !existing.is_empty() && !existing.ends_with('\n') {
        writeln!(file)?;
    }

    writeln!(file, ".tools/")?;

    Ok(true)
}

#[cfg(test)]
mod tests {
    use crate::init::{add_to_gitignore, generate_manifest};
    use std::{fs, io};

    #[test]
    fn generates_manifest_without_overwriting_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("binloom.toml");

        generate_manifest(&path).unwrap();

        let expected = format!(
            r#"manifest-version = 1

[binloom]
version = "{}"
"#,
            env!("CARGO_PKG_VERSION")
        );

        assert_eq!(fs::read_to_string(&path).unwrap(), expected);

        let error = generate_manifest(&path).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(path).unwrap(), expected);
    }

    #[test]
    fn adds_tools_to_gitignore_once_without_replacing_content() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(".gitignore");

        fs::write(&path, "target/\n.env").unwrap();

        assert!(add_to_gitignore(&path).unwrap());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "target/\n.env\n.tools/\n"
        );

        assert!(!add_to_gitignore(&path).unwrap());
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "target/\n.env\n.tools/\n"
        );
    }
}
