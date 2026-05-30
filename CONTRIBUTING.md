# Contributing to MDS

Thanks for your interest in contributing! This document describes the local
workflow and the checks that must pass before a change can be merged.

## Prerequisites

- **Rust** — stable toolchain; the workspace MSRV is **1.88** (declared in the
  root `Cargo.toml`). The published crates (`mds-core`, `mds-cli`) must compile on
  1.88.
- **Node.js** — **≥ 22** (see `engines` in the package manifests).
- **wasm-pack** + the `wasm32-unknown-unknown` target — for the WASM build/tests.
- **@napi-rs/cli** (installed via `npm ci`) — for the native addon.

## Repository layout

| Path | What it is |
|------|------------|
| `crates/mds-core` | The compiler library (published to crates.io as `mds-core`) |
| `crates/mds-cli` | The `mds` binary (published as `mds-cli`) |
| `crates/mds-wasm` | WASM bindings (`wasm-bindgen`) |
| `crates/mds-napi` | Native Node addon (`napi-rs`) — host package `@mdscript/mds-napi` |
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

Clippy warnings are treated as errors — keep the build warning-free.

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

Backend parity matters — when touching the JS bindings, run the `@mdscript/mds`
suite under both backends:

```bash
MDS_BACKEND=native npm test -w @mdscript/mds
MDS_BACKEND=wasm   npm test -w @mdscript/mds
```

## Pull requests

- **Conventional Commits** — PR titles and commits follow
  [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`,
  `refactor:`, `chore:`, `docs:`, …).
- **Update the CHANGELOG** — add user-facing changes under `## [Unreleased]` in
  `CHANGELOG.md`.
- **Tests** — add or update tests for behavior changes; assert outcomes, not
  implementation details.
- **No regressions** — every existing test must still pass.

## Security

Please report vulnerabilities privately — see [SECURITY.md](./SECURITY.md). Do not
open public issues for security problems.

## Code of Conduct

This project follows the [Contributor Covenant](./CODE_OF_CONDUCT.md). By
participating, you agree to uphold it.
