use std::path::Path;

use crate::{
    common::{LOCKFILE, MANIFEST, project_root},
    domain::{
        lockfile::{LockedPlatforms, LockedTool, LockedWrapper, Lockfile},
        manifest::{self, Manifest, Tool},
        resolve::{resolve_binloom, resolve_tool},
        sources::Source,
    },
    download,
};
use anyhow::{Context, Result, ensure};

pub(crate) fn update(tool_name: Option<&str>) -> Result<()> {
    let root = project_root()?;

    update_tools(&root, tool_name)
}

pub(crate) fn lock() -> Result<()> {
    let root = project_root()?;

    let lock_path = root.join(LOCKFILE);
    let manifest = Manifest::try_from(root.join(MANIFEST).as_path())?;
    let lockfile = resolve_project(&manifest, None, false, None, resolve_binloom)?;
    lockfile.write_new(&lock_path)?;
    println!("Updated {}", lock_path.display());

    Ok(())
}

pub(crate) fn lock_added_tool(
    root: &Path,
    manifest: &Manifest,
    tool_name: &str,
) -> Result<Lockfile> {
    let lock_path = root.join(LOCKFILE);

    let existing = if lock_path
        .try_exists()
        .context("failed to check binloom.lock")?
    {
        Some(Lockfile::try_from(lock_path.as_path())?)
    } else {
        None
    };

    let target = lock_target(existing.as_ref(), tool_name);

    resolve_project(manifest, target, false, existing, resolve_binloom)
}

fn update_tools(root: &Path, tool_name: Option<&str>) -> Result<()> {
    let manifest_path = root.join(MANIFEST);
    let lock_path = root.join(LOCKFILE);
    let manifest = Manifest::try_from(manifest_path.as_path())?;
    let existing = tool_name
        .map(|name| {
            ensure!(
                manifest.tools.contains_key(name),
                "tool {name} is not configured"
            );
            Lockfile::try_from(lock_path.as_path())
                .context("failed to load binloom.lock; run `binloom update` first")
        })
        .transpose()?;
    let lockfile = resolve_project(&manifest, tool_name, true, existing, resolve_binloom)?;

    let binloom_version = lockfile
        .binloom
        .as_ref()
        .filter(|_| tool_name.is_none())
        .map(|locked| locked.version.as_str());
    let versions = lockfile
        .tools
        .iter()
        .filter(|(name, _)| tool_name.is_none_or(|selected| selected == name.as_str()))
        .map(|(name, locked)| (name.as_str(), locked.version.as_str()));
    let updated_manifest = manifest::render_versions(&manifest_path, binloom_version, versions)?;
    lockfile.write_with_manifest(root, &updated_manifest)?;
    println!("Updated {}", lock_path.display());

    Ok(())
}

fn resolve_project(
    manifest: &Manifest,
    tool_name: Option<&str>,
    latest: bool,
    existing: Option<Lockfile>,
    resolve_bootstrap: impl FnOnce(
        &Source,
        Option<&str>,
        u64,
        &download::Client,
    ) -> Result<(LockedTool, LockedWrapper)>,
) -> Result<Lockfile> {
    let minimum_age = manifest.update.minimum_release_age_minutes;

    let (selected, mut lockfile): (Vec<(&str, &Tool)>, Lockfile) = match tool_name {
        Some(name) => {
            let tool = manifest
                .tools
                .get(name)
                .with_context(|| format!("tool {name} is not configured"))?;

            let lockfile = existing.context("missing binloom.lock; run `binloom update` first")?;
            ensure!(
                manifest.project_targets()
                    == lockfile
                        .platforms
                        .as_ref()
                        .map(|platforms| platforms.targets.as_slice()),
                "[platforms] targets changed; run `binloom update` to regenerate binloom.lock"
            );

            (vec![(name, tool)], lockfile)
        }
        None => {
            let selected = manifest
                .tools
                .iter()
                .map(|(name, tool)| (name.as_str(), tool))
                .collect();

            (selected, Lockfile::default())
        }
    };

    let client = download::client();

    for (name, tool) in selected {
        let version = if latest {
            None
        } else {
            Some(tool.version.as_str())
        };

        let targets = manifest.tool_targets(tool);
        let mut locked = resolve_tool(name, tool, targets, version, minimum_age, &client)?;
        // Explicit projects snapshot the resolver inputs so drift is detectable.
        if manifest.platforms.is_some() {
            locked.targets = Some(targets.to_vec());
            locked.asset_pattern.clone_from(&tool.asset);
        }

        lockfile.tools.insert(name.to_owned(), locked);
    }

    if tool_name.is_none() {
        let source = binloom_source();

        let version = if latest {
            None
        } else {
            Some(manifest.binloom.version.as_str())
        };

        let minimum_age = binloom_minimum_age(version, minimum_age);
        let (locked, wrapper) = resolve_bootstrap(&source, version, minimum_age, &client)?;

        lockfile.binloom = Some(locked);
        lockfile.wrapper = Some(wrapper);
    }

    lockfile.platforms = manifest.project_targets().map(|targets| LockedPlatforms {
        targets: targets.to_vec(),
    });

    Ok(lockfile)
}

pub(crate) fn update_binloom() -> Result<()> {
    let root = project_root()?;
    update_binloom_at(&root, |minimum_age| {
        resolve_binloom(&binloom_source(), None, minimum_age, &download::client())
    })
}

fn update_binloom_at(
    root: &Path,
    resolve: impl FnOnce(u64) -> Result<(LockedTool, LockedWrapper)>,
) -> Result<()> {
    let manifest = Manifest::try_from(root.join(MANIFEST).as_path())?;
    let (locked, wrapper) = resolve(manifest.update.minimum_release_age_minutes)?;
    apply_binloom_update(root, locked, wrapper)
}

fn apply_binloom_update(root: &Path, locked: LockedTool, wrapper: LockedWrapper) -> Result<()> {
    let manifest_path = root.join(MANIFEST);
    let lock_path = root.join(LOCKFILE);
    let updated_manifest = manifest::render_versions(&manifest_path, Some(&locked.version), [])?;
    Lockfile::write_bootstrap_with_manifest(root, &updated_manifest, locked, wrapper)?;

    println!("Updated Binloom in {}", lock_path.display());

    Ok(())
}

fn lock_target<'a>(existing: Option<&Lockfile>, tool_name: &'a str) -> Option<&'a str> {
    existing
        .is_some_and(|lockfile| lockfile.binloom.is_some() && lockfile.wrapper.is_some())
        .then_some(tool_name)
}

fn binloom_minimum_age(version: Option<&str>, configured: u64) -> u64 {
    // Locking the exact version already running needs no cooling-off period.
    if version == Some(env!("CARGO_PKG_VERSION")) {
        0
    } else {
        configured
    }
}

fn binloom_source() -> Source {
    Source::try_from("github:KyrboForge/binloom".to_owned())
        .expect("hardcoded Binloom source must be valid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::{
            lockfile::{ArtifactFormat, ChecksumSource},
            platform::Platform,
            resolve::resolve_binloom_release,
        },
        http_fixture::{Response, Server},
    };
    use std::fs;

    const BOOTSTRAP_ASSETS: &[&str] = &[
        "binloom-macos-aarch64.gz",
        "binloom-macos-x86_64",
        "binloom-linux-aarch64.gz",
        "binloom-linux-x86_64",
        "binloomw",
    ];

    fn github_release(
        assets: &[&str],
        published_at: &str,
        checksum_server: Option<&Server>,
    ) -> Server {
        let checksum = "a".repeat(64);
        let mut assets: Vec<_> = assets
            .iter()
            .map(|name| {
                if let Some(server) = checksum_server.filter(|_| *name == "binloomw") {
                    return format!(r#"{{"name":"{name}","browser_download_url":"{}/{name}"}}"#, server.url());
                }
                format!(r#"{{"name":"{name}","browser_download_url":"https://example.invalid/{name}","digest":"sha256:{checksum}"}}"#)
            })
            .collect();
        if let Some(server) = checksum_server {
            assets.push(format!(
                r#"{{"name":"binloomw.sha256","browser_download_url":"{}/binloomw.sha256"}}"#,
                server.url(),
            ));
        }
        Server::start(vec![Response {
            status: 200,
            body: format!(
                r#"{{"tag_name":"v1.1.0","published_at":"{published_at}","assets":[{}]}}"#,
                assets.join(",")
            )
            .into_bytes(),
        }])
    }

    fn resolve_fixture(
        server: &Server,
        source: &Source,
        minimum_age: u64,
        client: &download::Client,
    ) -> Result<(LockedTool, LockedWrapper)> {
        let Source::GitHub(github) = source else {
            panic!("Binloom release must use GitHub");
        };
        let release = github.fetch_latest_release_from(client, server.url(), None)?;
        resolve_binloom_release(source, &release, minimum_age, client)
    }

    fn assert_bootstrap(lockfile: Lockfile) {
        let binloom = lockfile.binloom.unwrap();
        assert_eq!(binloom.version, "1.1.0");
        assert_eq!(binloom.artifacts.len(), 4);
        for key in ["macos-aarch64", "linux-aarch64"] {
            assert!(matches!(binloom.artifacts[key].format, ArtifactFormat::Gz));
        }
        for key in ["macos-x86_64", "linux-x86_64"] {
            assert!(matches!(binloom.artifacts[key].format, ArtifactFormat::Raw));
        }
        let wrapper = lockfile.wrapper.unwrap();
        assert_eq!(wrapper.version, "1.1.0");
        assert_eq!(wrapper.url, "https://example.invalid/binloomw");
        assert_eq!(wrapper.sha256, "a".repeat(64));
    }

    #[test]
    fn self_update_resolution_failures_leave_both_files_byte_identical() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let manifest = "manifest-version = 1\r\n[binloom]\r\nversion = '1.0.0' # pin\r\n";
        let lock = "lock-version = 1 # existing lock\n";
        fs::write(root.join(MANIFEST), manifest).unwrap();
        fs::write(root.join(LOCKFILE), lock).unwrap();

        for omitted in BOOTSTRAP_ASSETS {
            let assets: Vec<_> = BOOTSTRAP_ASSETS
                .iter()
                .copied()
                .filter(|asset| asset != omitted)
                .collect();
            let server = github_release(&assets, "2020-01-01T00:00:00Z", None);
            let error = update_binloom_at(root, |age| {
                resolve_fixture(&server, &binloom_source(), age, &download::client())
            })
            .unwrap_err();
            let expected = omitted
                .strip_prefix("binloom-")
                .unwrap_or(omitted)
                .trim_end_matches(".gz");
            assert!(format!("{error:#}").contains(expected), "{error:#}");
            assert_eq!(fs::read(root.join(MANIFEST)).unwrap(), manifest.as_bytes());
            assert_eq!(fs::read(root.join(LOCKFILE)).unwrap(), lock.as_bytes());
        }

        let sidecar = Server::start(vec![
            Response {
                status: 200,
                body: b"invalid digest\n".to_vec(),
            },
            Response {
                status: 200,
                body: b"wrapper fallback\n".to_vec(),
            },
        ]);
        for (date, checksum_server, expected) in [
            ("2099-01-01T00:00:00Z", None, "too new"),
            (
                "2020-01-01T00:00:00Z",
                Some(&sidecar),
                "invalid SHA-256 sidecar",
            ),
        ] {
            let server = github_release(BOOTSTRAP_ASSETS, date, checksum_server);
            let error = update_binloom_at(root, |age| {
                resolve_fixture(&server, &binloom_source(), age, &download::client())
            })
            .unwrap_err();
            assert!(format!("{error:#}").contains(expected), "{error:#}");
            assert_eq!(fs::read(root.join(MANIFEST)).unwrap(), manifest.as_bytes());
            assert_eq!(fs::read(root.join(LOCKFILE)).unwrap(), lock.as_bytes());
        }
        assert_eq!(sidecar.requests().len(), 1);
        assert_eq!(sidecar.requests()[0].path, "/binloomw.sha256");
    }

    #[test]
    fn self_update_preserves_legacy_and_explicit_tool_sections_byte_for_byte() {
        for targets in [
            &["linux-x86_64"][..],
            &["linux-x86_64-musl", "linux-x86_64-gnu"][..],
        ] {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path();
            let explicit = targets.len() > 1;
            let platforms = if explicit {
                format!("[platforms] # reversed on purpose\r\ntargets = {targets:?}\r\n\r\n")
            } else {
                String::new()
            };
            let tool = "[tools.demo]\r\nversion  =  '2.0.0' # tool pin\r\nsource = 'github:owner/demo'\r\n";
            let manifest = format!(
                "# pin file\r\nmanifest-version = 1\r\n[binloom]\r\nversion  =  '1.0.0' # bootstrap pin\r\n\r\n{platforms}{tool}# trailing comment"
            );
            let mut locked_tool = format!("{tool}tag='v2.0.0'\r\n");
            if explicit {
                locked_tool.push_str(&format!("targets  = {targets:?} # tool targets\r\n"));
            }
            for target in targets {
                use std::fmt::Write;
                write!(locked_tool,
                    "\r\n[tools.demo.artifacts.{target}]\r\nurl='https://example.invalid/demo'\r\nasset='demo'\r\nsha256='{}'\r\nformat='raw'\r\n",
                    "b".repeat(64)
                ).unwrap();
            }
            let lock = format!(
                "# lock v1\r\nlock-version  =  1\r\n\r\n[binloom]\nversion='1.0.0'\nsource='github:KyrboForge/binloom'\ntag='v1.0.0'\n\n{platforms}{locked_tool}\n[wrapper]\nversion='1.0.0'\nurl='https://example.invalid/old-wrapper'\nsha256='{}'\n",
                "c".repeat(64)
            );
            fs::write(root.join(MANIFEST), &manifest).unwrap();
            fs::write(root.join(LOCKFILE), &lock).unwrap();
            let server = github_release(BOOTSTRAP_ASSETS, "2020-01-01T00:00:00Z", None);

            update_binloom_at(root, |age| {
                resolve_fixture(&server, &binloom_source(), age, &download::client())
            })
            .unwrap();

            assert_eq!(
                fs::read_to_string(root.join(MANIFEST)).unwrap(),
                manifest.replace("'1.0.0'", "\"1.1.0\"")
            );
            let updated = fs::read_to_string(root.join(LOCKFILE)).unwrap();
            assert!(
                updated.starts_with("# lock v1\r\nlock-version  =  1\r\n\r\n"),
                "{updated:?}"
            );
            assert!(
                updated.contains(&format!("{platforms}{locked_tool}")),
                "{updated:?}"
            );
            let locked = Lockfile::try_from(root.join(LOCKFILE).as_path()).unwrap();
            assert_eq!(locked.tools["demo"].artifacts.len(), targets.len());
            for target in targets {
                assert!(locked.tools["demo"].artifacts.contains_key(*target));
            }
            assert_bootstrap(locked);
        }
    }

    #[test]
    fn explicit_project_update_resolves_four_legacy_bootstrap_keys_and_wrapper() {
        let manifest = Manifest::parse(
            "manifest-version = 1\n[binloom]\nversion = '1.0.0'\n[platforms]\ntargets = ['linux-x86_64-musl', 'linux-x86_64-gnu']\n",
            Path::new(MANIFEST),
        ).unwrap();
        let server = github_release(BOOTSTRAP_ASSETS, "2020-01-01T00:00:00Z", None);
        let lock = resolve_project(
            &manifest,
            None,
            true,
            None,
            |source, version, age, client| {
                assert_eq!(version, None);
                resolve_fixture(&server, source, age, client)
            },
        )
        .unwrap();
        assert_eq!(
            lock.platforms.as_ref().unwrap().targets,
            [Platform::LinuxX86_64Gnu, Platform::LinuxX86_64Musl]
        );
        assert_bootstrap(lock);
    }

    #[test]
    fn exempts_only_the_running_binloom_pin_from_minimum_age() {
        let minimum_age = 24 * 60;

        assert_eq!(
            binloom_minimum_age(Some(env!("CARGO_PKG_VERSION")), minimum_age),
            0
        );
        assert_eq!(binloom_minimum_age(None, minimum_age), minimum_age);
        assert_eq!(
            binloom_minimum_age(Some("999.0.0"), minimum_age),
            minimum_age
        );
    }

    #[test]
    fn selects_single_tool_only_for_complete_lockfile() {
        let existing = Lockfile {
            wrapper: Some(LockedWrapper {
                version: "0.1.1".to_owned(),
                url: "https://example.com/binloomw".to_owned(),
                sha256: "a".repeat(64),
                checksum_source: ChecksumSource::Digest,
            }),
            binloom: Some(LockedTool {
                version: "0.1.0".to_owned(),
                source: "github:KyrboForge/binloom".to_owned(),
                tag: Some("v0.1.0".to_owned()),
                ..LockedTool::default()
            }),
            ..Lockfile::default()
        };

        assert_eq!(lock_target(Some(&existing), "lefthook"), Some("lefthook"));
        assert_eq!(lock_target(None, "lefthook"), None);
        assert_eq!(lock_target(Some(&Lockfile::default()), "lefthook"), None);
    }

    #[test]
    fn single_tool_update_rejects_changed_project_targets_before_writing() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = "manifest-version = 1\n[binloom]\nversion = \"0.1.0\"\n\
            [platforms]\ntargets = [\"linux-x86_64\"]\n\
            [tools.tool]\nversion = \"1.0.0\"\nsource = \"github:owner/tool\"\n";
        let lock = "lock-version = 1\n";
        std::fs::write(directory.path().join(MANIFEST), manifest).unwrap();
        std::fs::write(directory.path().join(LOCKFILE), lock).unwrap();

        let error = update_tools(directory.path(), Some("tool")).unwrap_err();

        assert!(error.to_string().contains("[platforms] targets changed"));
        assert_eq!(
            std::fs::read_to_string(directory.path().join(MANIFEST)).unwrap(),
            manifest
        );
        assert_eq!(
            std::fs::read_to_string(directory.path().join(LOCKFILE)).unwrap(),
            lock
        );
    }
}
