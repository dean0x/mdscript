# Release Flow — MDS

## Overview

Single coordinated release: all crates and npm packages ship together at the same version.
Fully automated via GitHub Actions `release.yml` workflow dispatch.

## Packages

- **Crates**: `mds-core`, `mds-cli` (published to crates.io in dependency order)
- **npm**: `@mdscript/mds-napi` (7 native targets), `@mdscript/mds-wasm`, `@mdscript/mds`,
  `@mdscript/bundler-utils`, `@mdscript/vite-plugin`, `@mdscript/rollup-plugin`, `@mdscript/webpack-loader`

## Version Strategy

- All packages share the same semver version
- Version files: `Cargo.toml` (workspace), 7 `package.json` files
- Bump tool: `node scripts/bump-version.mjs <version>`
- Consistency gate: `node scripts/verify-versions.mjs`

## Pre-release Checks

1. Clean working directory (untracked `.devflow/` OK)
2. All Rust tests pass: `cargo test --workspace`
3. Format + lint: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`
4. JS build + test: `npm ci && npm run build -w @mdscript/mds-wasm && npm run build --workspaces --if-present && npm test --workspaces --if-present`
5. Version consistency: `node scripts/verify-versions.mjs`
6. Tag does not already exist

## Changelog

- Format: Keep a Changelog
- Location: `CHANGELOG.md`
- Stamping: `bump-version.mjs` converts `[Unreleased]` to `[X.Y.Z] — YYYY-MM-DD`
- Manual step: ensure `[Unreleased]` section is populated before release

## Build & Test

- CI handles all builds (7 native targets + WASM)
- Local pre-flight validates correctness only
- WASM requires Binaryen v129+ (`wasm-opt -Oz`)

## Publish

- **Trigger (working path — tag-push)**: bump on `main` via PR, then
  `git tag -a vX.Y.Z -m vX.Y.Z && git push origin vX.Y.Z`. The tag fires
  `release.yml`; `prepare` is skipped and build+publish run from the tag.
- **Dry run**: `gh workflow run release.yml` (no version input) — build + A3 gate,
  publishes nothing.
- **BLOCKED**: `gh workflow run release.yml -f version=X.Y.Z` fails GH006 (prepare
  can't push to protected `main`) — see #127. Do not rely on it yet.
- **Flow (tag-push)**: version-gate → build-napi (7 targets) → stage+verify →
  publish-crates → publish-npm → github-release (prepare skipped)
- **Critical gate**: A3 name↔loader verification (`scripts/verify-napi-names.mjs`)

## Post-release

1. Verify packages on registries (crates.io, npmjs.com)
2. Check provenance attestation on npm
3. Smoke test: `npm i @mdscript/mds && node -e "import('@mdscript/mds').then(m=>m.init())"`
4. CHANGELOG: `[Unreleased]` section is auto-created by bump script

## Tag Format

`v{VERSION}` (e.g., `v0.2.0`)
