use std::{
    fs::OpenOptions,
    io::{self, Write},
    path::Path,
};

pub fn init() -> io::Result<()> {
    println!("Initializing Binloom...");

    match generate_manifest(Path::new("binloom.toml")) {
        Ok(()) => {
            println!("Created binloom.toml");
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Err(
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "binloom.toml already exists; project is already initialized",
            ),
        ),
        Err(error) => Err(error),
    }

}

fn generate_manifest(path: &Path) -> io::Result<()> {
    let manifest = format!(
        r#"manifest-version = 1

[binloom]
version = "{}"
"#,
        env!("CARGO_PKG_VERSION"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;

    file.write_all(manifest.as_bytes())
}
fn add_to_gitignore() {}
fn generate_binloomw() {}

#[cfg(test)]
mod tests {
    use std::{fs, io};
    use crate::init::generate_manifest;

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
}