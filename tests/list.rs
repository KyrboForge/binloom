use std::fs;
use std::process::Command;
#[test]
fn lists_configured_tools() {
    let directory = tempfile::tempdir().unwrap();

    fs::write(
        directory.path().join("binloom.toml"),
        r#"manifest-version = 1

[binloom]
version = "0.1.0"

[tools.lefthook]
version = "2.1.10"
source = "github:evilmartians/lefthook"
"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_binloom"))
        .arg("list")
        .current_dir(directory.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Binloom 0.1.0\nlefthook 2.1.10 (github:evilmartians/lefthook)\n"
    );
}
