# ADR-0003: Keep Binloom CLI first; evolve an internal reusable installation core

- **Status:** Accepted
- **Date:** 2026-11-17
- **Decision owners:** Binloom maintainers
- **Related:** [ADR-0001](0001-explicit-project-and-tool-targets.md), [ADR-0002](0002-linux-gnu-and-musl-targets.md), [#41](https://github.com/KyrboForge/binloom/issues/41), [#39](https://github.com/KyrboForge/binloom/issues/39), [Agenvil #16](https://github.com/KyrboForge/Agenvil/issues/16), [Agenvil #42](https://github.com/KyrboForge/Agenvil/issues/42)
- **Implementation status:** Not implemented by this ADR; this change is documentation only.

## Context

Binloom is a project-local tool installer. A pinned `binloomw`, `binloom.toml`, and `binloom.lock` allow a repository to bootstrap trusted CLI binaries without a global Binloom installation. Agenvil intends to use Binloom as the **sole bootstrap/distribution mechanism for its own binary**, and may need to reuse binary acquisition for tools such as RTK. Agenvil also needs to provision user-scoped managed tools, including Python CLIs such as Headroom via `uv`.

Reimplementing release resolution, checksum checking, safe archive extraction and cache integrity inside Agenvil would duplicate high-risk infrastructure. On the other hand, publishing a stable Rust `binloom-core` API now would create premature semver commitments while Binloom's installer, platform model and Windows support are still evolving.

## Decision

**CLI-first, internal core now; separate library only when a real downstream integration needs it.**

### Short term: reliable process boundary

- Agenvil's kit commits **Binloom's existing** `binloomw`, manifest and lock, with Agenvil's release declared as a pinned tool. No `agenvilw` and no global Binloom, Agenvil, Cargo or Python required for the *native Agenvil binary* when prebuilt assets exist.
- Intended future first-run command: `./binloomw exec agenvil init`. Until [#34](https://github.com/KyrboForge/binloom/issues/34) lands, the supported flow needs `./binloomw install` first.
- Do not broaden `exec` into arbitrary unpinned internet tool discovery. It may install only a declared/locked command, with verified SHA-256 and safe extraction.
- Windows-native bootstrap is tracked separately in [#35](https://github.com/KyrboForge/binloom/issues/35); do not advertise it before implementation.
- Optional shell/PATH convenience is separate ([#32](https://github.com/KyrboForge/binloom/issues/32)). No child process can edit its parent shell's `PATH`, and no repo should need a new committed shell setup file.

### Internal reusable boundaries

Keep CLI parsing, project root discovery, human-readable output, project manifest/lock mutations and optional shell integration **outside** core operations. Introduce testable internal interfaces conceptually equivalent to:

| Concern | Responsibility |
| --- | --- |
| Target resolution | Canonical OS/arch/ABI, supported-target policy, deterministic release asset selection |
| Acquisition | Strict HTTPS/client behavior, bounded downloads, offline/cache mode |
| Verification | Artifact SHA-256 and checksum provenance; fail closed on mismatch |
| Extraction | Single intended executable; zip-slip/tar traversal/symlink/bomb resistance |
| Installation | Stage, verify, atomic activation, rollback, concurrency and repair |
| Planning | Typed desired artifact + trust policy + target + destination and error report |

These names describe responsibilities, **not public Rust API types**. A pure `InstallPlan`-like data structure can be used internally, but no stable external contract is committed by this ADR. Core must not depend on Claude, skills, agent manifests, the user's real HOME or interactive shell state.

### Separate ownership

- **Binloom CLI:** repository-local tool pins, locked release provenance, installation under `.tools/`, pinned wrapper, optional project-local PATH UX.
- **Potential Binloom core:** verified **native binary** artifact handling reusable by other Rust tools; may eventually accept an explicit install root without assuming a repository root.
- **Agenvil:** user-scoped tool registry and owned install prefix, agent/skill/rule configuration, adapter dispatch, tool lifecycle, AI content lock; uses `uv` for managed Python tools when appropriate.
- Do **not** merge `agenvil.lock` (AI content) into `binloom.lock` (native binary provenance). Avoid a parallel unchecked download/extract implementation.

## Gate for a separate crate

Only propose a separately versioned `binloom-core` once **all** are true:

1. Internal resolver/verifier/extractor/installer boundaries have stable, deterministic tests, including corrupted cache and concurrent first use ([#39](https://github.com/KyrboForge/binloom/issues/39)).
2. Repo-local `install` and lazy `exec` share those internals ([#34](https://github.com/KyrboForge/binloom/issues/34)).
3. Agenvil #42 supplies a concrete user-scoped binary acquisition integration requiring an explicit different installation root, with a demonstrated test fixture.
4. The downstream API's trust policy, typed errors and lifecycle are sufficiently understood to version responsibly.

A spike demonstrating the seam does not automatically authorize moving all CLI modules into a public crate.

## Consequences

**Benefits:** fastest clean Agenvil bootstrap; one security-sensitive native binary engine; independent evolution of Binloom CLI and Agenvil; avoid premature framework design.

**Costs:** an initial subprocess boundary and additional integration testing; publishing a library later will require ownership and version policy. CLI may need disciplined internal refactoring but should not change user-facing behavior.

## Alternatives rejected

- Add a second `agenvilw` bootstrapper or a private Agenvil download/verify/extract stack.
- Extract and publish `binloom-core` now, before an actual consumer and stable model exist.
- Force all tools including Python Headroom, OS package managers or AI-specific adapters into Binloom's repo-local responsibilities.
- Make shell RC changes or global Binloom installation prerequisites for using Agenvil.

## Test/rollout plan

1. Complete/verify [#27](https://github.com/KyrboForge/binloom/issues/27) archive handling and [#34](https://github.com/KyrboForge/binloom/issues/34) pinned lazy exec, without editing active Claude tasks from this ADR PR.
2. Centralize actual executable verification and concurrency-safe cache behavior in [#39](https://github.com/KyrboForge/binloom/issues/39).
3. Build internal modules and testable error/plan boundaries through [#41](https://github.com/KyrboForge/binloom/issues/41) while retaining the CLI contract.
4. Smoke Agenvil bootstrap: clean checkout, pinned release, no global Rust/Node/Python/Brew/WinGet, offline second run, corrupted cache refusal. Windows parity after #35.
5. Only then decide on `binloom-core` publishing and downstream linkage.

**Format policy:** the target/schema improvements in ADR-0001 and ADR-0002 remain additions to `manifest-version = 1` and `lock-version = 1`; there is no v2 migration prerequisite for the core work.
