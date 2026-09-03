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
- The `markdown-script` Python wheel version — maturin stamps it dynamically from
  the Cargo workspace at build time. The gate asserts `pyproject.toml` names the
  package `markdown-script` (ADR-012) and keeps `"version"` in `dynamic[]`.

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
5. **Configure PyPI trusted publisher** for `markdown-script` at
   [pypi.org/manage/account/publishing](https://pypi.org/manage/account/publishing/):
   - Project name: `markdown-script`
   - Owner / repository: `dean0x/mdscript`
   - Workflow filename: `release.yml` (must match exactly)
   - Environment name: **leave blank** — the `publish-python` job has no
     `environment:` field; a named environment would cause PyPI to reject the
     OIDC token because the claim would not match the filed record.

   **Note:** A PyPI *pending publisher* is not a name reservation and blocks no
   one — it auto-expires ~30 days after creation unless a first upload actually
   lands (ADR-012 amendment). Only the first `pypa/gh-action-pypi-publish` run
   on a real tag push secures the name.

   **OIDC credential probe limitation:** The `version-gate` credential probe
   calls `npm whoami` to verify the npm token before any irreversible publish.
   PyPI OIDC has no equivalent long-lived token to probe; `pypi.org` issues the
   upload token just-in-time for the OIDC exchange at publish time. The probe
   therefore cannot cover PyPI — a misconfigured trusted publisher is only
   discovered when `publish-python` runs on a real tag push.

## Pre-flight (before tagging)

Run the local dry-runs and gates:

```bash
# Rust
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
# Rustdoc gate (mirrors the CI `rust` job). nextest, clippy, and `cargo test --doc`
# all miss broken private intra-doc links; only this command catches them.
RUSTDOCFLAGS="-D warnings" cargo doc -p mds-core --no-deps
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
# Verify #[deprecated(since = ...)] attributes match the release version.
# bump-version.mjs rewrites manifests and CHANGELOG only -- never .rs files.
# Every hit's version must be <= X.Y.Z. A deprecation introduced in THIS release
# must equal X.Y.Z; pre-existing ones keep their original version.
# PF-018: if the grep returns no hits, plant a temporary `since = "x.y.z"` in any
# .rs file, confirm the grep finds it, then remove it before proceeding.
grep -rn 'since = ' crates/ --include='*.rs'

# Source hygiene and pre-merge check gates
node scripts/verify-no-control-bytes.mjs
npm run test:gates                           # positive-control spec suite
# Before any --admin merge (PF-017 guard — cancelled runs read as green):
PR_NUMBER=NNN  # replace NNN with the bump PR number
node scripts/verify-pr-checks.mjs "$PR_NUMBER"
# Note: a branch dry-run's skipped publish jobs are tolerated by the verifier.

# Packaging spot-check (inspect tarball contents)
npm pack -w @mdscript/mds --dry-run
npm pack -w @mdscript/mds-wasm --dry-run
npm pack -w @mdscript/mds-napi --dry-run

# Python — local wheel build and install smoke (mirrors ci.yml's python-wheel job)
# Note: cross-platform Python wheels can only be verified in CI (manylinux/musl
# Docker containers are not reproduced locally). Use the branch dry-run below
# instead of trying to replicate the musl readelf gate locally. PF-036.
python -m venv .venv && . .venv/bin/activate
pip install "maturin==1.13.3" pytest
maturin build -m crates/mds-python/Cargo.toml --out dist
ls dist/ | grep -q 'cp311-abi3' || (echo "expected a cp311-abi3 wheel" && exit 1)
pip install --find-links dist --no-index markdown-script
python -c "import markdown_script as m; r = m.compile('Hello {{n}}!', vars={'n': 'CI'}); print('smoke ok:', r.output)"
```

Then validate the **risky cross-compile + platform packaging** without publishing,
via the dry-run workflow:

```bash
gh workflow run release.yml          # workflow_dispatch — builds the 7-target
                                     # napi matrix AND the 7-target + sdist
                                     # Python wheel matrix, stages packages,
                                     # runs the A3 name<->loader gate and the
                                     # Python readelf linkage gate, uploads
                                     # artifacts. Publishes NOTHING.
```

The dry-run workflow runs `version-gate` in full, which now includes the
**credential probe** (security-08): it calls `npm whoami` against the live
registry to verify the `NPM_TOKEN` is valid, and guards `CARGO_REGISTRY_TOKEN`
for non-empty. A revoked or absent token therefore fails the dry run — this
closes the former gap where a bad npm token was only discovered after
`cargo publish` had already made an irreversible crates.io release.

**Note:** `npm whoami` verifies authentication, not publish rights to the
`@mdscript` scope. A read-only or wrongly-scoped token passes the probe but
fails at publish time.

The dry run also exercises the **CI-history gate** (PF-017), asserting a
completed+success `CI` run for the dispatched ref's HEAD. Dispatch it only after
that ref's CI has finished, or the gate fails closed on a still-running run.

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
   gh pr merge --squash --admin --match-head-commit <headSha>
   ```
   (`--admin` is required because `main` is protected and the sole code-owner
   cannot self-approve. `--match-head-commit` closes the TOCTOU window between
   verification and merge. Both flags are emitted by the script — copy the
   printed command without modification.)
3. **Tag the merged commit and push:**
   Wait for the `CI` workflow run on the merge commit to finish green
   (`gh run list --commit <sha>` / `gh run watch <id>`): the release's
   version-gate asserts a completed+success CI run for the tagged SHA and fails
   closed while it is still running.
   ```bash
   git tag -a vX.Y.Z -m vX.Y.Z
   git push origin vX.Y.Z
   ```
   The tag push triggers `release.yml`; the build+publish jobs run from the tag.

### What happens after tagging

The `release.yml` workflow runs, in order:
   1. **version-gate** — synchronized-version check (fails fast).
   2. **build-napi** (parallel with build-python) — cross-compiles the addon for
      all 7 targets.
   3. **build-python** (parallel with build-napi) — builds `cp311-abi3` wheels
      for 7 platforms + sdist, runs the readelf linkage gate on Linux legs.
   4. **stage-and-verify-napi** — `napi create-npm-dirs` + `artifacts`, copies
      LICENSE into each platform dir, runs the **A3 name-gate**.
   5. **publish-crates** — blocked until BOTH `stage-and-verify-napi` and
      `build-python` succeed (so a Python build failure aborts before crates.io,
      which is irreversible — PF-023). `cargo publish` `mds-core`, polls the
      crates.io index for up to 5 min (bounded, max 20 × 15 s), then `mds-cli`.
   6. **publish-npm** and **publish-python** (parallel, both after publish-crates)
      — publish npm packages (with provenance) and PyPI `markdown-script` (OIDC
      trusted publishing + PEP 740 attestations, `skip-existing: true`).
   7. **github-release** — `gh release create` with generated notes; runs only
      after all three publish jobs succeed.

## Post-release

- Verify each package on its registry (crates.io, npmjs.com) and that npm shows
  the **provenance** attestation.
- Verify `markdown-script` on PyPI and that it shows the PEP 740 attestation.
- Smoke test a clean install on a fresh machine/container:
  - npm: `npm i @mdscript/mds` then `node -e "import('@mdscript/mds').then(m=>m.init())"`
  - Python: `pip install markdown-script` then
    `python -c "import markdown_script as m; print(m.compile('{{x}}', vars={'x':'ok'}).output)"`
- Open a fresh `## [Unreleased]` section in `CHANGELOG.md`.

## Notes

- The 7 native napi targets: aarch64-apple-darwin, x86_64-apple-darwin, x86_64-unknown-linux-gnu, x86_64-unknown-linux-musl, aarch64-unknown-linux-gnu, aarch64-unknown-linux-musl, x86_64-pc-windows-msvc. x86_64-gnu passes napi's --use-napi-cross; aarch64-gnu links with the apt cross gcc; both musl legs link with zig cc wrappers, and a release gate asserts each musl artifact links musl rather than glibc (see the build-napi matrix in release.yml). zig is pinned to 0.16.0 in release.yml's Install zig step; bump it deliberately, since zig cc's linker-arg allowlist changes between releases.
- The 8 Python artifacts (7 `cp311-abi3` wheels + 1 sdist): manylinux x86_64 and aarch64, musllinux_1_2 x86_64 and aarch64, macOS x86_64 and arm64, Windows x86_64, plus one source distribution. Built by `PyO3/maturin-action@v1.51.0` (maturin 1.13.3). The musl and manylinux legs run inside Docker containers that maturin-action manages; the readelf linkage gate asserts the `.so` inside each Linux wheel links the correct libc (musl or glibc), with a positive control and a non-vacuity guard (PF-038). Platform wheels cannot be built or validated locally — use the branch dry-run workflow instead.
- wasm-opt = ["-Oz", "--enable-bulk-memory", "--enable-sign-ext", ...] is enabled in crates/mds-wasm/Cargo.toml; CI installs wasm-pack and Binaryen v129 via the composite action at .github/actions/setup-wasm/ (version pins live there). Local builds do not need system Binaryen — wasm-pack auto-downloads wasm-opt (v117) on first use; install Binaryen v129+ (brew install binaryen / apt install binaryen) only for offline builds, to override a stale wasm-opt on PATH, or to reproduce CI's exact release optimizer.
- Platform packages are generated in CI only — they cannot be validated with a local npm pack; use the dry-run workflow instead.
- Due to its temp-file-then-rename implementation, atomic_write_file does not preserve hard links, ACLs, extended attributes (xattrs), or owner/group metadata of the original file.
