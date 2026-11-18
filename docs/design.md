# MVP design

This document records the implemented MVP contract, not a promise of future
features.

## Product boundary

Binloom manages pinned developer executables used by a repository. Tools may
come from prebuilt release assets or be compiled from a verified crates.io
package. Binloom does not manage application dependencies or language runtimes.

The MVP supports:

- public GitHub and GitLab Releases;
- public crates.io packages built by an existing Cargo toolchain;
- exact tool versions;
- macOS and Linux on ARM64 and x86-64;
- raw executables, single `.gz` files, and `.tar.gz`/`.tgz` archives holding
  the tool binary;
- SHA-256 verification;
- repository-local installation;
- Binloom bootstrapping through a committed POSIX shell wrapper.

Windows, private repositories, custom Cargo registries, semantic version
ranges, plugins, registry services, other archive formats, and persistent shell
integration are outside the MVP.

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

`binloom init` creates missing `binloom.toml`, `binloom.lock` and `binloomw`
files, makes the wrapper executable, and adds `.tools/` to `.gitignore`.
Existing project files are preserved; an existing `binloom.toml` must be valid.
The lock is resolved before the wrapper is written, so the wrapper can run
immediately.

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

Tool sources use `github:owner/repository`,
`gitlab:group[/subgroup]/project`, or `cargo:package`. GitLab project paths may
contain nested groups:

```toml
[tools.example]
version = "1.0.0"
source = "gitlab:group/subgroup/project"
```

`GITLAB_TOKEN` optionally authenticates GitLab API requests through the
`PRIVATE-TOKEN` header. Public repositories are supported; private asset
downloads remain outside the MVP.

Cargo sources resolve package metadata from crates.io, download and verify the
locked `.crate` checksum, and compile the package with the existing `cargo` and
`rustc` commands. The package must install a binary matching the configured tool
name:

```toml
[tools.cargo-nextest]
version = "0.9.143"
source = "cargo:cargo-nextest"
```

During automatic resolution, Binloom matches release assets using the tool
name and case-insensitive platform aliases, ignoring checksum files and system
packages (`.deb`, `.rpm`, `.apk`, `.pkg`, `.dmg`, `.msi`). It may prefer gzip
assets, canonical platform aliases, and plain names over variants such as
`tool-fips`; whenever candidates are discarded, Binloom emits
a warning naming them. Zero or multiple candidates remaining after these
preferences fail before replacing the lockfile.

An explicit `asset` pattern bypasses automatic matching and must resolve to
exactly one asset for every target of the tool.

The `asset` field is valid only for GitHub and GitLab release sources.

An optional asset pattern can narrow an ambiguous release:

```toml
[tools.example]
version = "1.2.3"
source = "github:owner/example"
asset = "example_{version}_{os}_{arch}.gz"
```

`{os}` and `{arch}` use Binloom's built-in alias sets. `{target}` expands to a
portable release target such as `x86_64-unknown-linux-musl` or
`universal-apple-darwin`.

Binloom tries `v{version}`, `{version}`, and `{tool}-{version}` release tags:

```toml
[tools.cargo-nextest]
version = "0.9.143"
source = "github:nextest-rs/nextest"
asset = "cargo-nextest-{version}-{target}.tar.gz"
```

Updates reject releases younger than 24 hours by default:

```toml
[update]
minimum-release-age-minutes = 1440
```

The value may be set to `0` when immediate releases are explicitly desired.

### Platform targets

A tool inherits `[platforms].targets` unless it declares a nonempty subset
in `targets`. Tool-level targets require `[platforms]`. See the
[manifest examples](../README.md#platform-targets) and
[ADR-0001](adr/0001-explicit-project-and-tool-targets.md).

The lockfile puts the original four targets first in canonical order
(`macos-aarch64`, `macos-x86_64`, `linux-aarch64`, `linux-x86_64`), followed by
`linux-aarch64-gnu`, `linux-aarch64-musl`, `linux-x86_64-gnu`, and
`linux-x86_64-musl`. Without `[platforms]`, the original four remain the
default. The legacy Linux IDs still match OS and architecture alone and
expand `{target}` to the historical musl triples. Binloom never reinterprets
them as ABI-specific IDs. A target list cannot combine a legacy Linux ID
with an explicit ABI for the same architecture. Windows support is tracked
in [#35](https://github.com/KyrboForge/binloom/issues/35).

Automatic selection for explicit Linux targets requires a standalone
`gnu` or `glibc` token for GNU, or a `musl` token for musl, in the platform
portion of the asset name. An explicit `asset` pattern asserts compatibility
and can match a filename without an ABI token. Both paths reject an
opposite or mixed ABI label. The `{target}`
placeholder expands to a Rust triple such as `x86_64-unknown-linux-gnu` or
`x86_64-unknown-linux-musl`. The lock uses Binloom IDs as keys. Binloom
never falls back between GNU and musl or silently drops a required target.
Missing-asset diagnostics name the target and why candidates were rejected.
Each Linux target requires a distinct asset. The sharing exception is a
macOS asset named `universal` with neither architecture label.

### Linux target detection

Binloom matches the running OS and architecture first. For an explicit
target on that Linux architecture, it reads ELF loader paths (`PT_INTERP`)
from `/proc/self/exe`, `/bin/sh`, and `/usr/bin/env`. Static executables have
no interpreter, so other binaries can supply the evidence. At least one
recognized loader is required and all loaders found must agree. No usable
evidence, conflicting evidence, or an unrecognized loader produces a
`could not determine libc` error. Binloom uses neither distro names nor
`/etc/*release` files. It has no default ABI or override flag.

Custom loader names, some NixOS-style loader paths, emulation such as QEMU,
and minimal or distroless containers without standard system binaries can
leave libc undetermined. ABI matching identifies the libc family only and
cannot guarantee a minimum glibc version, the presence of required shared
libraries, or compatibility with every distribution. A `musl` suffix does
not guarantee static linking. A `gnu` suffix declares no minimum glibc
version. See [ADR-0002](adr/0002-linux-gnu-and-musl-targets.md).

## Lockfile

`binloom.lock` is generated and committed. Release entries record the tag and
the artifact name, URL, checksum, and format for every supported platform:

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

For a project with `[platforms]`, the lockfile also records the resolver
inputs so that `install` and `exec` can detect a stale lock: a top-level
`[platforms] targets` list and, per tool, its effective `targets` plus an
`asset-pattern` snapshot of the manifest's `asset` when set. Release tools
lock exactly one artifact per target. Binloom's own artifacts still cover
the four legacy platforms.

```toml
[platforms]
targets = [
    "macos-aarch64",
    "linux-x86_64-gnu",
    "linux-x86_64-musl",
]

[tools.example]
version = "1.2.3"
source = "github:owner/example"
tag = "v1.2.3"
targets = ["linux-x86_64-gnu"]
asset-pattern = "example_{version}_{target}.gz"

[tools.example.artifacts.linux-x86_64-gnu]
asset = "example_1.2.3_x86_64-unknown-linux-gnu.gz"
url = "..."
sha256 = "..."
format = "gz"
```

Cargo entries instead record one checksum for the platform-independent source
archive:

```toml
[tools.cargo-nextest]
version = "0.9.143"
source = "cargo:cargo-nextest"
sha256 = "..."
```

`checksum-source` records where the checksum came from:

- `digest` — checksum published in release metadata,
- `sidecar` — checksum read from an upstream checksum asset,
- `download` — checksum computed by Binloom from the downloaded bytes,
- `unknown` — legacy lockfile created before provenance was recorded.

When neither release metadata nor a checksum sidecar provides a checksum,
locking downloads and hashes the artifact for every supported platform. This
can be bandwidth-heavy: complete artifacts may be downloaded for every target, and a
following install downloads the current platform artifact again. Upstreams
that publish SHA-256 digests or checksum files avoid these locking downloads.

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
- `install` consumes an existing lockfile. A missing lockfile directs the
  user to `binloom update`.
- `update <tool>` selects that tool's latest stable release and preserves the
  other locked entries.
- `update` without a name updates every tool, Binloom, and wrapper metadata.
- `update --self` updates only Binloom and wrapper metadata.
- `exec` prepends `.tools/.bin` to `PATH` for one process. It does not mutate
  the user's shell.
- With `[platforms]`, `install` and declared-tool `exec` fail before
  installation changes on a host outside the project targets. On a
  supported host, `install` skips tools excluding the detected target and
  `exec` rejects them with a diagnostic naming the target. An unavailable
  declared tool never falls through to a same-named command on `PATH`.
- Both file versions remain at 1. Changes to targets or asset patterns
  require regenerating the lock with `binloom update`. Neither `install`
  nor `exec` rewrites an existing lock. There is no migration command.
  Older Binloom executables may reject explicit ABI IDs. Bootstrap and the
  pinned Binloom and wrapper upgrade are tracked in
  [#38](https://github.com/KyrboForge/binloom/issues/38).
- `update` resolves every tool for its full target set before replacing any
  file; lockfile and manifest writes are atomic. `update <tool>` refuses to run
  when `[platforms] targets` changed, because the whole lock must be
  regenerated.
- `list` reports configured versions and sources.
- `path` prints the absolute `.tools/.bin` path.

Except for `init`, commands find the nearest ancestor containing
`binloom.toml` and resolve the lockfile and `.tools` directory from that project
root. `init` always operates in the current directory.

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
- GitHub and GitLab release metadata and downloaded bytes are untrusted remote
  input.
- crates.io metadata and `.crate` archives are untrusted remote input.
- Published digests and checksum sidecars provide an upstream checksum.
- When neither exists, Binloom warns, hashes the downloaded bytes, and records
  `checksum-source = "download"`. This is trust on first use: later installs
  verify the committed checksum, but the initial lock operation trusts the
  bytes received over HTTPS.
- Downloads go to temporary files and are verified before decompression or
  execution.
- Completed executables are moved atomically into their versioned location.
- An existing executable at the exact locked path makes installation
  idempotent only after it passes the cache verification below.
- Resolution never executes downloaded content.
- Installing a Cargo source compiles trusted repository configuration and may
  execute the crate's build script. It requires existing `cargo` and `rustc`
  commands; Binloom never installs a Rust toolchain.
- Tar archives are validated before extraction: absolute paths, `..`
  components, links, and special files fail the install. Only one regular
  file is streamed out: the file named after the tool, or the archive's only
  file. Nothing else is unpacked.
- Unsupported archive formats fail instead of extracting paths or links.

## Cache verification

The committed manifest and lockfile define the expected artifacts. Everything
under `.tools/` is user-writable and never an independent source of
authenticity: stamp files only record that a publication finished.

Per-version directory (`.tools/<tool>/<version>/`):

| File | Purpose |
| --- | --- |
| `<tool>` | The executable |
| `.artifact-sha256` / `.crate-sha256` | Locked digest this install was published for (a hint) |
| `.artifact` | Retained, verified download for non-raw formats (gzip included) |
| `.executable-sha256` | Digest of the extracted executable |
| `.install.lock` | Cross-process lock owned by the Rust installer |

Every `exec`, `install`, and `binloomw` run recomputes digests; a stamp match
alone never counts as a hit:

- **Raw:** the executable hashes to the locked SHA-256.
- **Archive (gz, tar.\*, zip):** `.artifact` hashes to the locked SHA-256 and the
  bytes unpacked from it hash to the executable's digest.
- **Binloom's own bootstrap:** the same rules, applied by both `binloomw` and
  `binloom install`; `.tools/.bin/binloom` is never linked to an unverified
  executable.
- **Cargo builds:** the crate archive digest identifies the source, not the
  compiled bytes. The recorded `.executable-sha256` only detects accidental
  corruption or tampering by someone who cannot also rewrite the stamps; it is
  not an authenticity guarantee. A mismatch rebuilds from the
  checksum-verified crate source (needs `cargo`) and otherwise fails.
- **Legacy caches** without newer metadata are verified the same way; stamp-only
  Binloom bootstrap caches from older wrappers are not trusted and are repaired.

Integrity failures fail closed. An intact cache runs offline with no network.
A corrupt cache is repaired by re-downloading and verifying, so offline it
fails with an error instead of running modified bytes.

Executability is checked for the effective user (owner, group, other and ACLs).
A restrictive but valid mode such as `0700` is left alone. A verified file the
caller owns but cannot execute gets its owner execute bit restored. Executables
owned by another user (root excepted) or writable by group or others are
replaced from the verified artifact.

Publication and concurrency:

- Staged files (`.binloom-tmp.*`, wrapper: `.binloomw-tmp.*`) live in the
  version directory. Archive, executable, then stamps are published by atomic
  rename; the Rust installer fsyncs the files and directory, the wrapper only
  runs a best-effort `sync` before and after publishing (no per-file fsync).
  Any interrupted state reads
  as "not cached" and is repaired next run; a failed staging step leaves the
  previous installation untouched. Abandoned staging files are swept under
  the lock.
- The Rust installer holds an exclusive `flock` on `.install.lock`, opened with
  `O_NOFOLLOW`, and re-checks after locking that the locked inode is still the
  path's inode. `exec` keeps the lock until `execve`. FIFOs, symlinks, and
  directories at managed paths are rejected or replaced without following links.
- `binloomw` has no lock (`flock` is not portable to macOS). Concurrent wrappers
  and a concurrent `binloom install` are safe because every publish is an atomic
  rename of verified content and every hit is re-verified; the worst case is a
  redundant reinstall.

Known limits:

- Hashing and `execve` are separate steps. A process running as the same user
  that rewrites the executable in place after hashing is not excluded; closing
  that needs sealing (`memfd`/`fexecve`) or supervision. The same applies to two
  `binloomw` wrappers whose lockfiles name different artifacts for one Binloom
  version: they share one `.tools/binloom/<version>/` path, so one run can end up
  executing the other's verified bytes.
- Anyone who can rewrite both the executable and the lockfile controls what runs;
  the lockfile is trusted repository state.
- Managed cache directories (`.tools`, `.tools/<name>`, `.tools/<name>/<version>`,
  and `.tools/.bin` in the Rust installer) must be owned by the effective user
  (root excepted) and not group/other-writable (`mode & 0o022`); otherwise both
  the installer and `binloomw` fail closed and name the path to `chmod go-w`.
  New directories are created `0755` regardless of umask, so a `002` umask
  works. Directories already group-writable (e.g. created by an older version
  under `002`) must be fixed once by hand.
- `bootstrap.sh` has no cache and fetches `SHA256SUMS` from the same release
  origin as the archive: it detects corruption, not a compromised release.
- Windows is out of scope (#35).

## Language independence

Binloom does not inspect a project's `Cargo.toml`, `package.json`,
`pyproject.toml`, or equivalent dependency files. Cargo is an optional tool
source, not a requirement for the project using Binloom. Every repository uses
the same workflow:

```sh
./binloomw install
./binloomw exec <command> [arguments...]
```
