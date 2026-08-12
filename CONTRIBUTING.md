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
automatically in CI (`source-hygiene` job) and can be run locally:

```bash
node scripts/verify-no-control-bytes.mjs          # full tracked-tree scan
node scripts/verify-no-control-bytes.mjs --staged  # staged-only (pre-commit)
```

**Opt-in pre-commit hook** (replaces `.git/hooks` wholesale — document your
existing local hooks before enabling):

```bash
git config core.hooksPath scripts/hooks
```

**Hazard class**: C0 (0x00-0x1F) excluding TAB and LF, DEL (0x7F), C1
(0x80-0x9F at codepoint level — catches UTF-8-encoded NEL 0xC2 0x85), the
twelve Unicode `Bidi_Control=Yes` codepoints (Trojan Source, CVE-2021-42574)
including U+061C, U+2028 (LS), U+2029 (PS), and U+FEFF (BOM). CR (U+000D) is
permitted only as the first byte of CRLF.

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

Exit codes are a contract: `0` verified, `1` a required context is missing or
not successful, `2` the tool could not tell (protection unreadable, no required
contexts configured, `gh` older than 2.31, incomplete pagination). **Only `0`
means verified** — never read `2` as a pass.

Scope, stated so it is not assumed: the verifier checks the checks *on one
commit*. It does **not** assert that the head is up to date with the base
branch, so a stale-but-green head can still be merged under `--admin` even
after the verifier passes. Keep the branch rebased.

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

