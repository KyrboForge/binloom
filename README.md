<p align="center">
  <img src="docs/assets/binloom-logo.png" alt="Binloom logo" width="160">
</p>

<h1 align="center">🧶 Binloom</h1>

<p align="center">
  <strong>Pinned, checksum-verified developer tools that live with your repository.</strong>
</p>

<p align="center">
  <a href="https://github.com/KyrboForge/binloom/actions/workflows/ci.yml"><img src="https://github.com/KyrboForge/binloom/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/KyrboForge/binloom/releases/latest"><img src="https://img.shields.io/github/v/release/KyrboForge/binloom" alt="Release"></a>
  <a href="https://crates.io/crates/binloom"><img src="https://img.shields.io/crates/v/binloom.svg" alt="crates.io"></a>
  <a href="LICENSE-MIT"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg" alt="License"></a>
</p>

Binloom is a small, repository-local manager for developer tools distributed
as release binaries or crates.io packages. It gives every contributor and CI
job the same pinned executables without a global Binloom installation.

It works the same way in Rust, Go, Python, JavaScript, Java, and mixed
repositories.

**No global installs. No language lock-in. No version drift.**

## 📦 Installation

### Bootstrap without a global install

The recommended bootstrap downloads a temporary, checksum-verified Binloom
binary and runs the requested command without installing anything globally:

```sh
curl -fsSL https://raw.githubusercontent.com/KyrboForge/binloom/main/bootstrap.sh \
  -o /tmp/binloom-bootstrap
sh /tmp/binloom-bootstrap init
rm /tmp/binloom-bootstrap
```

### Optional global install

If you want `binloom` available everywhere, choose one of these:

#### crates.io

```sh
cargo install binloom
```

#### Git repository

```sh
cargo install --git https://github.com/KyrboForge/binloom
```

🚧 Homebrew support through `KyrboForge/tap` is in progress.

Global Binloom is convenient for commands such as `binloom init`. The
committed `binloomw` still uses the version pinned by the repository so every
contributor and CI job runs the same binary. Contributors therefore only need
`./binloomw`, not a global installation.

## ✨ Why Binloom?

Projects often need command-line tools that are not application dependencies:
hook runners, linters, formatters, code generators, and protocol compilers.
Installing and aligning them globally creates onboarding work and CI drift.

Binloom keeps the whole flow reproducible and local:

```mermaid
flowchart LR
    manifest["binloom.toml<br/>What you want"]
    lock["binloom.lock<br/>Exact inputs + SHA-256"]
    wrapper["./binloomw<br/>Verified bootstrap"]
    tools[".tools/<br/>Local executables"]

    manifest --> lock --> wrapper --> tools
```

The manifest describes intent. The committed lockfile records exact versions
and checksums. Release sources also record artifact URLs, formats, and checksum
provenance. Downloads are verified before installation.

## 🚀 Quick start

The maintainer initializes the repository using any available Binloom binary:

```sh
binloom init
binloom add lefthook \
  --source github:evilmartians/lefthook \
  --version 2.1.11
binloom update --self

git add binloomw binloom.toml binloom.lock .gitignore
```

Contributors only need the committed files:

```sh
./binloomw install
./binloomw exec lefthook -- run pre-commit
```

`binloomw` downloads the Binloom version pinned for the current platform,
verifies its SHA-256 checksum, caches it under
`.tools/binloom/<version>/binloom`, and forwards the command. No global
installation or shell setup is required.

Managed tools are installed under `.tools/<tool>/<version>/` and linked into
`.tools/.bin`. Use them through `binloomw exec`, or add the printed path for a
single shell command:

```sh
PATH="$(./binloomw path):$PATH" lefthook version
```

## ⚙️ Configuration

`binloom.toml` stays intentionally small:

```toml
#:schema https://raw.githubusercontent.com/KyrboForge/binloom/main/schemas/binloom.schema.json

manifest-version = 1

[binloom]
version = "0.2.0"

[tools.lefthook]
version = "2.1.11"
source = "github:evilmartians/lefthook"
```

Supported source formats are:

- `github:owner/repository`
- `gitlab:group[/subgroup]/project`
- `cargo:package`

GitLab projects may use nested groups. `GITLAB_TOKEN` optionally authenticates
GitLab API requests through the `PRIVATE-TOKEN` header. Public repositories are
supported; private asset downloads are not.

GitHub and GitLab sources use prebuilt release assets. When automatic matching
is ambiguous, an optional pattern can select one asset for each platform:

```toml
[tools.example]
version = "1.2.3"
source = "github:owner/example"
asset = "example_{version}_{os}_{arch}.gz"
```

Binloom tries `v{version}`, `{version}`, and `{tool}-{version}` release tags.
`{target}` expands to Binloom's portable target for each platform. For example,
cargo-nextest can use its prebuilt releases without requiring Rust:

```toml
[tools.cargo-nextest]
version = "0.9.143"
source = "github:nextest-rs/nextest"
asset = "cargo-nextest-{version}-{target}.tar.gz"
```

```sh
./binloomw install
./binloomw exec cargo nextest run
```

Cargo sources download a versioned `.crate` archive from crates.io, verify its
SHA-256 checksum, and compile it locally with `cargo install --locked`. They
require both `cargo` and `rustc`, and the package must install a binary matching
the configured tool name:

```toml
[tools.cargo-nextest]
version = "0.9.143"
source = "cargo:cargo-nextest"
```

Prefer GitHub or GitLab when suitable prebuilt binaries are available. The
`asset` field is valid only for those release sources.

Updates ignore releases younger than 24 hours by default. Repositories can
change that safety window:

```toml
[update]
minimum-release-age-minutes = 1440
```

## 🧰 Commands

| Command | Purpose |
| --- | --- |
| `binloom init` | Create missing manifest, lock and wrapper files; add `.tools/` to `.gitignore` |
| `binloom add <name> --source <source> --version <version> [--asset <pattern>]` | Append a tool, refresh the lockfile, and install it |
| `binloom install` | Install every locked tool; reconstruct the lockfile from manifest pins when it is missing |
| `binloom update [tool]` | Update one tool, or all tools and Binloom when omitted |
| `binloom update --self` | Update only Binloom and its wrapper metadata |
| `binloom exec <command> [args...]` | Run with `.tools/.bin` prepended to `PATH` |
| `binloom list` | List configured tools and versions |
| `binloom path` | Print the absolute `.tools/.bin` path |

Except for `init`, commands find the nearest parent directory containing
`binloom.toml`, so they also work from project subdirectories.

Updates modify `binloom.toml` and `binloom.lock`. Commit both so contributors
and CI receive the same toolchain.

## 📁 Repository files

| Path | Purpose | Commit? |
| --- | --- | --- |
| `binloomw` | Generated POSIX bootstrap wrapper | yes |
| `binloom.toml` | Human-written requirements | yes |
| `binloom.lock` | Resolved versions, inputs, checksums, and provenance | yes |
| `.tools/` | Downloaded binaries and links | no |

When `[wrapper]` metadata is present in the lockfile, `binloomw` also verifies
its generated version and checksum. A changed or outdated wrapper replaces
itself atomically from the locked release asset and restarts.

## 🎯 Scope

Binloom currently supports public GitHub and GitLab Releases plus public
crates.io packages. Release assets may be raw executables, gzip-compressed
executables, or `.tar.gz` archives containing exactly one regular file whose
name matches the configured tool name. Cargo sources require an existing Rust
toolchain and compile the verified crate locally.

It is not a language package manager, runtime manager, daemon, GUI, or remote
package registry. See the [MVP design](docs/design.md) for the detailed
contract.

## 🧪 Development

Run the test suite and measure line coverage locally with:

```sh
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov
rustup run stable cargo llvm-cov --all-targets --locked --summary-only
```

CI rejects changes that lower line coverage below 60% and uploads the report
to GitHub Code Quality.

## 📜 License

Licensed under either [MIT](LICENSE-MIT) or
[Apache License 2.0](LICENSE-APACHE), at your option.
