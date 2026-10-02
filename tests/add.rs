use std::{fs, process::Command};

#[test]
fn add_restores_manifest_when_resolution_fails() {
    let directory = tempfile::tempdir().unwrap();
    let manifest = "manifest-version = 1\n\n[binloom]\nversion = \"0.1.0\"\n";

    fs::write(directory.path().join("binloom.toml"), manifest).unwrap();

    // Route all requests through a closed port so resolution fails offline.
    let output = Command::new(env!("CARGO_BIN_EXE_binloom"))
        .args([
            "add",
            "--source",
            "github:owner/example",
            "example",
            "1.2.3",
        ])
        .current_dir(directory.path())
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("all_proxy", "http://127.0.0.1:1")
        .env("https_proxy", "http://127.0.0.1:1")
        .env("http_proxy", "http://127.0.0.1:1")
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .output()
        .unwrap();

    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("binloom.toml was restored")
    );
    assert_eq!(
        fs::read_to_string(directory.path().join("binloom.toml")).unwrap(),
        manifest
    );
    assert!(!directory.path().join("binloom.lock").exists());
}
