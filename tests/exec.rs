use std::{fs, process::Command};

#[cfg(unix)]
#[test]
fn prepends_project_tools_to_path_and_preserves_exit_code() {
    let directory = tempfile::tempdir().unwrap();
    let nested = directory.path().join("src/nested");
    let expected_bin = fs::canonicalize(directory.path())
        .unwrap()
        .join(".tools/.bin");

    fs::create_dir_all(&nested).unwrap();
    fs::write(directory.path().join("binloom.toml"), "").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_binloom"))
        .args([
            "exec",
            "--",
            "sh",
            "-c",
            r#"if [ "${PATH%%:*}" = "$EXPECTED_BIN" ]; then exit 42; else exit 7; fi"#,
        ])
        .env("EXPECTED_BIN", &expected_bin)
        .current_dir(&nested)
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(42),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
