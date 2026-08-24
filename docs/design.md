# MVP design

This document records the implemented MVP contract, not a promise of future
features.

## Product boundary

Binloom manages downloadable developer executables used by a repository. It
does not manage application dependencies or language runtimes.

The MVP supports:

- public GitHub Releases;
- exact tool versions;
- macOS and Linux on ARM64 and x86-64;
- raw executables and single `.gz` files;
- SHA-256 verification;
- repository-local installation;
- Binloom bootstrapping through a committed POSIX shell wrapper.

Windows, private repositories, additional sources, semantic version ranges,
plugins, registry services, complex archives, and persistent shell integration
are outside the MVP.

## Bootstrap model

The committed `binloomw` script is the only project-supplied prerequisite. It:

1. Detects the current platform.
2. Reads the locked Binloom artifact from `binloom.lock`.
3. Downloads it when the version is not cached.
4. Verifies its SHA-256 checksum.
5. Decompresses and atomically installs it under
   `.tools/binloom/<version>/binloom`.
6. Forwards all arguments to that binary.

The wrapper uses only POSIX shell plus `awk`, `gzip`, one of `curl` or `wget`,
and one of `sha256sum` or `shasum`. It reads the canonical generated lockfile;
it is not a general TOML parser and never evaluates lockfile content as shell
code.

When the lockfile contains `[wrapper]`, the script verifies its own generated
version and SHA-256 checksum. If it is outdated or changed, it downloads the
locked `binloomw` release asset, verifies it, replaces itself atomically, and
restarts. Older lockfiles without this optional section remain usable.

`binloom init` creates missing `binloom.toml` and `binloomw` files, makes the
wrapper executable, and adds `.tools/` to `.gitignore`. Existing project files
are preserved.

## Manifest

`binloom.toml` is human-written and expresses intent:

```toml
#:schema https://raw.githubusercontent.com/KyrboForge/binloom/main/schemas/binloom.schema.json

manifest-version = 1

[binloom]
version = "0.2.0"

[tools.lefthook]
version = "2.1.11"
source = "github:evilmartians/lefthook"
```

The schema comment enables validation in editors that support TOML schemas.
Binloom preserves comments while changing versions.

During resolution, Binloom matches release assets using the tool name and
case-insensitive platform aliases. Resolution must produce exactly one asset
for every supported platform. Zero or multiple matches fail before replacing
the lockfile.

An optional asset pattern can narrow an ambiguous release:

```toml
[tools.example]
version = "1.2.3"
source = "github:owner/example"
asset = "example_{version}_{os}_{arch}.gz"
```

`{os}` and `{arch}` use Binloom's built-in alias sets, so users do not repeat
platform mappings in the manifest.

Updates reject releases younger than 24 hours by default:

```toml
[update]
minimum-release-age-minutes = 1440
```

The value may be set to `0` when immediate releases are explicitly desired.

## Lockfile

`binloom.lock` is generated and committed. It records the release tag and
artifact name, URL, checksum, and format for every supported platform:

```toml
lock-version = 1

[binloom]
version = "0.2.0"
source = "github:KyrboForge/binloom"
tag = "v0.2.0"

[binloom.artifacts.macos-aarch64]
asset = "binloom_macos_aarch64.gz"
url = "https://github.com/KyrboForge/binloom/releases/download/v0.2.0/binloom_macos_aarch64.gz"
sha256 = "..."
format = "gz"
checksum-source = "digest"

[wrapper]
version = "0.2.0"
url = "https://github.com/KyrboForge/binloom/releases/download/v0.2.0/binloomw"
sha256 = "..."
checksum-source = "digest"
```

Tool entries use the same `artifacts.<platform>` shape under
`[tools.<name>]`. URLs use HTTPS and checksums are lowercase SHA-256 hex.

`checksum-source` records where the checksum came from:

- `digest` — checksum published in release metadata,
- `sidecar` — checksum read from an upstream checksum asset,
- `download` — checksum computed by Binloom from the downloaded bytes,
- `unknown` — legacy lockfile created before provenance was recorded.

## Commands and mutation rules

```text
binloom init
binloom add <name> --source <source> --version <version> [--asset <pattern>]
binloom install
binloom update [tool]
binloom update --self
binloom exec <command> [args...]
binloom list
binloom path
```

- `init` creates only missing files and never overwrites project files.
- `add` appends one exact tool requirement, rebuilds the lockfile at those
  requested versions, and installs the tools.
- `install` consumes an existing lockfile. If none exists, it first resolves
  the latest releases allowed by the manifest policy, updates manifest
  versions, and creates the lockfile.
- `update <tool>` selects that tool's latest stable release and preserves the
  other locked entries.
- `update` without a name updates every tool, Binloom, and wrapper metadata.
- `update --self` updates only Binloom and wrapper metadata.
- `exec` prepends `.tools/.bin` to `PATH` for one process. It does not mutate
  the user's shell.
- `list` reports configured versions and sources.
- `path` prints the absolute `.tools/.bin` path.

`add`, an install without a lockfile, and updates write repository
configuration or lock state. Those changes are reviewed and committed like
dependency updates.

## Installation layout

```text
.tools/
  .bin/
    lefthook -> ../lefthook/2.1.11/lefthook
  binloom/
    0.2.0/
      binloom
  lefthook/
    2.1.11/
      lefthook
```

Versioned directories prevent a changed lockfile from reusing an incompatible
binary. `.tools/.bin` provides stable command names to `exec` and optional
one-command `PATH` use.

`.tools/` is ignored by Git. Installations are disposable and recreated from
the committed lockfile.

## Installation and trust rules

- The committed manifest, lockfile, and wrapper are trusted repository state.
- GitHub metadata and downloaded bytes are untrusted remote input.
- Published digests and checksum sidecars provide an upstream checksum.
- When neither exists, Binloom warns, hashes the downloaded bytes, and records
  `checksum-source = "download"`. This is trust on first use: later installs
  verify the committed checksum, but the initial lock operation trusts the
  bytes received over HTTPS.
- Downloads go to temporary files and are verified before decompression or
  execution.
- Completed executables are moved atomically into their versioned location.
- An existing executable at the exact locked path makes installation
  idempotent.
- Resolution never executes downloaded content.
- Unsupported archive formats fail instead of extracting paths or links.

## Language independence

Binloom operates only on executable files and process arguments. It does not
inspect `Cargo.toml`, `package.json`, `pyproject.toml`, or equivalent language
files. Any repository uses the same workflow:

```sh
./binloomw install
./binloomw exec <command> [arguments...]
```
