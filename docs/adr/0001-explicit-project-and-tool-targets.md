# ADR-0001: Explicit project and tool targets

- **Status:** Accepted
- **Date:** 2026-11-17
- **Decision owners:** Binloom maintainers
- **Related:** [ADR-0002](0002-linux-gnu-and-musl-targets.md), [ADR-0003](0003-cli-first-internal-install-core.md), [#36](https://github.com/KyrboForge/binloom/issues/36), [#38](https://github.com/KyrboForge/binloom/issues/38), [#40](https://github.com/KyrboForge/binloom/issues/40)
- **Implementation status:** Implemented by [#36](https://github.com/KyrboForge/binloom/issues/36) for the four legacy targets and by [#37](https://github.com/KyrboForge/binloom/issues/37) for explicit Linux GNU/musl targets. Windows is tracked in [#35](https://github.com/KyrboForge/binloom/issues/35), bootstrap and upgrades in [#38](https://github.com/KyrboForge/binloom/issues/38), and cross-platform CI fixtures in [#40](https://github.com/KyrboForge/binloom/issues/40).

## Context

Binloom 0.3.2 resolves every tool against a hard-coded four-platform matrix (`macos-aarch64`, `macos-x86_64`, `linux-aarch64`, `linux-x86_64`). Resolution fails when one of those release assets is unavailable, even if a project only needs the tool on one platform. A single machine may successfully use a binary while the lock incorrectly claims the same binary works on a different OS/architecture (see [#29](https://github.com/KyrboForge/binloom/issues/29)).

Repositories increasingly mix platform-specific commands, including consumers such as Agenvil. We need explicit, reproducible platform requirements rather than opportunistic host-only resolution. Conversely, we must not change the meaning of existing locks silently.

## Decision

1. A project declares **an explicit set of required targets** in optional `[platforms].targets` when using the new feature.
2. Each tool may declare its own optional `targets`; the default is **the full project target set**. A tool's declared targets must be a **nonempty subset** of the project set.
3. Resolution produces one deterministic, committed lockfile for the complete declared target matrix, independent of the host performing resolution. A missing required asset fails resolution; the resolver never silently removes a target.
4. On a supported project host, `install` skips a tool that explicitly excludes that host; `exec` for that tool fails with an actionable unsupported-target message. On a host outside the project set, `install` fails before making changes.
5. An identical upstream asset may serve more than one target **only where its compatibility is defensible** (e.g., verified universal macOS executable). Shared URL alone is not evidence of cross-OS compatibility.
6. This is an **in-place additive evolution**: retain `manifest-version = 1` and `lock-version = 1`. No v2 format and no `binloom migrate` command. New fields are optional, but older Binloom executables may reject them; see upgrade protocol below.

### Manifest example

```toml
manifest-version = 1

[binloom]
version = "0.4.0" # illustration, not a release commitment

[platforms]
targets = [
  "macos-aarch64",
  "macos-x86_64",
  "linux-aarch64",
  "linux-x86_64",
]

[tools.actionlint]
source = "github:rhysd/actionlint"
version = "1.7.7"
# targets omitted: inherits every project target

[tools.linux-profiler]
source = "github:example/linux-profiler"
version = "1.0.0"
targets = ["linux-x86_64"]
```

Target names are **Binloom canonical IDs**. The lock orders them as `macos-aarch64`, `macos-x86_64`, `linux-aarch64`, `linux-x86_64` regardless of manifest order. Duplicate and malformed entries are rejected.

### Lock additions (still `lock-version = 1`)

```toml
lock-version = 1

[platforms]
targets = ["macos-aarch64", "linux-x86_64"]

[tools.example]
version = "1.2.3"
source = "github:vendor/example"
tag = "v1.2.3"
targets = ["linux-x86_64"]
asset-pattern = "example_{version}_{os}_{arch}.tar.gz"

[tools.example.artifacts.linux-x86_64]
asset = "example_1.2.3_linux_x86_64.tar.gz"
url = "https://example.invalid/example.tar.gz"
sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
format = "tar-gz"
checksum-source = "digest"
```

The lock example only shows the *target-specific tool portion*, not the required Binloom/wrapper bootstrap entries. `asset-pattern` is a **snapshot of a resolver input**, not an output asset; omit it if the manifest uses automatic matching. `targets` under each tool is its **effective** target set. The reader must compare the manifest's relevant inputs with locked values, including source, version, asset selection pattern, project targets and effective tool targets. The current source/version-only comparison is insufficient. Keep this explicit rather than introducing a hash-only opaque request fingerprint for now.

### Legacy v1 semantics

- An existing manifest **without** `[platforms]` retains the prior four-target matrix and legacy Linux target keys. This applies regardless of the newer resolver supporting additional platforms.
- Tool-level `targets` requires explicit `[platforms]`; absent project targets + tool targets is an invalid mixed mode.
- Existing `binloom.lock` entries retain their original meaning. Do not reinterpret `linux-x86_64` as verified GNU or musl, and do not silently rewrite them during `install`.
- Once a project deliberately opts in to `[platforms]` and regenerates its lock, only the new explicit target contract applies.
- New manifests remain version 1, but **old executables are not guaranteed to read the new optional fields**. The maintainer upgrades pinned Binloom and its wrapper to a compatible release **first**, and only then adds target declarations and regenerates the lock. [#38](https://github.com/KyrboForge/binloom/issues/38) owns this transition, including clean-clone bootstrapping.

## Consequences

**Positive:** platform-specific developer tools become representable without weakening CI reproducibility; the resolver can produce useful diagnostics instead of misleading cross-platform lock entries.

**Cost:** more platform-specific release lookups, larger locks, compatibility-sensitive upgrades of existing consumers and additional test coverage. Some tools will still be impossible to lock for the full project scope unless explicitly narrowed.

**Security:** installing a tool for an unintended platform is a correctness and supply-chain risk. Never silently default to a different architecture, OS or binary just because a release asset is missing. Cross-target reuse requires evidence, not just filename matching.

## Alternatives rejected

- Require every tool to publish for every Binloom-supported OS, even when not used there.
- Resolve only for the machine generating the lockfile (host-specific and non-reproducible in CI).
- Automatically infer each tool's supported targets from whatever assets happen to exist (silent feature loss on update).
- Introduce manifest v2/lock v2 and an explicit migration CLI before v1 has real external consumers.

## Test/rollout requirements

- Manifest/lock legacy roundtrip, opt-in roundtrip, strict unknown target validation, subset and nonempty constraints, deterministic serialization, detection of `asset-pattern` drift.
- Required-but-missing release asset must fail **before** mutating repository files; optional on-host tool must skip or error according to the command semantics.
- Universal macOS case and cross-OS false-positive regression (#29).
- Clean clone with pinned wrapper; old files work with the new binary; no automatic conversion to explicit targets.
- Implementation and tests belong in [#36](https://github.com/KyrboForge/binloom/issues/36), [#38](https://github.com/KyrboForge/binloom/issues/38), and [#40](https://github.com/KyrboForge/binloom/issues/40). Do not alter Claude's currently active #27–#32 issues/PRs as part of this documentation PR.
