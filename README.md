# Binloom

Binloom is a small, repository-local manager for downloadable developer tools.
It gives every contributor and CI job the same pinned executables without
requiring a global installation of Binloom, Lefthook, Buf, or another tool
manager.

Binloom is language-independent. A Rust, Go, Python, JavaScript, or mixed
repository uses the same manifest, lockfile, and commands.

## Why

Projects often require command-line tools that are not application
dependencies: hook runners, linters, formatters, code generators, and protocol
compilers. Asking contributors to install and align them globally creates
onboarding work and CI drift.

Binloom keeps those tools inside the repository working tree:

```text
binloom.toml
    -> resolve
binloom.lock
    -> download and verify
.tools/
```

The manifest describes intent. The committed lockfile records exact release
assets and checksums. Installation only consumes the lockfile and never silently
updates it.

## Intended workflow

The repository maintainer initializes Binloom once:

```sh
binloom init
git add binloomw binloom.toml binloom.lock
```

Contributors only clone the repository and run its committed wrapper:

```sh
./binloomw install
./binloomw exec lefthook -- run pre-commit
```

`binloomw` downloads the Binloom version pinned in `binloom.lock`, verifies its
SHA-256 checksum, caches it under `.tools/`, and forwards the command. Binloom
then installs and runs the repository's other pinned tools. Nothing managed by
Binloom needs a global installation or shell integration.

Planned explicit updates:

```sh
./binloomw update
./binloomw update lefthook
./binloomw update binloom
```

Updates modify the lockfile. Installs do not.

## Repository files

| Path | Purpose | Committed |
| --- | --- | --- |
| `binloomw` | Small bootstrap wrapper | yes |
| `binloom.toml` | Human-written tool requirements | yes |
| `binloom.lock` | Resolved URLs, versions, and checksums | yes |
| `.tools/` | Downloaded Binloom and managed executables | no |

## Principles

- Repository-local by default.
- Reproducible and checksum-verified.
- No required PATH changes or shell integration.
- No dependency on a language package manager.
- One native Binloom binary and one small committed wrapper.
- Predictable local and CI behavior.
- Explicit updates; no surprise network-driven upgrades.

## Scope

The first version targets public GitHub Releases on macOS and Linux for ARM64
and x86-64. It installs standalone executables and simple gzip-compressed
executables. Lefthook is the first real consumer.

Binloom is not a language package manager, runtime manager, environment manager,
daemon, GUI, or remote package registry.

## Status

Binloom is currently in design. See [docs/README.md](docs/README.md) for the MVP
contract and decisions.
