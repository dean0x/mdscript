# Release Flow — MDS

## Overview

Single coordinated release: all crates and npm packages ship together at the same version.
Triggered by pushing the `vX.Y.Z` tag; `workflow_dispatch` is the build-only dry-run that
publishes nothing.

## Packages

- **Crates**: `mds-core`, `mds-cli` (published to crates.io in dependency order)
- **npm**: `@mdscript/mds-napi` (7 native targets), `@mdscript/mds-wasm`, `@mdscript/mds`,
  `@mdscript/bundler-utils`, `@mdscript/vite-plugin`, `@mdscript/rollup-plugin`, `@mdscript/webpack-loader`

## Version Strategy

- All packages share the same semver version
- Version files: `Cargo.toml` (workspace), 7 `package.json` files
- Bump tool: `node scripts/bump-version.mjs <version>` — rewrites manifests and CHANGELOG
  only; does NOT refresh `Cargo.lock` or `package-lock.json`, and never touches `.rs` files
  (so `#[deprecated(since = ...)]` attributes must be checked by hand via
  `grep -rn 'since = ' crates/ --include='*.rs'`)
- After bumping: run `cargo update -w` and `npm install --package-lock-only --ignore-scripts`,
  then `npm ci` to confirm the lock is consistent; commit `Cargo.lock` and `package-lock.json`
  with the bump
- Consistency gate: `node scripts/verify-versions.mjs`

## Pre-release Checks

1. Clean working directory (untracked `.devflow/` OK)
2. Rust tests: `cargo nextest run --workspace` PLUS `cargo test --workspace --doc` separately
   (nextest skips doctests)
3. Rustdoc gate: `RUSTDOCFLAGS="-D warnings" cargo doc -p mds-core --no-deps`
4. Format + lint: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`
5. Python bindings: activate a 3.11+ venv, run `maturin develop -m crates/mds-python/Cargo.toml`,
   then `pytest crates/mds-python/tests -q`
6. JS build + test: `npm ci && npm run build -w @mdscript/mds-wasm && npm run build --workspaces --if-present && npm test --workspaces --if-present`
7. Source hygiene + gates: `node scripts/verify-no-control-bytes.mjs && npm run test:gates`
8. Deprecated attribute check: `grep -rn 'since = ' crates/ --include='*.rs'` — every hit must
   have version <= X.Y.Z; deprecations introduced in this release must equal X.Y.Z exactly
9. Version consistency: `node scripts/verify-versions.mjs`
10. Packaging spot-check: `npm pack --dry-run -w @mdscript/mds && npm pack --dry-run -w @mdscript/mds-wasm && npm pack --dry-run -w @mdscript/mds-napi`
11. Tag does not already exist
12. Branch dry-run (mandatory when `release.yml` changes): `gh workflow run release.yml` — runs
    version-gate, 7-target build, A3 gate, and credential probe; all publish jobs are skipped.
    **Limitation**: ref-guarded steps (tag-only publish gates) are structurally un-exercisable
    from a branch dispatch — four clean dry-runs still let a tag-only gate fail on the first
    real tag. For musl artifact verification: download `bindings-*-linux-musl` artifacts and
    confirm no `GLIBC_`/`libc.so.6`/`ld-linux` strings; use `bindings-x86_64-unknown-linux-gnu`
    as the positive control (it must show those strings).

## Changelog

- Format: Keep a Changelog
- Location: `CHANGELOG.md`
- Stamping: `bump-version.mjs` converts `[Unreleased]` to `[X.Y.Z] — YYYY-MM-DD`
- Manual step: ensure `[Unreleased]` section is populated before release

## Build & Test

- CI handles all builds (7 native targets + WASM)
- Local pre-flight validates correctness only
- WASM: wasm-pack auto-downloads wasm-opt (`-Oz`); system Binaryen v129+ only for offline builds or to reproduce CI's exact release optimizer

## Publish

- **Trigger (tag-push — the only path)**: Land version bump on `main` via CI-gated PR. Before
  merging, run `node scripts/verify-pr-checks.mjs <pr-number>` — it tolerates the three
  tag-guarded publish jobs appearing as `skipped` on a branch dry-run head; any other conclusion
  fails. Wait for the `CI` workflow run on the merge commit to finish green before tagging — the
  release version-gate asserts a completed+success CI run for the tagged SHA and fails closed
  while CI is still running. Then:
  `git tag -a vX.Y.Z -m vX.Y.Z && git push origin vX.Y.Z`.
  The tag fires `release.yml`; build+publish run from the tag.
- **Dry run**: `gh workflow run release.yml` (no version input) — runs version-gate, 7-target
  build, A3 gate, and credential probe; publishes nothing. **Limitation**: ref-guarded steps
  (tag-only publish gates) are structurally un-exercisable from a branch dispatch — four clean
  dry-runs still let a tag-only gate fail on the first real tag.
- **Recovery**: Publishes are idempotent (crates.io matches "already uploaded/exists"; npm
  pre-checks `npm view <pkg>@<ver>` and tolerates E403 on `napi prepublish`). A partial failure
  is recoverable with `gh run rerun --failed <run-id>`. Never re-tag.
- **Known gap (issue #345)**: The version-gate credential probe runs `npm whoami` for npm but
  only checks `CARGO_REGISTRY_TOKEN` for non-emptiness, not capability — a dead crates.io token
  is only discovered after builds have run.
- **Flow (tag-push)**: version-gate → build-napi (7 targets) → stage+verify → publish-crates →
  publish-npm → github-release
- **Critical gate**: A3 name↔loader verification (`scripts/verify-napi-names.mjs`)
- **Toolchain pins**: Unpinned inputs that have broken release builds: stable rustc (1.98.0 added
  `-Wl,--fix-cortex-a53-843419`; aarch64-musl wrapper filters it) and zig (pinned to 0.16.0 in
  `release.yml`). See #339 for the durable fix.

## Post-release

1. Verify packages on registries (crates.io, npmjs.com)
2. Check provenance attestation on npm
3. Smoke test: `npm i @mdscript/mds && node -e "import('@mdscript/mds').then(m=>m.init())"`
4. musl artifact check: `npm pack @mdscript/mds-napi-linux-x64-musl@X.Y.Z` and confirm the `.node`
   file has no glibc markers (`GLIBC_`/`libc.so.6`/`ld-linux` absent); `0.1.0`–`0.3.0` shipped
   glibc-linked; `release.yml` now gates this
5. Close the milestone; check PyPI pending-publisher expiry if one is outstanding (#292)
6. CHANGELOG: `[Unreleased]` section is auto-created by bump script

## Tag Format

`v{VERSION}` (e.g., `v0.2.0`)
