<!-- TL;DR: 21 decisions. Key: ADR-017, ADR-018, ADR-019, ADR-020, ADR-021 -->
# Architectural Decisions

Append-only. Status changes allowed; deletions prohibited.

## ADR-001: obs_a7f3c1 — Squash merge with pre-merge gate

**Date**: 2026-05-27
**Status**: Accepted
**Confidence**: 0.95

### Context

When merging a feature/refactor branch into main after a resolve cycle, the team uses a structured merge process to keep main history clean and prevent quality regressions.

### Decision

Use squash merge for feature/refactor branches into main, gated by a mandatory pre-merge checklist:
1. Lint and formatting must pass.
2. Each linked issue must be verified as actually addressed by the PR content (not just referenced).

### Rationale

Squash merge keeps main history linear and readable. The gate prevents closing issues prematurely or introducing quality regressions. Automated PR-keyword issue closing does not verify semantic completeness.

### Evidence

- "let's move forward and squash merge this PR into main. Make sure that all the issues it closes are closed. Before you close the issues, make sure the content of this PR actually addresses the issues we're about to close"
- "One important thing to check is that linting and formatting, everything is passing before we move forward"

---

## ADR-002: obs_b2d8e4 — Verify PR content addresses linked issues before closing

**Date**: 2026-05-27
**Status**: Accepted
**Confidence**: 0.95

### Context

A PR resolving multiple linked issues is about to be merged. Automated issue-closing via PR keywords (`Closes #N`) does not verify whether the fix is semantically complete or correctly scoped.

### Decision

Before closing any linked issues during a merge, explicitly audit the PR diff to confirm each issue is addressed by the actual changes. Only close issues confirmed as resolved.

### Rationale

A fix may be incomplete, mis-scoped, or address a symptom rather than the root cause. Semantic verification prevents false closure and preserves issue integrity for tracking.

### Evidence

- "Before you close the issues, make sure the content of this PR actually addresses the issues we're about to close. If everything looks good once you do that, we can move to main"
- Resolve cycle result: Reviews Processed: 11 reports — Fixed: 11, False Positive: 6, Deferred: 0

---

## ADR-003: Automated release via workflow_dispatch with version input

- **Date**: 2026-06-01
- **Status**: Accepted
- **Context**: v0.1.0 required manual tag/delete/re-tag loop that was error-prone
- **Decision**: add workflow_dispatch trigger with version input to release.yml that bumps all Cargo.toml and package.json versions, stamps CHANGELOG, commits, creates tag, and triggers publish pipeline
- **Consequences**: single command replaces multi-step manual ceremony, eliminates risk of version drift between manifests
- **Source**: self-learning:obs_c3d7f1

## ADR-004: Tiered dependency update strategy: patch → minor → major, verified at each tier

- **Date**: 2026-06-01
- **Status**: Accepted
- **Context**: multiple Dependabot PRs spanning patch, minor, and major version bumps across Rust and JS ecosystems
- **Decision**: group updates by risk tier and process sequentially — patch first (safe to batch), then minor, then major (one at a time with explicit compatibility check)
- **Consequences**: isolates regressions to the responsible tier, prevents compound failures where multiple major upgrades interact
- **Source**: self-learning:obs_d4e8g2

## ADR-005: Branch + full CI (all 3 OS targets) + examples gate before merging dependency or compatibility updates

- **Date**: 2026-06-01
- **Status**: Accepted
- **Context**: dependency updates can silently break platform-specific builds or example configurations not covered by unit tests
- **Decision**: any update touching peerDeps, major versions, or build tooling must be validated on a dedicated branch with full CI across all 3 OS targets (Linux, macOS, Windows) and all examples built and run before squash-merge to main
- **Consequences**: catches platform-specific regressions and example drift that the test suite alone does not cover
- **Source**: self-learning:obs_e5f9h3

## ADR-006: --admin merge bypass is acceptable when CI is already green and PR has completed review cycle

- **Date**: 2026-06-01
- **Status**: Accepted
- **Context**: branch protection blocks squash merge even after CI passes and code review cycle is complete
- **Decision**: use gh pr merge --admin --squash to bypass protection
- **Consequences**: branch protection exists to guarantee CI coverage — once CI is green and a review cycle has completed, admin bypass retains the merge benefits (squash history, issue closure) without adding process overhead
- **Source**: self-learning:obs_f6g0i4

## ADR-007: Defer features with high distribution/DevOps overhead (editor tooling, LSP) to final milestone

- **Date**: 2026-06-01
- **Status**: Accepted
- **Context**: roadmap planning for post-v0.1.0 milestones — VS Code extension, Tree-sitter grammar, LSP server
- **Decision**: defer editor tooling features to the final milestone (v1.0.0) despite appearing low-effort at implementation level
- **Consequences**: distribution and validation overhead is disproportionate — prefer enriching the core language and ecosystem first
- **Source**: self-learning:obs_g7h1j5

## ADR-008: Bundle related small language features into a single PR rather than incremental one-at-a-time delivery

- **Date**: 2026-06-01
- **Status**: Accepted
- **Context**: v0.2.0 language features #53 (built-ins), #54 (default args), #55 (logical operators) proposed as sequential single-issue PRs
- **Decision**: implement all three language features in a single PR
- **Consequences**: related features touching the same compiler layers create less overhead when batched — avoids repeated merge/rebase ceremony for tightly coupled changes
- **Source**: self-learning:obs_h8i2k6

## ADR-009: Use milestone-tagged GitHub issues with mini-PRDs as the planning artifact for roadmap items

- **Date**: 2026-06-01
- **Status**: Accepted
- **Context**: post-v0.1.0 roadmap with 17 items across 5 milestones needing planning artifacts
- **Decision**: one GitHub issue per roadmap item tagged to target milestone, with mini-PRD (motivation + proposed API + design considerations + acceptance criteria)
- **Consequences**: right-sized artifact — orients future planning sessions without over-specifying
- **Source**: self-learning:obs_i9j3l7

## ADR-010: Reuse parse_expr_inner across interpolation and directive parsing to avoid grammar duplication

- **Date**: 2026-06-06
- **Status**: Accepted
- **Context**: @for and @if only accepted bare variable names while interpolation had full expression support
- **Decision**: extract parse_expr_inner from parse_interpolation_expr and wire it into directive condition and iterable parsing
- **Consequences**: single grammar shared between interpolation and directives eliminates semantic gap and keeps the two paths in sync when new expression forms are added
- **Source**: self-learning:obs_j1k4m8

## ADR-011: Specify explicit wasm-opt feature flags matching LLVM output rather than using -all or --all-features

- **Date**: 2026-06-06
- **Status**: Accepted
- **Context**: wasm-opt was disabled because newer LLVM emits instructions that crash wasm-opt without matching feature flags
- **Decision**: pass explicit feature flags (--enable-bulk-memory, --enable-sign-ext, --enable-nontrapping-float-to-int, --enable-mutable-globals) rather than -all
- **Consequences**: explicit flags are self-documenting, match actual LLVM output, and are the consensus approach across wasm-pack and cargo-leptos communities
- **Source**: self-learning:obs_k2l5n9

## ADR-012: Extract repeated CI tool setup into composite action for single-point version management

- **Date**: 2026-06-06
- **Status**: Accepted
- **Context**: wasm-pack + Binaryen setup duplicated across multiple CI jobs
- **Decision**: extract into a composite action at .github/actions/setup-wasm/ with SHA-pinned dependencies and version parameters
- **Consequences**: single-point version management means updating Binaryen or wasm-pack requires one edit
- **Source**: self-learning:obs_l3m6o0

## ADR-013: MDS runtime virtual filesystem: Linux fuse3 crate (pure Rust) + macOS FSKit (Swift + C FFI bridge)

- **Date**: 2026-06-06
- **Status**: Accepted
- **Context**: designing on-read compilation for .md files containing MDS directives
- **Decision**: dual-platform virtual filesystem — Linux uses fuse3 crate speaking /dev/fuse directly in pure Rust, macOS uses Apple FSKit with a Swift extension bridging to mds-core via a C FFI crate (mds-cffi)
- **Consequences**: each platform uses its native mechanism with no third-party kernel extensions
- **Source**: self-learning:obs_n5o8q2

## ADR-014: Frontmatter imports resolve before body @import directives; namespace collision is a compile error

- **Date**: 2026-06-06
- **Status**: Accepted
- **Context**: adding imports key to YAML frontmatter as alternative to body @import directives
- **Decision**: frontmatter imports processed first, duplicate namespace between frontmatter and body is a hard compile error
- **Consequences**: first-wins ordering makes dependency resolution deterministic
- **Source**: self-learning:obs_o6p9r3

## ADR-015: Post-release verification checklist: confirm all packages live on registries and npm shows SLSA provenance attestation before declaring release done

- **Date**: 2026-06-06
- **Status**: Accepted
- **Context**: after triggering a coordinated release across crates.io and npm for all @mdscript/* packages
- **Decision**: post-release verification is a required step — confirm each package appears on its registry at the correct version AND that npm shows SLSA v1 provenance attestation
- **Consequences**: publish jobs can succeed in CI without the package being fully indexed or provenance attached — registry verification catches partial publish failures and attestation drift before users encounter them
- **Source**: self-learning:obs_s0t4u8

## ADR-016: Re-validate dynamically-resolved values at runtime even when the static literal form is already checked at parse time

- **Date**: 2026-06-07
- **Status**: Accepted
- **Context**: @message role validation rejected empty/whitespace roles at parse time, but {expr} dynamic roles resolve to a value only at runtime and thus bypassed the parse-time check
- **Decision**: enforce the same non-empty-role invariant at runtime for dynamically-resolved roles, mirroring the parse-time rule, rather than trusting that parse-time validation covers all role values
- **Consequences**: any invariant that can be violated by a value computed after parsing must be re-checked at the point the value becomes concrete — defense in depth keeps the runtime and parse-time contracts identical and prevents silently emitting a structurally invalid message. Extends ADR-010.
- **Source**: self-learning:obs_u2v6w0

## ADR-017: Triage every non-blocking review suggestion to an explicit verdict (fixed / false-positive / wont-fix / deferred) with recorded per-item reasoning

- **Date**: 2026-06-07
- **Status**: Accepted
- **Context**: a code-review cycle produces a mix of blocking findings, should-fix items, and lower-value suggestions
- **Decision**: give every suggestion an explicit verdict (fixed / false-positive / wont-fix / deferred) and record a short rationale for each wont-fix item rather than silently dropping it
- **Consequences**: an auditable per-item disposition makes the resolution defensible, prevents the same suggestion being re-raised next cycle, and distinguishes a conscious wont-fix from an overlooked finding. Complements ADR on skipping a second review cycle (obs_m4n7p1).
- **Source**: self-learning:obs_x5y9z3

## ADR-018: Skip second code review cycle when convergence signal is strong: zero blocking issues and all should-fix items resolved

- **Date**: 2026-06-07
- **Status**: Accepted
- **Context**: deciding whether to run a second code review cycle after all blocking issues resolved
- **Decision**: skip the second cycle when the convergence signal is strong — zero blocking items, all should-fix items resolved, CI green, and only consciously deferred/wont-fix items remain
- **Consequences**: a second cycle yields diminishing returns when the first cycle fixed everything actionable
- **Source**: self-learning:obs_m4n7p1

## ADR-019: Track only curated team knowledge under .devflow/ via ignore-by-default plus explicit re-includes

- **Date**: 2026-06-08
- **Status**: Accepted
- **Context**: 519 files under .devflow/ were tracked in git, most per-developer or transient (docs reports, dream runtime markers, locks, scratch results) rather than shared team knowledge
- **Decision**: adopt an ignore-by-default .devflow/.gitignore (* then explicit !re-includes) tracking ONLY the curated shared set — decisions/decisions.md, decisions/pitfalls.md, features/index.json, features/*/KNOWLEDGE.md, release-baseline.md, and the policy .gitignore files
- **Consequences**: shared knowledge belongs in git, transient/per-developer state does not. Ignore-by-default makes any NEW .devflow/ file ignored unless explicitly listed, preventing future drift where scratch or runtime files silently re-enter tracking. Committed as PR #92.
- **Source**: self-learning:obs_y6z0a4

## ADR-020: Do not mutate the working tree while background maintenance agents are actively writing .devflow/ files — wait for them to settle first

- **Date**: 2026-06-08
- **Status**: Accepted
- **Context**: a branch switch / cleanup (checkout main, delete merged branch) was blocked by uncommitted .devflow/ changes that background Dream maintenance agents (decisions + knowledge) were concurrently writing
- **Decision**: sequence the work — do NOT stash, discard, reset, or checkout over those files mid-flight
- **Consequences**: a stash/checkout/reset that races an actively-writing agent can drop or corrupt curated decision/knowledge output. The irreversible step (the merge) was already complete, so the reversible local cleanup can safely wait for maintenance to settle.
- **Source**: self-learning:obs_z7a1b5

## ADR-021: Liveness-gated reconcile for the file-watch loop: cheap per-tick re-arm, full directory rescan only on watch-loss/recovery

- **Date**: 2026-06-09
- **Status**: Accepted
- **Context**: designing the mds watch event loop to recompute file/dependency state without paying a full directory tree walk on every filesystem event
- **Decision**: gate reconciliation on watcher liveness — on each tick do only a cheap re-arm of watches, and perform a full directory rescan ONLY when the watcher is lost and recovers
- **Consequences**: a per-tick tree walk is O(tree) work on every event and does not scale
- **Source**: self-learning:obs_b9c3d7
