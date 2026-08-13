# Contributing to MDS

Thanks for your interest in contributing! This document describes the local
workflow and the checks that must pass before a change can be merged.

## Prerequisites

- **Rust**: stable toolchain; the workspace MSRV is **1.88** (declared in the
  root `Cargo.toml`). The published crates (`mds-core`, `mds-cli`) must compile on
  1.88.
- **Node.js**: **≥ 22** (see `engines` in the package manifests).
- **wasm-pack** + the `wasm32-unknown-unknown` target, for the WASM build/tests.
- **@napi-rs/cli** (installed via `npm ci`), for the native addon.

## Repository layout

| Path | What it is |
|------|------------|
| `crates/mds-core` | The compiler library (published to crates.io as `mds-core`) |
| `crates/mds-cli` | The `mds` binary (published as `mds-cli`) |
| `crates/mds-wasm` | WASM bindings (`wasm-bindgen`) |
| `crates/mds-napi` | Native Node addon (`napi-rs`), host package `@mdscript/mds-napi` |
| `packages/mds` | Universal JS/TS bindings (`@mdscript/mds`) |
| `packages/mds-wasm` | WASM workspace wrapper (`@mdscript/mds-wasm`) |
| `packages/{vite,rollup}-plugin`, `packages/webpack-loader`, `packages/bundler-utils` | Bundler integrations |
| `examples/` | Runnable templates and integration apps |

## Quality gates

All of the following must pass locally and in CI before merge.

### Rust

```bash
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo check -p mds-core -p mds-cli          # on the 1.88 toolchain (MSRV)
```

Clippy warnings are treated as errors. Keep the build warning-free.

### WASM

```bash
wasm-pack test --node crates/mds-wasm
```

### JavaScript / TypeScript

```bash
npm ci
npm run build --workspaces --if-present
npm test --workspaces --if-present
```

Backend parity matters. When touching the JS bindings, run the `@mdscript/mds`
suite under both backends:

```bash
MDS_BACKEND=native npm test -w @mdscript/mds
MDS_BACKEND=wasm   npm test -w @mdscript/mds
```

### Source hygiene

All tracked source must be free of hazardous codepoints. The gate runs
automatically in CI (job key `source-hygiene`, display name `Source hygiene`;
`scripts/verify-pr-checks.mjs` matches on the display name — renaming it in
`ci.yml` requires updating `EXPECTED_CONTEXTS` in that script) and can be run
locally:

```bash
node scripts/verify-no-control-bytes.mjs          # full tracked-tree scan
node scripts/verify-no-control-bytes.mjs --staged  # staged-only (pre-commit)
npm run test:gates                                 # positive-control spec suite
```

Exit codes are a contract: `0` no hazards found (prints file and byte counts
for non-vacuity), `1` hazard found or scan failed closed (zero files scanned,
unreadable path, stale allowlist entry, git not on PATH), `2` indeterminate
(a git subcommand failed unexpectedly — never treat `2` as clean).

Note on exit-code symmetry: the scanner folds a missing `git` executable into
its fail-closed exit 1 (a known, named failure — the tool can say definitively
it could not run), whereas the verifier (`verify-pr-checks.mjs`) treats a missing
or outdated `gh` as indeterminate exit 2 (the tool cannot assess merge safety).
Both refuse to report success; they differ in whether tool absence is a named
failure (exit 1) or an indeterminate error (exit 2).

**Opt-in pre-commit hook** (replaces `.git/hooks` wholesale — document your
existing local hooks before enabling):

```bash
git config core.hooksPath scripts/hooks
```

**Hazard class**: C0 (0x00-0x1F) excluding TAB and LF, DEL (0x7F), C1
(0x80-0x9F at codepoint level — catches UTF-8-encoded NEL 0xC2 0x85), the
twelve Unicode `Bidi_Control=Yes` codepoints including U+061C (Trojan Source,
CVE-2021-42574), plus U+2028 (LS), U+2029 (PS), and U+FEFF (BOM). CR (U+000D)
is permitted only as the first byte of CRLF.

**BSD grep trap**: macOS ships BSD grep, which has no `-P` flag and exits 2
with empty output. That empty output is indistinguishable from a clean scan.
The gate uses pure Node codepoint iteration — never grep.

**Authoring rule**: when writing code or documentation that mentions hazardous
codepoints, use numeric notation (`U+202E`, `0x202e`, or `String.fromCodePoint(0x202e)`)
rather than backslash-u escapes. The edit tooling decodes the 4-hex-digit form
`\uXXXX` to live bytes, injecting the hazard into the very file that warns about it.

## Pull requests

- **Conventional Commits**: PR titles and commits follow
  [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`,
  `refactor:`, `chore:`, `docs:`, ...).
- **Update the CHANGELOG**: add user-facing changes under `## [Unreleased]` in
  `CHANGELOG.md`.
- **Tests**: add or update tests for behavior changes; assert outcomes, not
  implementation details.
- **No regressions**: every existing test must still pass.

## Merging

**Admin merges require the pre-merge check verifier.** GitHub's `--admin`
flag bypasses required-status enforcement; a cancelled CI run reads as
"not failing" rather than as failing (PF-017). Run the verifier before any
`gh pr merge --admin`:

```bash
node scripts/verify-pr-checks.mjs <pr-number>
```

The verifier reads required contexts from live branch protection, checks that
every context is `status=completed` AND `conclusion=success`, and on pass
emits a `gh pr merge --squash --match-head-commit <sha>` command pinned to
the verified SHA (closes the TOCTOU window).

Exit codes are a contract: `0` all Tier A, Tier A+, and Tier B checks passed,
`1` any Tier A failure (required context missing or non-success), any Tier A+
failure (a job listed in `EXPECTED_CONTEXTS` is absent or non-success), any
Tier B failure (non-required check-run concluded
failure/cancelled/timed_out/action_required/stale), or zero check-runs found,
`2` the tool could not tell (protection unreadable, no required contexts
configured, `gh` older than 2.31, incomplete pagination). **Only `0` means
verified** — never read `2` as a pass.

Tier A+ is the binding mechanism for the `Source hygiene` gate: the CI job key
is `source-hygiene` (ci.yml), but its display name — `Source hygiene` — is what
GitHub reports as the check-run name and what `EXPECTED_CONTEXTS` in
`scripts/verify-pr-checks.mjs` matches. The job is not among `main`'s required
branch-protection contexts. Tier B alone cannot make it binding: Tier B only
iterates check-runs that already exist in the check-run list — an absent job has
nothing to iterate. Tier A+ (`EXPECTED_CONTEXTS`) closes this gap by asserting
presence and passing with the same semantics as Tier A, so an absent, renamed,
or pre-start-cancelled `Source hygiene` run is FAIL, not a pass (applies
ADR-009, avoids PF-013). Tier B still applies when the run exists but concluded
badly. **Renaming the display `name:` in `ci.yml` requires updating
`EXPECTED_CONTEXTS` in `scripts/verify-pr-checks.mjs` to match.**

Scope, stated so it is not assumed: the verifier checks the checks *on one
commit*. It does **not** assert that the head is up to date with the base
branch, so a stale-but-green head can still be merged under `--admin` even
after the verifier passes. Keep the branch rebased. It does **not** assert that
`source-hygiene` is a required context — `--admin` bypasses required-status
enforcement outright for non-required checks, so Tier A+ (`EXPECTED_CONTEXTS`)
and Tier B are complementary binding mechanisms for non-required jobs — Tier A+
covers absent and renamed jobs (Tier B cannot: it only iterates existing
check-runs); Tier B covers existing runs that concluded badly. Tier B fails on
non-required check-runs that are still `queued` or `in_progress` — a
non-completed run is not evidence of success (avoids PF-017). Ensure all jobs
have completed before running the verifier.

If the base branch is unprotected (e.g. a wave branch), supply `--required-from`:

```bash
node scripts/verify-pr-checks.mjs <pr-number> --required-from main
```

## Security

Please report vulnerabilities privately. See [SECURITY.md](./SECURITY.md). Do not
open public issues for security problems.

## Code of Conduct

This project follows the [Contributor Covenant 2.1](CODE_OF_CONDUCT.md). By
participating, you agree to abide by its terms.

