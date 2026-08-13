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
  `@mdscript/rollup-plugin`, `@mdscript/webpack-loader`, `@mdscript/rspack-loader`
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

# Source hygiene and pre-merge check gates
node scripts/verify-no-control-bytes.mjs
npm run test:gates                           # positive-control spec suite
# Before any --admin merge (PF-017 guard — cancelled runs read as green):
node scripts/verify-pr-checks.mjs <pr-number>

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

## Release

### Tag-push (the only path)

The release is driven by pushing a `vX.Y.Z` tag. This is how all versions have shipped.

1. **Bump versions:** `node scripts/bump-version.mjs X.Y.Z` (updates all
   manifests and stamps the CHANGELOG, opening a fresh `[Unreleased]`).
2. **Land the bump on `main`:** open a PR (CI-gated). Once CI is green, run the
   pre-merge check verifier before merging — a cancelled run reads as green under
   `--admin` (PF-017):
   ```bash
   node scripts/verify-pr-checks.mjs <pr-number>
   ```
   On exit 0 the script prints the exact merge command — copy and run it verbatim:
   ```bash
   gh pr merge --squash --match-head-commit <headSha>
   ```
   (`main` is protected; the sole code-owner can't self-approve so `--admin` is
   required. `--match-head-commit` closes the TOCTOU window between verification
   and merge.)
3. **Tag the merged commit and push:**
   ```bash
   git tag -a vX.Y.Z -m vX.Y.Z
   git push origin vX.Y.Z
   ```
   The tag push triggers `release.yml`; the build+publish jobs run from the tag.

### What happens after tagging

The `release.yml` workflow runs, in order:
   1. **version-gate** — synchronized-version check (fails fast).
   2. **build-napi** — cross-compiles the addon for all 7 targets.
   3. **stage-and-verify-napi** — `napi create-npm-dirs` + `artifacts`, copies
      LICENSE into each platform dir, runs the **A3 name-gate**.
   4. **publish-crates** — `cargo publish` `mds-core`, polls the crates.io index
      for up to 5 min (bounded, max 20 × 15 s), then `mds-cli`.
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

- The 7 native targets: aarch64-apple-darwin, x86_64-apple-darwin, x86_64-unknown-linux-gnu, x86_64-unknown-linux-musl, aarch64-unknown-linux-gnu, aarch64-unknown-linux-musl, x86_64-pc-windows-msvc. Linux musl/arm builds use napi's --use-napi-cross.
- wasm-opt = ["-Oz", "--enable-bulk-memory", "--enable-sign-ext", ...] is enabled in crates/mds-wasm/Cargo.toml; CI installs wasm-pack and Binaryen v129 via the composite action at .github/actions/setup-wasm/ (version pins live there). Local builds need Binaryen separately (brew install binaryen / apt install binaryen).
- Platform packages are generated in CI only — they cannot be validated with a local npm pack; use the dry-run workflow instead.
- Due to its temp-file-then-rename implementation, atomic_write_file does not preserve hard links, ACLs, extended attributes (xattrs), or owner/group metadata of the original file.