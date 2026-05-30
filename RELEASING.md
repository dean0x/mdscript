# Releasing MDS

MDS ships as a **single coordinated release**: both crates and all npm packages go
out together at the same version. This document is the ordered runbook.

> The release is **deliberately a manual, triggered step.** Pushing a `v*` tag is
> what starts it. Until then, nothing publishes.

## Versions that must match

The [version-consistency gate](scripts/verify-versions.mjs) (run in CI and locally)
asserts these are all equal before anything publishes:

- Workspace crate version — `Cargo.toml` `[workspace.package] version` (covers
  `mds-core`, `mds-cli`, `mds-wasm`, `mds-napi`)
- Every publishable `package.json`: `@mdscript/mds-napi`, `@mdscript/mds`,
  `@mdscript/mds-wasm`, `@mdscript/bundler-utils`, `@mdscript/vite-plugin`,
  `@mdscript/rollup-plugin`, `@mdscript/webpack-loader`
- All internal `@mdscript/*` dependency ranges are `^<version>` (no `file:`)

## One-time prerequisites (maintainer / repo owner)

These are **not** automated and must be done before the first release:

1. **Register the `@mdscript` npm organization** (or scope) so the scoped packages
   can be published.
2. **Configure npm publish auth** — either:
   - npm **trusted publisher / OIDC** for this repo's `release.yml` (preferred; no
     long-lived token), or
   - add an `NPM_TOKEN` repo secret with publish rights to `@mdscript/*`.
   Provenance requires the `id-token: write` permission (already set on the
   publish job) plus publishing from GitHub Actions.
3. **Add the `CARGO_REGISTRY_TOKEN` repo secret** with publish rights to
   `mds-core` and `mds-cli` on crates.io.
4. **Enable GitHub private vulnerability reporting** (Settings → Code security →
   Private vulnerability reporting) so the SECURITY.md flow works.
5. Add **`CODE_OF_CONDUCT.md`** (tracked in #38) if not already present.

## Pre-flight (before tagging)

Run the local dry-runs and gates:

```bash
# Rust
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo publish -p mds-core --dry-run
# NOTE: `cargo publish -p mds-cli --dry-run` fails locally with
# "no matching package named `mds-core` found" until mds-core is on crates.io —
# mds-cli has a path+version dep on it. This is expected; the release workflow
# publishes mds-core first (and waits for the index), then mds-cli.

# JS
npm ci
npm run build -w @mdscript/mds-wasm
npm run build --workspaces --if-present
npm test --workspaces --if-present
node scripts/verify-versions.mjs

# Packaging spot-check (inspect tarball contents)
npm pack -w @mdscript/mds --dry-run
npm pack -w @mdscript/mds-wasm --dry-run
npm pack -w @mdscript/mds-napi --dry-run
```

Then validate the **risky cross-compile + platform packaging** without publishing,
via the dry-run workflow:

```bash
gh workflow run release.yml          # workflow_dispatch — builds the 7-target
                                     # napi matrix, stages platform packages,
                                     # runs the A3 name<->loader gate, uploads
                                     # artifacts. Publishes NOTHING.
```

Confirm the **A3 name-gate** step (`scripts/verify-napi-names.mjs`) passes in that
run. **This is a hard checkpoint** — if the generated platform package names or
their `.node` filenames drift from the hand-written `crates/mds-napi/index.js`
loader, the published universal package will fail to load the native binary at
runtime on the affected platform. Do not proceed past a failing gate.

## Release (ordered)

1. **Stamp the CHANGELOG.** Replace `## [Unreleased]` with `## [X.Y.Z] — <date>`
   and update the link reference at the bottom. Commit on the release branch and
   merge to `main`.
2. **Tag and push:**
   ```bash
   git checkout main && git pull
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```
3. The `release.yml` workflow then runs, in order:
   1. **version-gate** — synchronized-version check (fails fast).
   2. **build-napi** — cross-compiles the addon for all 7 targets.
   3. **stage-and-verify-napi** — `napi create-npm-dirs` + `artifacts`, copies
      LICENSE into each platform dir, runs the **A3 name-gate**.
   4. **publish-crates** — `cargo publish` `mds-core`, wait for the index, then
      `mds-cli`.
   5. **publish-npm** — regenerate `index.d.ts`, re-run the A3 gate, then publish
      (with provenance): the **platform packages** (`napi prepublish`), the
      **host** `@mdscript/mds-napi`, **`@mdscript/mds-wasm`**, the **universal**
      `@mdscript/mds`, and the **bundler** packages.
   6. **github-release** — `gh release create` with generated notes.

## Post-release

- Verify each package on its registry (crates.io, npmjs.com) and that npm shows
  the **provenance** attestation.
- Smoke test a clean install on a fresh machine/container:
  `npm i @mdscript/mds` then `node -e "import('@mdscript/mds').then(m=>m.init())"`.
- Open a fresh `## [Unreleased]` section in `CHANGELOG.md`.

## Notes

- The 7 native targets: `aarch64-apple-darwin`, `x86_64-apple-darwin`,
  `x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`,
  `aarch64-unknown-linux-gnu`, `aarch64-unknown-linux-musl`,
  `x86_64-pc-windows-msvc`. Linux musl/arm builds use napi's `--use-napi-cross`.
- `wasm-opt` is currently disabled (`crates/mds-wasm/Cargo.toml`); re-enable once
  CI provides Binaryen to recover ~10–20% wasm size.
- Platform packages are generated **in CI only** — they cannot be validated with a
  local `npm pack`; use the dry-run workflow above instead.
