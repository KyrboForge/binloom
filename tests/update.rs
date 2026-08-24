use std::{
    fs,
    process::{Command, Output},
};

fn run_update(manifest: &str, arguments: &[&str]) -> Output {
    let directory = tempfile::tempdir().unwrap();

    fs::write(directory.path().join("binloom.toml"), manifest).unwrap();

    Command::new(env!("CARGO_BIN_EXE_binloom"))
        .arg("update")
        .args(arguments)
        .current_dir(directory.path())
        .output()
        .unwrap()
}

#[test]
fn update_rejects_unknown_tool() {
    let output = run_update(
        r#"manifest-version = 1

[binloom]
version = "0.1.0"

[tools.lefthook]
version = "2.1.10"
source = "github:evilmartians/lefthook"
"#,
        &["missing"],
    );

    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "error: tool missing is not configured\n"
    );
}

#[test]
fn self_update_conflicts_with_tool_name() {
    let output = run_update(
        r#"manifest-version = 1

[binloom]
version = "0.1.0"
"#,
        &["lefthook", "--self"],
    );

    assert!(!output.status.success());

    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("cannot be used with"));
}
