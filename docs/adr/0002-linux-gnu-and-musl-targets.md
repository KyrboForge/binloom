# ADR-0002: Model GNU and musl as separate Linux targets

- **Status:** Accepted
- **Date:** 2026-11-17
- **Decision owners:** Binloom maintainers
- **Related:** [ADR-0001](0001-explicit-project-and-tool-targets.md), [#37](https://github.com/KyrboForge/binloom/issues/37), [#38](https://github.com/KyrboForge/binloom/issues/38), [#40](https://github.com/KyrboForge/binloom/issues/40)
- **Implementation status:** Implemented by [#37](https://github.com/KyrboForge/binloom/issues/37) for all four explicit Linux GNU/musl targets. Windows is tracked in [#35](https://github.com/KyrboForge/binloom/issues/35) and cross-platform CI fixtures in [#40](https://github.com/KyrboForge/binloom/issues/40). [#38](https://github.com/KyrboForge/binloom/issues/38) implements pinned-wrapper upgrades with legacy bootstrap keys and evolved v1 locks.

## Context

Binloom 0.3.2 identifies only `linux-aarch64` and `linux-x86_64`; internally `Platform::target()` maps both to musl-oriented Rust target strings. A Linux release binary may depend on glibc, musl, a particular libc version, or other dynamically linked libraries. Operating system plus CPU architecture is **not** a complete compatibility contract. The repository-local lockfile should not claim one Linux artifact is valid for unrelated ABIs.

This is particularly important for Alpine containers, mainstream GNU/Linux development hosts, cross-platform CI and Agenvil's requirements. Filename aliases alone are not trustworthy proof that an executable is compatible.

## Decision

### Canonical explicit targets

```text
macos-aarch64
macos-x86_64
linux-aarch64-gnu
linux-aarch64-musl
linux-x86_64-gnu
linux-x86_64-musl
windows-x86_64
```

The macOS/Windows entries share ADR-0001's target namespace; Windows support is separately delivered by [#35](https://github.com/KyrboForge/binloom/issues/35). Linux IDs capture the libc/ABI family explicitly. Binloom's target IDs are distinct from Rust target triples, even though archive patterns such as `{target}` may expand to triples like `x86_64-unknown-linux-gnu` or `x86_64-unknown-linux-musl`.

**Never implicitly fall back from GNU to musl or musl to GNU.** A missing compatible release is an actionable resolution failure, not permission to download any similarly named executable. A `musl` suffix does not guarantee static linking; a `gnu` suffix does not declare a minimum glibc version.

### Target resolution contract

1. Detect OS + CPU and, on Linux, ABI reliably for the actual runtime where feasible; unknown ABI is a clear unsupported/undetermined error, not guessed from distro name.
2. Normalize release-name aliases **for matching**, while preserving the canonical target in the lock. Disambiguate `x64`/`amd64` and `arm64`/`aarch64` separately from `gnu`/`musl`.
3. Explicitly map requested target to a compatible release asset; pin source, release tag, URL, asset name, format, SHA-256 and checksum provenance in the existing lock v1 artifact table.
4. Treat universal macOS binaries as a *separate verified exception* to per-target asset uniqueness, not as a general reason to reuse Linux assets across ABIs.
5. Do not assume minimum glibc or transitive shared-library compatibility from the canonical key. A future verified compatibility declaration or executable inspection may extend this model, with separate review and tests.
6. Preserve legacy `linux-aarch64` and `linux-x86_64` lock keys for old manifests in legacy mode. Their content must not be silently re-labeled or their checksums copied under new ABI keys.

### Example (new explicit target mode, still v1)

```toml
manifest-version = 1

[binloom]
version = "0.4.0" # illustration

[platforms]
targets = ["linux-x86_64-gnu", "linux-x86_64-musl"]

[tools.shellcheck]
version = "0.11.0"
source = "github:koalaman/shellcheck"
# both targets required unless narrowed
```

Corresponding lock entries use `[tools.shellcheck.artifacts.linux-x86_64-gnu]` and `[tools.shellcheck.artifacts.linux-x86_64-musl]` with separate, verified artifacts where those are published. Absence of one fails locking; it does not auto-restrict the tool.

## Compatibility and rollout

The schema **remains `manifest-version = 1`, `lock-version = 1`**. Implementation changes the semantics only for **new explicit target declarations**. Old target IDs remain usable in legacy mode. This avoids creating an unnecessary v2 format while the project has negligible external adoption, but it does *not* imply old Binloom binaries understand the expanded v1 fields. See [#38](https://github.com/KyrboForge/binloom/issues/38) for the pinned-Binloom/wrapper upgrade sequence.

The POSIX wrapper currently selects `binloom.artifacts.linux-x86_64` / `binloom.artifacts.linux-aarch64` without understanding libc. It must remain capable of bootstrapping the new engine from a new lock on a clean host: either upgrade the wrapper reader in the pinned compatibility release, or retain carefully validated bootstrap entries during transition. No silent checksum/ABI re-labeling.

## Consequences

**Benefits:** avoids many false-positive asset matches, supports explicit GNU and Alpine/musl environments, and gives failures diagnostic meaning.

**Costs:** more platform variants and test cases; some upstreams publish only one Linux flavor, so an explicit two-ABI project will need a narrower tool scope or deliberate upstream change. Metadata-only detection cannot prove arbitrary executable compatibility.

## Alternatives rejected

- Keep a generic Linux target indefinitely and let execution fail at runtime.
- Prefer musl unconditionally as "portable" without proving static linkage.
- Permit implicit GNU↔musl fallback or silently accept unknown ABI.
- Require a new lock-version 2 for these additive, opt-in fields.

## Test/acceptance matrix

- GNU amd64 + aarch64, musl amd64 + aarch64; explicit target detection and canonical output.
- Exact ABI matches succeed; absent ABI-compatible asset fails closed; malformed or misleading asset names do not pass.
- Ubuntu/Debian GNU and Alpine musl smoke, with real environment support claims separated from fixtures.
- Universal macOS binaries do not cause Linux ABI reuse; Windows target additions remain independent.
- Legacy v1 `linux-*` entries remain loadable and are never implicitly rewritten.
- Regression checks for [#28](https://github.com/KyrboForge/binloom/issues/28) aliases and [#29](https://github.com/KyrboForge/binloom/issues/29) wrong-platform asset matching.
- Implementation: [#37](https://github.com/KyrboForge/binloom/issues/37), cross-platform fixtures [#40](https://github.com/KyrboForge/binloom/issues/40); documentation-only ADR PR does not change code.
