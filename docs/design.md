# MVP design

This document records first-pass decisions. It is a contract for the initial
implementation, not a promise of future features.

## Product boundary

Binloom manages downloadable developer executables used by a repository. It
does not manage application dependencies or language runtimes.

MVP supports:

- public GitHub Releases;
- exact tool versions;
- macOS ARM64 and x86-64;
- Linux ARM64 and x86-64;
- raw executables and `.gz` files;
- SHA-256 verification;
- repository-local installation;
- Binloom bootstrapping through a committed wrapper.

Windows, semantic version ranges, private repositories, additional sources,
plugins, registry services, PATH integration, and complex archives are outside
the MVP.

## Bootstrap model

The committed `binloomw` script is the only prerequisite supplied by the
repository. It performs a deliberately small job:

1. Read the locked Binloom artifact for the current platform.
2. Download it to a temporary file when it is not cached.
3. Verify its SHA-256 checksum.
4. Atomically place it under `.tools/binloom/<version>/binloom`.
5. Forward all arguments to that binary.

The wrapper reads only its stable, generated section of `binloom.lock`. It does
not implement a general TOML parser and must never use `eval` on lockfile data.
The Binloom binary parses the complete files.

`binloom init` creates the wrapper and initial configuration in a new
repository. Once the wrapper exists, `./binloomw init` may repair or complete
the repository setup through the pinned Binloom binary.

## Manifest

`binloom.toml` is human-written and expresses intent:

```toml
manifest-version = 1

[binloom]
version = "0.1.0"

[tools.lefthook]
version = "2.1.10"
source = "github:evilmartians/lefthook"
```

Exact versions keep the first resolver simple. During `update`, Binloom matches
release assets using the tool name and case-insensitive aliases for each
normalized OS and architecture. For example, macOS may match `macos`, `darwin`,
or `MacOS`, while ARM64 may match `arm64` or `aarch64`.

Resolution must produce exactly one asset for every supported platform. Zero or
multiple matches fail with the candidate names and leave the lockfile unchanged.
Most tools need no additional configuration.

An optional asset pattern can narrow an ambiguous release:

```toml
[tools.example]
version = "1.2.3"
source = "github:owner/example"
asset = "example_{version}_{os}_{arch}.gz"
```

`{os}` and `{arch}` use Binloom's built-in alias sets, so the manifest does not
repeat platform mappings. The generated lockfile remains the source of exact
asset URLs and checksums used by `install`.

## Lockfile

`binloom.lock` is generated and committed. It records everything needed to
install without rediscovering releases:

```toml
lock-version = 1

[binloom]
version = "0.1.0"

[binloom.assets.macos-aarch64]
url = "https://github.com/example/binloom/releases/download/v0.1.0/binloom-macos-aarch64"
sha256 = "..."

[tools.lefthook]
version = "2.1.10"
source = "github:evilmartians/lefthook"

[tools.lefthook.assets.macos-aarch64]
url = "https://github.com/evilmartians/lefthook/releases/download/v2.1.10/lefthook_2.1.10_MacOS_arm64.gz"
sha256 = "..."
format = "gz"
```

The real lockfile contains a Binloom asset and each tool asset for all supported
platforms. URLs must use HTTPS. Checksums are lowercase SHA-256 hex strings.

Changing lockfile shape requires increasing `lock-version`. A generated wrapper
and its lockfile version are updated together.

## Commands

```text
binloom init
binloom install
binloom update [tool]
binloom update --self
binloom exec <tool> -- <args...>
binloom list
binloom path <tool>
```

Behavior:

- `init` creates missing project files and refuses to overwrite edited files.
- `install` requires a present, compatible lockfile and never modifies it.
- `update` selects the latest stable releases, updates the manifest versions,
  resolves assets and checksums, then replaces the lockfile. Without a tool
  name it also updates Binloom.
- `exec` installs the selected tool when missing, then executes its versioned
  binary directly.
- `list` reports locked and installed versions.
- `path` prints the exact installed executable path.

`update --self` updates only Binloom and its locked bootstrap assets.

## Installation layout

```text
.tools/
  binloom/
    0.1.0/
      binloom
  lefthook/
    2.1.10/
      lefthook
```

Versioned directories prevent a changed lockfile from reusing an incompatible
binary. `exec` resolves the direct path, so the MVP needs no `.tools/bin`
symlinks or shims.

`.tools/` is ignored by Git. Installations are disposable and recreated from
the committed lockfile.

## Install rules

An installation is successful only after:

1. Download to a temporary file in the destination filesystem.
2. Verify the complete file against the locked checksum.
3. Decompress without executing downloaded content.
4. Set executable permissions on Unix.
5. Rename the completed temporary directory into its versioned destination.

An existing executable at the exact locked path makes installation idempotent.
Partial files never become the final installation.

## Trust and security

- The committed manifest, lockfile, and wrapper are trusted repository state.
- Release metadata and downloaded bytes are untrusted remote input.
- HTTPS is required but does not replace checksum verification.
- Downloads are verified before decompression or execution.
- Archive support must reject absolute paths, parent traversal, and unsafe
  links before it is added.
- Resolution never executes downloaded content.
- Logs must not expose credentials if authenticated sources are added later.

## Language independence

Binloom operates on executable files and process arguments. It does not inspect
`Cargo.toml`, `package.json`, `pyproject.toml`, or equivalent language files.
Any repository can use the same workflow:

```sh
./binloomw install
./binloomw exec <tool> -- <arguments>
```

Language-specific conventions can call these commands, but they are not part of
Binloom's contract.
