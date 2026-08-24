use std::process::Command;

#[cfg(unix)]
#[test]
fn prepends_local_tools_to_path_and_preserves_exit_code() {
    let directory = tempfile::tempdir().unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_binloom"))
        .args([
            "exec",
            "--",
            "sh",
            "-c",
            r#"if [ "${PATH%%:*}" = "$PWD/.tools/.bin" ]; then exit 42; else exit 7; fi"#,
        ])
        .current_dir(directory.path())
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(42),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
