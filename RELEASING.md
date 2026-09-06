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

   **PyPI OIDC probe:** The `version-gate` job also probes the PyPI trusted
   publisher by performing the OIDC mint-token exchange (`pypi.org/_/oidc/mint-token`)
   before any irreversible publish. A missing, expired, or mismatched trusted-publisher
   record causes version-gate to fail, aborting the release before any crates.io or
   npm publish runs. The minted token expires unused — the probe is free and safe.
   This step is NOT tag-guarded so it runs in the `workflow_dispatch` dry run too,
   exercising the PyPI trust chain before the real tag push (PF-039). It is
   run on `pull_request` events too — the version-gate step fails closed on
   fork and Dependabot PRs (they receive no `id-token: write` and no
   repository secrets), and no PR run can reach a publish in any case.

6. **Configure TestPyPI trusted publisher** (optional, needed for `testpypi: true`
   dispatch runs) at [test.pypi.org/manage/account/publishing](https://test.pypi.org/manage/account/publishing/):
   - Project name: `markdown-script`
   - Owner / repository: `dean0x/mdscript`
   - Workflow filename: `release.yml`
   - Environment name: **leave blank**

   The `publish-testpypi` job is a dispatch-input-guarded opt-in leg (`testpypi: true`
   on `workflow_dispatch`). It is skipped on all PR and standard dispatch runs;
   `TIER_B_EXPECTED_SKIPPED` lists its name so the pre-merge verifier tolerates the
   skipped conclusion. The trusted publisher for TestPyPI is independent of the PyPI
   one — both must be configured separately.

   To trigger the first upload and lock the TestPyPI name, run:
   `gh workflow run release.yml --ref <branch> -f testpypi=true`
   A boolean dispatch input cannot be set without `-f`; omitting it leaves `testpypi`
   at its default (`false`) and `publish-testpypi` is skipped.

   **Publisher expiry:** a PyPI pending publisher auto-expires ~30 days after
   creation unless an upload lands. For TestPyPI, the first `workflow_dispatch`
   run with `testpypi: true` is the first upload that locks the name. If the
   pending publisher is missing or expired, the `Publish to TestPyPI (rehearsal)`
   job fails at the OIDC token exchange with an `invalid-publisher` message —
   re-file the pending publisher (environment name BLANK) and re-dispatch; no
   code change needed.

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
                                     # artifacts. Rehearses the publish-python
                                     # step. Publishes NOTHING.
```

The dry-run workflow runs `version-gate` in full, which now includes the
**credential probe** (security-08): it calls `npm whoami` against the live
registry to verify the `NPM_TOKEN` is valid, guards `CARGO_REGISTRY_TOKEN`
for non-empty, and probes the PyPI trusted publisher via the OIDC mint-token
exchange. A revoked token, absent secret, or misconfigured trusted publisher
therefore fails the dry run — all before any irreversible crates.io release.

Both probes **run on every event, including `pull_request`**. On fork and
Dependabot PRs — which receive no repository secrets and no `id-token:
write` — the probes fail closed with an actionable error: maintainers must
supersede with a first-party branch PR or dispatch `gh workflow run
release.yml --ref <branch>`. No PR run can reach a publish in any case,
so the fail-closed behaviour is informational, not a merge blocker by itself.

**Note:** `npm whoami` verifies authentication, not publish rights to the
`@mdscript` scope. A read-only or wrongly-scoped token passes the probe but
fails at publish time.

Even though release-surface PRs now trigger `release.yml` automatically, a
manual `gh workflow run release.yml --ref <branch>` is still required in four
cases (the five release-surface paths are `.github/workflows/release.yml`,
`.github/actions/**`, `crates/mds-napi/**`, `crates/mds-python/**`, and
`scripts/verify-napi-names.mjs`):

1. **CI-history gate (PF-017)** — the gate is step-skipped on `pull_request`
   because `github.sha` is the ephemeral merge commit, not the branch head; a
   `::notice::` makes the skip visible. It runs only on tag push and dispatch.
2. **Changes outside the release surface** — dependency sweeps,
   `crates/mds-core/**`, `Cargo.toml`, and `package.json` are not in the five
   paths above and do not trigger a `pull_request` run on `release.yml`.
3. **Dependabot and fork PRs** — no repository secrets and no `id-token: write`;
   `Version gate` fails closed with the "No Actions secrets on this run" error.
   Do not merge on those checks; supersede with a maintainer-authored PR or
   dispatch by hand.
4. **TestPyPI handshake** — `gh workflow run release.yml --ref <branch> -f testpypi=true`
   (once per version; `skip-existing: true` makes repeats no-ops).

The dry run also exercises the **CI-history gate** (PF-017), asserting a
completed+success `CI` run for the dispatched ref's HEAD. Dispatch it only after
that ref's CI has finished, or the gate fails closed on a still-running run. The
gate is skipped on `pull_request` runs by a step-level guard (the sibling notice
step makes the skip visible) and is enforced unchanged on tag push and
`workflow_dispatch`.

### PyPI publish rehearsal (#350, PF-039)

The dry run also runs the **`rehearse-publish-python` job** (`Rehearse PyPI
publish (no upload)`), which rehearses everything about `publish-python` except
the one irreversible act. It proves four properties, each with a positive
control (PF-013 — a gate never observed rejecting anything is not evidence):

1. **Pin shape** — the `pypa/gh-action-pypi-publish` pin must be a `vX.Y.Z`
   release tag. Control: the image-backed commit `dc37677b...` and the annotated
   tag object `a892a5a6...` are both rejected — policy, not existence.
2. **GHCR manifest** — GHCR must hold an image for that exact ref (HTTP 200).
   Control: a ref that cannot exist must return 404 with `MANIFEST_UNKNOWN`.
3. **`docker pull`** — the image must actually pull. Control: a missing tag must
   fail; bounded to 3 attempts. ("The ref resolves in git" is necessary and
   never sufficient — PF-040.)
4. **`twine check`** — the image's own twine 7.0.0 runs against all 8
   distributions with `--network none` and `--entrypoint twine`, so the step
   physically cannot reach pypi.org. Control: a deliberately corrupt wheel must
   be rejected.

> The job **must never** `uses: pypa/gh-action-pypi-publish`. The action has no
> dry-run/no-upload mode: an unrecognised `dry-run:` input is warned about and
> ignored, and the action then uploads for real. A `dry-run: true` rehearsal
> shipped briefly and attempted a live pypi.org upload from a pull request
> (run 34060146952); it failed only because that version was already published.
> Spec S14 in `scripts/__test__/release-auth-probe.spec.mjs` now pins the
> invocation to `publish-python` and `publish-testpypi` only, and the rehearsal
> is denied `id-token` so it holds no credential to upload with.

The rehearsal proves everything listed above; it cannot prove the upload handshake
and trusted-publisher exchange at publish time — the action has no dry-run mode.
The credential half is covered by `version-gate`'s OIDC probe (which runs on
every event including PRs); the upload half is closed by the opt-in TestPyPI leg
(`-f testpypi=true`), which exercises the full exchange once per version.

`publish-crates` needs this job, so a broken pin aborts the release before the
irreversible crates.io write. It is intentionally unguarded, so it runs on
`pull_request` and `workflow_dispatch`, not just tag pushes (PF-039).

Five jobs are expected-skipped on a standard `workflow_dispatch` dry run and
are listed in `TIER_B_EXPECTED_SKIPPED` in `scripts/verify-pr-checks.mjs`:
`Publish to crates.io`, `Publish to npm`, `Publish to PyPI`, `GitHub Release`,
and `Publish to TestPyPI (rehearsal)`. The same five are skipped on a
release-surface PR run.

Confirm the **A3 name-gate** step (`scripts/verify-napi-names.mjs`) passes in that
run. **This is a hard checkpoint** — if the generated platform package names or
their `.node` filenames drift from the hand-written `crates/mds-napi/index.js`
loader, the published universal package will fail to load the native binary at
runtime on the affected platform. Do not proceed past a failing gate.

### Release-surface PRs

Release-surface PRs — those touching `.github/workflows/release.yml`,
`.github/actions/**`, `crates/mds-napi/**`, `crates/mds-python/**`, or
`scripts/verify-napi-names.mjs` — also trigger the workflow via the
`pull_request` event, so a Dependabot bump to an action reachable only from a
tag-guarded job is exercised on the PR instead of first running on a tag push
after crates.io has published (PF-039).

On such PRs, `verify-pr-checks.mjs` requires three additional check-runs:
`Version gate`, `Stage + verify platform packages`, and `Rehearse PyPI publish
(no upload)`. All other publish jobs are skipped, and their skipped conclusions
are tolerated by the verifier.

That path list lives in **two** places that must stay identical: the
`on.pull_request.paths` filter in `release.yml` and `RELEASE_SURFACE` in
`scripts/verify-pr-checks.mjs`. Spec S10 compares them as sets — a filter the
verifier does not know about would let a release-surface PR pass as a silent
no-run (ADR-013 amendment). The verifier also fails closed (exit 2) if it
cannot enumerate the PR's changed files at all.

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
   1. **version-gate** — synchronized-version check, credential probe, PyPI OIDC
      probe, source-hygiene gate, and CI-history gate (fails fast).
   2. **build-napi** (parallel with build-python) — cross-compiles the addon for
      all 7 targets.
   3. **build-python** (parallel with build-napi) — builds `cp311-abi3` wheels
      for 7 platforms + sdist, runs the readelf linkage gate on Linux legs.
   4. **stage-and-verify-napi** — `napi create-npm-dirs` + `artifacts`, copies
      LICENSE into each platform dir, runs the **A3 name-gate**.
   5. **rehearse-publish-python** — pin shape, GHCR manifest, `docker pull` and
      `twine check` (each with a positive control); uploads nothing and holds no
      OIDC token. publish-crates blocks on this so a broken action pin aborts
      before crates.io (irreversible).
   6. **publish-crates** — blocked until `stage-and-verify-napi`, `build-python`,
      AND `rehearse-publish-python` succeed. `cargo publish` `mds-core`, polls the
      crates.io index for up to 5 min (bounded, max 20 × 15 s), then `mds-cli`.
   7. **publish-npm** and **publish-python** (parallel, both after publish-crates)
      — publish npm packages (with provenance) and PyPI `markdown-script` (OIDC
      trusted publishing + PEP 740 attestations, `skip-existing: true`).
   8. **github-release** — `gh release create` with generated notes; runs only
      after all three publish jobs succeed.

   `publish-testpypi` never runs on a tag: it is guarded by `inputs.testpypi`,
   which only a `workflow_dispatch` can set. On a tag push it reports `skipped`.

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
