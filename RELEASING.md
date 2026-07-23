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

## Release

### Tag-push (current working path)

The release is driven by pushing a `vX.Y.Z` tag. This is how v0.1.0–v0.3.0 shipped.

1. **Bump versions:** `node scripts/bump-version.mjs X.Y.Z` (updates all
   manifests and stamps the CHANGELOG, opening a fresh `[Unreleased]`).
2. **Land the bump on `main`:** open a PR (CI-gated). `main` is protected and the
   sole code-owner can't self-approve, so the merge needs an admin override
   (`enforce_admins=false` permits it). Squash-merge to keep linear history.
3. **Tag the merged commit and push:**
   ```bash
   git tag -a vX.Y.Z -m vX.Y.Z
   git push origin vX.Y.Z
   ```
   The tag push triggers `release.yml`; the `prepare` job is skipped and the
   build+publish jobs run from the tag.

### Automated `workflow_dispatch` — currently BLOCKED (#127)

```bash
gh workflow run release.yml -f version=X.Y.Z   # DOES NOT WORK YET — see #127
```

Intended to do everything in one command, but its `prepare` job pushes the release
commit directly to protected `main`, which branch protection rejects for the Actions
bot (`GH006`). It leaves an orphaned tag and publishes nothing. Use tag-push until
#127 is fixed.

See @RELEASING.md for the full runbook.

## Notes

- The 7 native targets: aarch64-apple-darwin, x86_64-apple-darwin, x86_64-unknown-linux-gnu, x86_64-unknown-linux-musl, aarch64-unknown-linux-gnu, aarch64-unknown-linux-musl, x86_64-pc-windows-msvc. Linux musl/arm builds use napi's --use-napi-cross.
- wasm-opt = ["-Oz", "--enable-bulk-memory", "--enable-sign-ext", ...] is enabled in crates/mds-wasm/Cargo.toml; CI installs wasm-pack and Binaryen v129 via the composite action at .github/actions/setup-wasm/ (version pins live there). Local builds need Binaryen separately (brew install binaryen / apt install binaryen).
- Platform packages are generated in CI only — they cannot be validated with a local npm pack; use the dry-run workflow instead.
- Due to its temp-file-then-rename implementation, atomic_write_file does not preserve hard links, ACLs, extended attributes (xattrs), or owner/group metadata of the original file.