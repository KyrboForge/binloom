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

## Shell activation

For one shell session, explicitly prepend this project's absolute `.tools/.bin`:

```sh
# Bash (use --shell zsh in Zsh)
eval "$(./binloomw env --shell bash)"
```

```fish
./binloomw env --shell fish | source
```

Repeated activation does not add another entry when it is already first. This
manual mode does not check trust or undo PATH changes when you leave the project;
restore your previous PATH or start a fresh shell to deactivate it. For scripts
and CI, `./binloomw exec <command>` keeps activation scoped to one process.

For automatic activation, the first successful interactive `./binloomw install`
in Bash, Zsh, or Fish asks once:

```text
Enable automatic project-local commands in your shell? [y/N]
```

Answering `y` trusts the current project and appends a marked block to `~/.bashrc`,
`${ZDOTDIR:-~}/.zshrc`, or `${XDG_CONFIG_HOME:-~/.config}/fish/config.fish`,
selected from `$SHELL`. Existing contents and permissions are preserved; an existing profile gets a sibling
`.binloom.bak` backup before atomic replacement. `HOME`, `ZDOTDIR` and `XDG_CONFIG_HOME`
are resolved like the shell does; symlinked profile files or directories below them are rejected. The block runs the executable used during setup and does nothing if it is
missing; no global binary is installed.
Keep that cached executable available, or regenerate the hook from another
available Binloom.

Open a new shell or source the updated profile to enable it. A child process
cannot change the PATH of the shell that ran `install`. Answering anything other
than `y` declines; the answer is remembered in
`$XDG_CONFIG_HOME/binloom/activation-prompt` (default
`~/.config/binloom/activation-prompt`). `install --no-prompt`,
`BINLOOM_NO_PROMPT=1`, unsupported shells, and noninteractive installs skip setup
without editing a profile or recording an answer.

You can also configure the hook explicitly by putting the output of
`binloom shell init --shell bash` (or `zsh` / `fish`) in your rc file. To try it
in the current shell, use `eval "$(./binloomw shell init --shell bash)"`, or
`./binloomw shell init --shell fish | source` in Fish. Trust projects explicitly:

```sh
./binloomw shell trust             # nearest project, including from a subdirectory
binloom shell trust /path/to/repo   # canonicalizes the root
./binloomw shell status
./binloomw shell untrust
```

The hook checks only when the working directory changes. It finds the nearest
ancestor containing `binloom.toml` and activates it only if its canonical root
appears in `$XDG_CONFIG_HOME/binloom/trusted` (default
`~/.config/binloom/trusted`). The directory is created with mode `0700`, and the
trust and answer files with `0600`. An untrusted nested project does not inherit
its parent's trust. Interactive shells show at most one trust hint per root per
session; noninteractive hooks stay quiet.

Trust lets the project's commands shadow system commands; it does not sandbox
their execution. The hook never runs repository scripts or downloads tools.
The `.tools/.bin` directory must exist and resolve inside the trusted root;
symlink escapes are rejected. On project switches or exit, only the PATH entry
added by the hook is removed, preserving other entries and user PATH changes.
Trust changes take effect on the next directory change. Activation paths must
be UTF-8 and cannot contain colons or newlines.

After installation, `.tools/.bin/binloom` points to
`../binloom/<locked-version>/binloom` when that cached binary exists. An activated
shell therefore uses the project's pinned Binloom as well as its tools. Run
`./binloomw install` after a Binloom version update to refresh this link. The
tool name `binloom` is reserved: installation rejects `[tools.binloom]` rather
than overwriting that tool with the manager.

To disable automatic activation, remove the rc block between
`# >>> binloom shell activation >>>` and `# <<< binloom shell activation <<<`,
then start a new shell. You can restore the sibling backup if needed. Remove
the `activation-prompt` file to allow the install question again, and use
`shell untrust` to revoke individual projects. Manual hooks can be removed from
the rc file in the same way. `./binloomw` and `exec` keep working without hooks.

Windows is currently unsupported; native Windows support is tracked in
[#35](https://github.com/KyrboForge/binloom/issues/35).

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
| `binloom env --shell <bash\|zsh\|fish>` | Print commands for manual PATH activation |
| `binloom shell init --shell <bash\|zsh\|fish>` | Print directory-change hooks |
| `binloom shell trust [path]` / `untrust [path]` | Grant or revoke project activation |
| `binloom shell status` | Show the current project's trust status |

Project commands find the nearest parent directory containing
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
executables, or `.tar.gz`/`.tgz` archives holding the tool binary: the file
named after the tool, or the archive's only file. Cargo sources require an existing Rust
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
