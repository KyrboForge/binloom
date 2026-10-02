use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn run_init(directory: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_binloom"))
        .arg("init")
        .current_dir(directory)
        .output()
        .unwrap()
}

#[test]
fn init_rejects_invalid_existing_manifest() {
    let directory = tempfile::tempdir().unwrap();

    fs::write(
        directory.path().join("binloom.toml"),
        "manifest-version = 1\n",
    )
    .unwrap();

    let output = run_init(directory.path());

    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("error: existing binloom.toml is invalid")
    );
    assert!(!directory.path().join("binloom.lock").exists());
    assert!(!directory.path().join("binloomw").exists());
}

#[test]
#[ignore = "requires network access to GitHub releases"]
fn init_creates_lock_for_runnable_wrapper() {
    let directory = tempfile::tempdir().unwrap();

    let output = run_init(directory.path());
    assert!(output.status.success(), "{output:?}");
    assert!(directory.path().join("binloom.lock").is_file());

    let output = Command::new("./binloomw")
        .arg("--help")
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    // Valid manifest with missing lock: init recreates the lock.
    fs::remove_file(directory.path().join("binloom.lock")).unwrap();

    let output = run_init(directory.path());
    assert!(output.status.success(), "{output:?}");
    assert!(directory.path().join("binloom.lock").is_file());
}
