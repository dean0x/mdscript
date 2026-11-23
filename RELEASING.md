# Releasing MDS

MDS ships as a **single coordinated release**: both crates and all npm packages go
out together at the same version. This document is the ordered runbook.

> The release is **deliberately a manual, triggered step.** Pushing a `v*` tag is
> what starts it. Until then, nothing publishes.

## Versions that must match

The [version-consistency gate](scripts/verify-versions.mjs) (run in CI and locally)
asserts these are all equal before anything publishes:

- Workspace crate version — `Cargo.toml` `[workspace.package] version` (covers
  `mds-core`, `mds-cli`, `mds-wasm`, `mds-napi`, and `mds-python`)
- Workspace member versions in `Cargo.lock` and `package-lock.json`. The gate
  checks the members in `scripts/release-manifest.mjs`, not transitive versions.
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
   `mds-core` and `mds-cli` on crates.io. The workflow cannot probe this token
   (crates.io has no read-only endpoint that accepts a scoped token — see the
   credential probe under Pre-flight); verify it in the crates.io UI before
   tagging (PF-023).
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

   **PyPI OIDC probe:** `Credential probe` checks the PyPI trusted publisher
   before any publish. It calls `pypi.org/_/oidc/mint-token` on tag push and
   manual dispatch. A missing, expired, or mismatched record stops the release.
   The probe does not use the token it receives. A PR run skips this step.

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

Install the CI pin, `cargo-deny 0.20.2`, before the Rust checks (#136).
Run this block in Bash. Use Git Bash on Windows.
Use `curl`, `jq`, `tar`, and either `sha256sum` or `shasum`.
If any command fails, stop. Use the same shell for the checks below.

```bash
if cargo_deny_tmp=$(mktemp -d) && bash -s -- "$cargo_deny_tmp" <<'INSTALL_CARGO_DENY'
set -euo pipefail
work=$1
version=0.20.2
action_sha=5b4d68e2e660441203ab128a23676f1e4faf1532
binary=cargo-deny
case "$(uname -s):$(uname -m)" in
  Darwin:arm64)       platform=aarch64_macos;      target=aarch64-apple-darwin ;;
  Darwin:x86_64)      platform=x86_64_macos;       target=x86_64-apple-darwin ;;
  Linux:aarch64)      platform=aarch64_linux_musl; target=aarch64-unknown-linux-musl ;;
  Linux:x86_64)       platform=x86_64_linux_musl;  target=x86_64-unknown-linux-musl ;;
  MINGW*:x86_64|MSYS*:x86_64)
    platform=x86_64_windows; target=x86_64-pc-windows-msvc; binary=cargo-deny.exe ;;
  *) printf 'No pinned cargo-deny archive for this host\n' >&2; exit 1 ;;
esac
bundle="cargo-deny-$version-$target"
mkdir "$work/download" "$work/bin"
archive="$work/download/$bundle.tar.gz"
manifest="https://raw.githubusercontent.com/taiki-e/install-action/$action_sha/manifests/cargo-deny.json"
expected=$(curl -fsSL --proto '=https' --proto-redir '=https' "$manifest" |
  jq -er --arg v "$version" --arg p "$platform" \
    '.[$v][$p].hash | strings | select(test("^[0-9a-f]{64}$"))')
curl -fsSL --proto '=https' --proto-redir '=https' -o "$archive" \
  "https://github.com/EmbarkStudios/cargo-deny/releases/download/$version/$bundle.tar.gz"
if command -v sha256sum >/dev/null; then
  actual=$(sha256sum < "$archive")
else
  actual=$(shasum -a 256 < "$archive")
fi
if [ "${actual%% *}" != "$expected" ]; then
  printf 'cargo-deny archive SHA-256 mismatch\n' >&2
  exit 1
fi
tar -xzf "$archive" -C "$work/bin" --strip-components=1 "$bundle/$binary"
if ! "$work/bin/$binary" --version | cmp -s - <(printf 'cargo-deny %s\n' "$version"); then
  printf 'Expected cargo-deny %s\n' "$version" >&2
  exit 1
fi
INSTALL_CARGO_DENY
then
  export PATH="$cargo_deny_tmp/bin:${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
else
  printf 'Stop: cargo-deny install failed\n' >&2
  false
fi
```

The first `PATH` entry is the private directory that this block creates.
The explicit Cargo home `bin` entry stops Cargo from putting it first.
If you set `CARGO_HOME` in Git Bash, use a path in Git Bash format.

### Check NOTICE freshness (#136)

Install `cargo-about 0.9.1` from the pinned manifest.
Run this block in Bash. Use Git Bash on Windows.
If the manifest has no archive for your host, use a supported host.
If the install fails, stop.

```bash
if cargo_about_tmp=$(mktemp -d) && bash -s -- "$cargo_about_tmp" <<'INSTALL_CARGO_ABOUT'
set -euo pipefail
work=$1
version=0.9.1
action_sha=5b4d68e2e660441203ab128a23676f1e4faf1532
binary=cargo-about
case "$(uname -s):$(uname -m)" in
  Darwin:arm64)       platform=aarch64_macos;      target=aarch64-apple-darwin ;;
  Linux:aarch64)      platform=aarch64_linux_musl; target=aarch64-unknown-linux-musl ;;
  Linux:x86_64)       platform=x86_64_linux_musl;  target=x86_64-unknown-linux-musl ;;
  MINGW*:x86_64|MSYS*:x86_64)
    platform=x86_64_windows; target=x86_64-pc-windows-msvc; binary=cargo-about.exe ;;
  MINGW*:aarch64|MSYS*:aarch64|MINGW*:arm64|MSYS*:arm64)
    platform=aarch64_windows; target=aarch64-pc-windows-msvc; binary=cargo-about.exe ;;
  *) printf 'No pinned cargo-about archive for this host\n' >&2; exit 1 ;;
esac
bundle="cargo-about-$version-$target"
mkdir "$work/download" "$work/bin"
archive="$work/download/$bundle.tar.gz"
manifest="https://raw.githubusercontent.com/taiki-e/install-action/$action_sha/manifests/cargo-about.json"
expected=$(curl -fsSL --proto '=https' --proto-redir '=https' "$manifest" |
  jq -er --arg v "$version" --arg p "$platform" \
    '.[$v][$p].hash | strings | select(test("^[0-9a-f]{64}$"))')
curl -fsSL --proto '=https' --proto-redir '=https' -o "$archive" \
  "https://github.com/EmbarkStudios/cargo-about/releases/download/$version/$bundle.tar.gz"
if command -v sha256sum >/dev/null; then
  actual=$(sha256sum < "$archive")
else
  actual=$(shasum -a 256 < "$archive")
fi
if [ "${actual%% *}" != "$expected" ]; then
  printf 'cargo-about archive SHA-256 mismatch\n' >&2
  exit 1
fi
tar -xzf "$archive" -C "$work/bin" --strip-components=1 "$bundle/$binary"
"$work/bin/$binary" --version > "$work/version"
if ! cmp -s "$work/version" <(printf 'cargo-about 0.9.1\n'); then
  printf 'Expected cargo-about 0.9.1\n' >&2
  exit 1
fi
INSTALL_CARGO_ABOUT
then
  cargo_about="$cargo_about_tmp/bin/cargo-about"
  if [ ! -x "$cargo_about" ]; then cargo_about="$cargo_about.exe"; fi
else
  printf 'Stop: cargo-about install failed\n' >&2
  false
fi
```

In the same Bash shell, define `refresh_notice` from the repository root.
The command writes a temporary NOTICE from the locked graph.
Compare it with both copies before you tag.
If a copy differs, update both copies in the version bump pull request.

```bash
notice_raw="$cargo_about_tmp/notice.raw"
notice_out="$cargo_about_tmp/NOTICE"
refresh_notice() {
  cargo fetch --locked || return $?
  "$cargo_about" about generate --frozen --workspace --all-features --fail --target aarch64-apple-darwin --target x86_64-apple-darwin --target x86_64-unknown-linux-gnu --target x86_64-unknown-linux-musl --target aarch64-unknown-linux-gnu --target aarch64-unknown-linux-musl --target x86_64-pc-windows-msvc --target wasm32-unknown-unknown --config about.toml --output-file "$notice_raw" about.hbs && node scripts/generate-notice.mjs < "$notice_raw" > "$notice_out"
}
refresh_notice && cmp NOTICE "$notice_out" && cmp crates/mds-python/NOTICE "$notice_out"
```

Run the remaining local gates from the repository root:

```bash
# Rust
cargo metadata --locked --format-version 1 >/dev/null
cargo deny --locked --config deny.toml check advisories licenses bans sources
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
node scripts/check-wasm-tools.mjs --strict  # Match the WASM tool pins in CI
npm ci
npm run build -w @mdscript/mds-wasm
npm run build --workspaces --if-present
npm test --workspaces --if-present
node scripts/verify-versions.mjs
# Verify #[deprecated(since = ...)] attributes match the release version.
# bump-version.mjs updates manifests, lockfiles, and CHANGELOG, not Rust source.
# Every hit's version must be <= X.Y.Z. A deprecation introduced in THIS release
# must equal X.Y.Z; pre-existing ones keep their original version.
# PF-018: REQUIRED while the grep returns no hits (none since #304): plant a temporary
# `since = "x.y.z"` in any .rs file, confirm the grep finds it, then remove it.
grep -rn 'since = ' crates/ --include='*.rs'

# Source hygiene and pre-merge check gates
node scripts/verify-no-control-bytes.mjs
npm run test:gates                           # positive-control spec suite
# Before any --admin merge (PF-017 guard — cancelled runs read as green):
PR_NUMBER=NNN  # replace NNN with the bump PR number
node scripts/verify-pr-checks.mjs "$PR_NUMBER"
# Note: a branch dry-run's skipped publish jobs are tolerated by the verifier.

# Packaging spot-check (inspect tarball contents)
# The pack gate requires root LICENSE and NOTICE in @mdscript/mds-wasm (#136).
node scripts/verify-pack-contents.mjs
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
wheels=(dist/*-cp311-abi3-*.whl)
[[ ${#wheels[@]} -eq 1 && -f "${wheels[0]}" ]] || { echo 'Expected one cp311-abi3 wheel' >&2; exit 1; }
wheel=${wheels[0]##*/}
unzip -Z1 "${wheels[0]}" > "$cargo_about_tmp/wheel-files" &&
  grep -Fx -- "${wheel%-cp311-abi3-*}.dist-info/licenses/NOTICE" "$cargo_about_tmp/wheel-files"
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

The dry-run workflow runs **Credential probe** before `Version gate`.
What each registry check proves:

- **npm** (`npm whoami`): proves the token is accepted by the npm registry
  (authentication). Does NOT prove publish rights to the `@mdscript` scope —
  a read-only or wrongly-scoped token passes this check but fails at publish time.
- **PyPI**: the OIDC mint-token exchange (`pypi.org/_/oidc/mint-token`) proves the
  trusted-publisher record matches the workflow. The minted token expires unused —
  the probe is free and safe.
- **crates.io**: non-empty guard only. This is the strongest check the crates.io
  API allows for an API token: `GET /api/v1/me` is `AuthCheck::only_cookie()`
  (`src/controllers/user/me.rs:38-41`) and returns HTTP 403 for any token, scoped
  or unscoped (`src/auth.rs:136-144`). The only token-accepting read endpoint,
  `GET /api/v1/me/tokens/{id}` (`src/controllers/token.rs:269-282`), accepts
  legacy unscoped tokens only — a scoped token (the least-privilege kind a publish
  secret should be) is rejected there with HTTP 403 "this token does not have the
  required permissions". A well-formed but
  revoked/deleted token gets HTTP 403 "authentication failed" (`src/auth.rs:297-303`);
  a malformed token gets HTTP 401 "The given API token does not match the format
  used by crates.io" (`src/auth.rs:295`, `InsecurelyGeneratedTokenRevoked`).
  Every crates.io API request must also carry a `User-Agent` header — without one
  the `require_user_agent` middleware (`src/middleware/require_user_agent.rs:35-47`)
  returns HTTP 403 with a plaintext body before authentication runs, so a manual
  probe without that header fails for an unrelated reason. In
  practice this means a revoked crates.io token is first detected at the first
  `cargo publish` (fail-before-write, after the build matrix has been paid for).
  The v0.4.0 release experienced exactly this (run 33569514359 attempt 1 failed
  at `Publish mds-core` with HTTP 403, nothing published, `gh run rerun --failed`
  completed it; see PF-023). A durable fix — Trusted Publishing for crates.io —
  is tracked in #368; #345 is closed as won't-fix-as-filed with this finding.

The registry checks run on tag push and manual dispatch. On PR runs,
`Credential probe` reports a notice and passes. The npm and crates.io publish
steps receive only the registry inputs that they use.

`Version gate` rejects a PR that touches the release surface when the actor or
the PR author is `dependabot[bot]`, when the head repository is a fork, or when
the head repository is missing or is not this repository. A maintainer push to a
Dependabot PR is also rejected. Replace such a PR with a maintainer PR. A PR outside the release surface does not
require release checks.

Even though release-surface PRs now trigger `release.yml` automatically, a
manual `gh workflow run release.yml --ref <branch>` is still required in four
cases (the six release-surface paths are `.github/workflows/release.yml`,
`.github/actions/**`, `crates/mds-napi/**`, `crates/mds-python/**`,
`scripts/verify-napi-names.mjs`, and `scripts/musl-load-probe.cjs`):

1. **CI-history gate (PF-017)** — the gate is step-skipped on `pull_request`
   because `github.sha` is the ephemeral merge commit, not the branch head; a
   `::notice::` makes the skip visible. It runs only on tag push and dispatch.
2. **Changes outside the release surface** — dependency sweeps,
   `crates/mds-core/**`, `Cargo.toml`, and `package.json` are not in the six
   paths above and do not trigger a `pull_request` run on `release.yml`.
3. **Dependabot and fork PRs on the release surface** — `Version gate` reports
   "Release checks require a maintainer PR." Open a maintainer PR to replace it.
   Dispatch a dry run on the maintainer branch.
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
`Credential probe` checks the exchange on tag push and manual dispatch. The
opt-in TestPyPI leg (`-f testpypi=true`) tests the full upload once per version.

`publish-crates` needs this job, so a broken pin aborts the release before the
irreversible crates.io write. It is intentionally unguarded, so it runs on
`pull_request` and `workflow_dispatch`, not just tag pushes (PF-039).

Five jobs are expected-skipped on a standard `workflow_dispatch` dry run and
are listed in `TIER_B_EXPECTED_SKIPPED` in `scripts/verify-pr-checks.mjs`:
`Publish to crates.io`, `Publish to npm`, `Publish to PyPI`, `GitHub Release`,
and `Publish to TestPyPI (rehearsal)`. The same five are skipped on a
release-surface PR run. The skipped tolerance applies only to check-runs whose
`check_suite.id` maps (via `GET /actions/runs?head_sha=`) to a `release.yml`
check suite (any event); the three D-PR7 contexts (`Version gate`,
`Stage + verify platform packages`, `Rehearse PyPI publish (no upload)`) are
attributed by the same suite. The verifier exits 2 when it cannot enumerate the
head's workflow runs (D-PR8, #341).

Confirm the **A3 name-gate** step (`scripts/verify-napi-names.mjs`) passes in that
run. **This is a hard checkpoint** — if the generated platform package names or
their `.node` filenames drift from the hand-written `crates/mds-napi/index.js`
loader, the published universal package will fail to load the native binary at
runtime on the affected platform. Do not proceed past a failing gate.

### Release-surface PRs

Release-surface PRs — those touching `.github/workflows/release.yml`,
`.github/actions/**`, `crates/mds-napi/**`, `crates/mds-python/**`,
`scripts/verify-napi-names.mjs`, or `scripts/musl-load-probe.cjs` — also
trigger the workflow via the `pull_request` event, so a Dependabot bump to an
action reachable only from a tag-guarded job is exercised on the PR instead of
first running on a tag push after crates.io has published (PF-039).

On such PRs, `verify-pr-checks.mjs` requires three additional check-runs:
`Version gate`, `Stage + verify platform packages`, and `Rehearse PyPI publish
(no upload)`. All other publish jobs are skipped, and their skipped conclusions
are tolerated by the verifier.

That path list lives in **two** places that must stay identical: the
`on.pull_request.paths` filter in `release.yml` and `RELEASE_SURFACE` in
`scripts/verify-pr-checks.mjs`. Spec S10 compares them as sets — a filter the
verifier does not know about would let a release-surface PR pass as a silent
no-run (ADR-013 amendment). The verifier also fails closed (exit 2) if it
cannot enumerate the PR's changed files at all. The skipped-publish allowance
and D-PR7 context attribution are both keyed on the check-run's `release.yml`
check suite (D-PR8, #341 — any release.yml event counts, including `pull_request`
and `workflow_dispatch`); the verifier exits 2 when it cannot enumerate the
head's workflow runs.

The `Alpine load test (linux-arm64-musl)` job (`load-test-musl-arm64`) is an
unguarded Tier-B-binding check-run on release-surface PRs — it reaches
`conclusion=success` on every PR and dispatch run. It is deliberately NOT added
to `RELEASE_SURFACE_CONTEXTS` (the 2026-09 verifier fixtures predate it and
would fail if it appeared there); instead, spec S21 in
`scripts/__test__/release-auth-probe.spec.mjs` pins its existence, runner,
guard shape, wiring into `publish-crates`, and step order. It is not a
branch-protection required context (branch protection covers `ci.yml` jobs only;
`release.yml` jobs only run on release-surface PRs).

## Release

### Tag-push (the only path)

The release is driven by pushing a `vX.Y.Z` tag. This is how all versions have shipped.

1. **Bump versions:** Run `node scripts/bump-version.mjs X.Y.Z`. The script
   updates the release manifests and workspace member versions in both lockfiles.
   It stamps the CHANGELOG and opens a fresh `[Unreleased]` section.
   - **Refresh NOTICE:** Run the install block under [Check NOTICE freshness](#check-notice-freshness-136).
     Define `refresh_notice` as shown there. Run this block from the repository root.
     Include any changes to both NOTICE copies in the version bump pull request.

     ```bash
     refresh_notice && cp "$notice_out" NOTICE && cp "$notice_out" crates/mds-python/NOTICE
     ```
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
   (`gh run list --commit <sha>` / bounded polling with `gh run view <id> --json
   status,conclusion`): the release's version-gate asserts a completed+success CI
   run for the tagged SHA and fails closed while it is still running.
   ```bash
   git tag -a vX.Y.Z -m vX.Y.Z
   git push origin vX.Y.Z
   ```
   The tag push triggers `release.yml`; the build+publish jobs run from the tag.

### What happens after tagging

The `credential-probe` job checks the registry inputs first. After it passes,
`release.yml` runs these jobs:
   1. **version-gate** — checks release manifests, lockfile versions, source hygiene, and CI history.
      A read-only job regenerates NOTICE and compares both copies (#136).
   2. **build-napi** (parallel with build-python) — cross-compiles the addon for
      all 7 targets.
   3. **build-python** (parallel with build-napi) — builds `cp311-abi3` wheels for seven platforms and one sdist.
      It checks the Linux wheel linkage and the NOTICE entries in the sdist (#136).
   4. **stage-and-verify-napi** — stages the seven platform packages with `LICENSE` and `NOTICE` after `build-napi`.
      It checks each packed file list for `NOTICE` before the **A3 name-gate** (#136).
      The last step runs the x64 Alpine load test (`linux-x64-musl`) inside `node:22-alpine`
      after the staged artifact upload, so the artifact is preserved even when
      the x64 test fails; if x64 fails, the arm64 job is skipped and both are
      re-run together after the fix.
   5. **load-test-musl-arm64** — unguarded job on a native `ubuntu-24.04-arm`
      runner (no QEMU); downloads the `napi-staged` artifact; asserts the arm64
      ELF shape with a positive control; runs the arm64 Alpine load test
      (`linux-arm64-musl`) on `node:22-alpine` with a run block byte-identical
      to the x64 step. Skipped when x64 fails (`if: !cancelled() &&
      needs.stage-and-verify-napi.result == 'success'`); both are re-run
      together after a fix. `publish-crates` needs this job and requires
      `needs.load-test-musl-arm64.result == 'success'` in its `if:` (PF-047,
      PF-038, #340).
   6. **rehearse-publish-python** — pin shape, GHCR manifest, `docker pull` and
      `twine check` (each with a positive control); uploads nothing and holds no
      OIDC token. publish-crates blocks on this so a broken action pin aborts
      before crates.io (irreversible).
   7. **publish-crates** — blocked until `stage-and-verify-napi`,
      `load-test-musl-arm64`, `build-python`, AND `rehearse-publish-python`
      succeed. `cargo publish` `mds-core`, polls the crates.io index for up to
      5 min (bounded, max 20 × 15 s), then `mds-cli`.
   8. **publish-npm**, then **publish-python** (`publish-python` `needs` `publish-npm`).
      The npm job checks the restored platform NOTICE files before the A3 re-run (#136).
      It checks the workspace pack lists before the first npm publish.
      The workspace publish follows the platform publish with no step in between.
      The Python job publishes `markdown-script` with OIDC, PEP 740 attestations, and `skip-existing: true`.
   9. **github-release** — `gh release create` with generated notes; runs only
      after all three publish jobs succeed.

   `publish-testpypi` never runs on a tag: it is guarded by `inputs.testpypi`,
   which only a `workflow_dispatch` can set. On a tag push it reports `skipped`.

### When a tag-push release fails part-way

`release.yml` is a chain of gates followed by irreversible publishes. The name of
the failed job tells you what has already happened: nothing publishes before
`Publish to crates.io`, and inside that job nothing is irreversible until the
`Publish mds-core` step succeeds.

| Failed job | Already irreversible | Recovery |
|---|---|---|
| `Credential probe` — a registry check failed | Nothing | Correct the registry setup. Run `gh run rerun <run-id> --failed`. |
| `Version gate` — version mismatch, CI-history gate on a still-running CI run, source hygiene | Nothing | Fix the cause. CI still running or just finished: wait for it, then `gh run rerun <run-id> --failed`. Wrong commit tagged (version mismatch, hygiene): delete the tag (`git tag -d vX.Y.Z && git push origin :refs/tags/vX.Y.Z`), land the fix via PR, wait for its CI, re-tag. |
| `Version gate` — stale NOTICE (#136) | Nothing | Run `refresh_notice` from [Check NOTICE freshness](#check-notice-freshness-136). Copy the result to both NOTICE paths in a pull request. If no registry has the version, move the tag to the corrected commit after CI passes. |
| `Version gate` — cargo-about install or version check fails (#136) | Nothing | If the archive host failed temporarily, run `gh run rerun <run-id> --failed`. If the pin or archive is wrong, fix it in a pull request. Verify the local install and the release dry run. If no registry has the version, move the tag to the corrected commit after CI passes. |
| `build-napi`, `build-python`, `Stage + verify platform packages`, `Alpine load test (linux-arm64-musl)`, `Rehearse PyPI publish (no upload)` | Nothing | Transient (runner, Docker Hub pull, registry outage): `gh run rerun <run-id> --failed` — re-runs the failed jobs and their dependents; upstream artifacts are reused. Genuine defect: land the fix, bump to the next patch version, tag that. Do not move a tag that a defect was found on. |
| `Publish to crates.io` at `Publish mds-core` — the v0.4.0 case: HTTP 403 from a revoked token, run 33569514359 | Nothing | Fix the secret or cause, then `gh run rerun <run-id> --failed`. |
| `Publish to crates.io` after `mds-core` is live — index poll timeout, `Publish mds-cli` failure | `mds-core@X.Y.Z` on crates.io (cannot be deleted; `cargo yank` only hides it from new resolutions) | Transient: `gh run rerun <run-id> --failed` — the `mds-core` step treats "already uploaded" as success and proceeds to `mds-cli`. Defect in `mds-cli`: the version is consumed for the whole workspace (one coordinated version); land the fix, bump to the next patch, optionally `cargo yank --version X.Y.Z mds-core`, and say so in the GitHub Release body. |
| `Publish to npm` — any point | Both crates on crates.io. Zero or more `@mdscript/*` packages at X.Y.Z can be on npm. If a NOTICE or pack-list check fails, the run makes no new npm writes. | `gh run rerun <run-id> --failed`: `napi prepublish` tolerates E403 for platform packages already published and `publish_if_absent` skips versions `npm view` already sees. **Do not publish from a workstation** — a laptop `npm publish` carries no OIDC provenance attestation, and that gap is permanent for the version. If a code or workflow change is required for the re-run to succeed: bump to the next patch; npm unpublish is restricted by registry policy and leaves the version number consumed either way, so retire a partial set with `npm deprecate` rather than trying to remove it. |
| `Publish to PyPI` — the v0.4.1 case: `manifest unknown` on an annotated-tag-object pin (#350) | crates.io and the npm packages at X.Y.Z; `GitHub Release` was skipped | Transient: `gh run rerun <run-id> --failed` (`skip-existing: true` makes the upload re-run safe). Workflow or pin defect: per the tag-immutability rule above, the fix goes to a new commit rather than the tagged one, so land it and bump to the next patch (v0.4.2 was that release), then create the partial version's GitHub Release by hand so the tag is not left bare — `gh release create vX.Y.Z --title vX.Y.Z --generate-notes --latest=false` — with a body line naming which registries the version reached (v0.4.1 was backfilled this way on 2026-09-04). |
| `GitHub Release` | Everything is published | `gh run rerun <run-id> --failed`, or by hand: `gh release create vX.Y.Z --title vX.Y.Z --generate-notes` (the job itself falls back to `gh release edit`). |

Across rows:

- Recover with `gh run rerun <run-id> --failed`, not by re-pushing the tag while a
  run is in flight: the `release-<ref>` concurrency group has
  `cancel-in-progress: false`, a second push queues behind the first, and a
  cancelled run reads as not-failed to the pre-merge verifier.
- Moving a tag is acceptable only while nothing has been published. Once
  `mds-core` is on crates.io the version belongs to the artifacts built from the
  tagged commit; re-running later jobs against a different commit would publish npm
  or PyPI artifacts that do not match what crates.io holds. The next patch version
  is the recovery.
- Run the Post-release checks below per registry after a partial recovery — they are
  how you confirm the recovery reached every leg that had failed.

## Post-release

- Verify each package on its registry (crates.io, npmjs.com) and that npm shows
  the **provenance** attestation.
- Verify `markdown-script` on PyPI and that it shows the PEP 740 attestation.
- Smoke test a clean install on a fresh machine/container:
  - npm: `npm i @mdscript/mds` then `node -e "import('@mdscript/mds').then(m=>m.init())"`
  - Python: `pip install markdown-script` then
    `python -c "import markdown_script as m; print(m.compile('{{x}}', vars={'x':'ok'}).output)"`
- Confirm `## [Unreleased]` is empty — `bump-version.mjs` inserts the new version
  heading beneath it and leaves the heading in place.

## Branch protection

`main` requires every repo-owned `ci.yml` context to pass (ADR-013); third-party and
analysis contexts (CodeQL, Snyk) report but are deliberately not required, so a
vendor outage cannot block a merge. `enforce_admins` is `false` and must stay
`false`: the sole code owner cannot self-approve, so `--admin` merges (through the
command `scripts/verify-pr-checks.mjs` prints) are the only merge path.

### Adding a required CI context

A new `ci.yml` job cannot block a merge until branch protection lists it, so adding
one is a three-place change: the job, `EXPECTED_CONTEXTS` in
`scripts/verify-pr-checks.mjs`, and `main`'s required status checks. The worked
example is `Rust — clippy, test (windows-latest)` (#147), which takes protection
from 15 to 16 required contexts.

1. **In the PR that adds the job**, list the job's `name:` in `EXPECTED_CONTEXTS`
   (Tier A+) and raise the `EXPECTED_CONTEXTS.length` assertion in
   `scripts/__test__/verify-pr-checks.spec.mjs`. Tier A+ fails the verifier when
   the job is absent or not `completed+success` whether or not protection lists
   it yet, so the adding PR is already gated on its own job. Copy the name
   byte-for-byte from `ci.yml` (the dash is an em dash, U+2014), and confirm the
   job has no `paths:`/`paths-ignore:` filter and no job-level `if:` — a required
   context that never reports blocks every PR.
2. **Save the current protection** before changing it:
   ```bash
   gh api repos/dean0x/mdscript/branches/main/protection > protection-saved.json
   gh api repos/dean0x/mdscript/branches/main/protection/required_status_checks \
     --jq '{strict: .strict, checks: [.checks[] | {context, app_id}]}' > checks-saved.json
   ```
3. **PATCH only `required_status_checks`** — once the PR head is fully green,
   including the new job, and immediately before merging. Send the existing
   checks plus the new one, each with `app_id` 15368 (GitHub Actions), and keep
   `strict` as it is:
   ```bash
   gh api repos/dean0x/mdscript/branches/main/protection/required_status_checks \
     --jq '{strict: .strict, checks: ([.checks[] | {context, app_id}]
            + [{context: "Rust — clippy, test (windows-latest)", app_id: 15368}])}' \
     > checks-new.json
   gh api --method PATCH repos/dean0x/mdscript/branches/main/protection/required_status_checks \
     --input checks-new.json
   ```
   Never use the whole-protection `PUT repos/dean0x/mdscript/branches/main/protection`:
   it replaces the entire object and resets every field it is not sent — the
   pull-request review rules included. The scoped endpoint leaves `enforce_admins`
   untouched.
4. **GET-verify** the count, the names and `enforce_admins`:
   ```bash
   gh api repos/dean0x/mdscript/branches/main/protection/required_status_checks \
     --jq '(.checks | length), .strict, (.checks[].context)'
   gh api repos/dean0x/mdscript/branches/main/protection/enforce_admins --jq .enabled
   ```
   Expect the old count plus one (16 for #147), `strict` unchanged, the new name
   exactly as the job reports it, and `false`.
5. **Merge** as usual: `node scripts/verify-pr-checks.mjs <pr>` now lists the new
   context under Tier A; run the `gh pr merge` command it prints.

**Rollback**: PATCH the saved checks back and GET-verify the old count:

```bash
gh api --method PATCH repos/dean0x/mdscript/branches/main/protection/required_status_checks \
  --input checks-saved.json
```

Roll back whenever the merge slips after the PATCH. Until the job exists on `main`,
every open PR — Dependabot's included — runs the old `ci.yml`, never reports the new
context, and cannot merge. After the merge, an open PR still needs a rebase onto
the new `main` before its head runs the new job; Dependabot rebases its own PRs when
`main` moves (or comment `@dependabot rebase`).

### Dependency policy (cargo-deny)

The policy in [`deny.toml`](deny.toml) applies to the locked Rust dependency graph (#136).
The [Pre-flight](#pre-flight-before-tagging) install block verifies the archive SHA-256
against the pinned action manifest. It checks the binary version before use.
The local pre-flight command runs all four checks.

- `Rust — cargo-deny licenses, bans, sources` runs in `ci.yml` on every pull request
  and on pushes to `main`. The verifier requires the GitHub Actions check run with this exact name.
  The license check accepts only the allowed licenses and crate exceptions.
  The bans check rejects wildcard requirements and duplicate or unused workspace dependency declarations.
  Duplicate crate versions give a warning only. The checked graph has four duplicate
  pairs with skips: `syn`, `unicode-width`, `windows-sys`, and `wit-bindgen`.
  The sources check rejects dependencies from Git or registries other than crates.io.
- `Rust — cargo-deny advisories` runs in `.github/workflows/deny-advisories.yml`.
  It runs daily at 04:23 UTC, on manual dispatch, and on pull requests that change
  `Cargo.lock`, `**/Cargo.toml` (also the root manifest), `deny.toml`, or that workflow file.
  The verifier checks both paths of a renamed file. On a matching head, it requires
  a successful check from the advisories workflow. A missing, incomplete, or failed run blocks the merge.
  Other pull requests do not require that run. If a run exists on the head, it must pass.
- The release CI-history check reads only CI workflow runs. The CI dependency job
  must pass for the release. The separate advisories run does not block a release tag.
  Run the full local check before you tag.

**Absent advisories run.**

1. For a branch in this repository, run `gh workflow run deny-advisories.yml --ref <branch>`.
   Manual dispatch requires the workflow on the default branch.
   If a fork PR needs approval, use the [workflow approval procedure](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/approve-runs-from-forks).
2. Get the run head SHA with `gh run view <run-id> --json headSha`.
   Get the PR head SHA with `gh pr view <pr-number> --json headRefOid`.
   Confirm that the two SHAs match.
3. Wait for the run to complete successfully.
4. Run `node scripts/verify-pr-checks.mjs <pr-number>` again.

Do not waive the check or roll back protection for an absent run.

**Advisory triage.** The maintainer who last changed the cron line receives scheduled
failure notices. That maintainer checks each notice. If a new advisory applies,
open a pull request with the dependency fix.

If the fix must wait, open an issue.
Add an ignore in `deny.toml`.
Set `id` to the advisory ID.
Set the reason to `#NNN until YYYY-MM-DD: text` with that issue number.
Set the date no later than the end of the soak window for the release with the fix.
The governance spec rejects expired ignores. Remove the entry when the fix lands.

The advisory ID and issue number in `deny.toml` are public.
CI diagnostics are also public.
In the public issue, state only the effect, for example, "update crate X."
Use only the private report process in [SECURITY.md](SECURITY.md) for new reports
about this project and their details.

**Break-glass procedure.** If the tool or database fails, use a reviewed pull request
that updates the tool pin. If the pinned tool cannot read a new database record format,
use the same procedure. If a pin update cannot restore the check, restore the saved
required checks with the rollback above. Then use a revert pull request that removes
the failed check and its verifier rule together. If a control fails, use a pull request
with a new fixture that proves the check rejects the control. Never merge without the verifier.

**Tool pin update.** Dependabot does not update `tool:` values.

1. Install the candidate with the archive and checksum procedure in Pre-flight.
   Check the exact output of `cargo-deny --version`.
2. Run the production checks and controls with the candidate tool.
   Record the command forms, exit codes, and diagnostic text before you change the spec literals.
3. Update all six pin sites in the same pull request:
   - The install step and version assertion in `.github/workflows/ci.yml`.
   - The install step and version assertion in `.github/workflows/deny-advisories.yml`.
   - The header in `deny.toml`.
   - The governance spec, `scripts/__test__/cargo-deny-policy.spec.mjs`.
   - The Pre-flight section of this guide.
   - The local check line in `CLAUDE.md`.
4. Run the governance spec. It checks that the six sites agree.

## Notes

- The 7 native napi targets: aarch64-apple-darwin, x86_64-apple-darwin, x86_64-unknown-linux-gnu, x86_64-unknown-linux-musl, aarch64-unknown-linux-gnu, aarch64-unknown-linux-musl, x86_64-pc-windows-msvc. x86_64-gnu passes napi's --use-napi-cross; aarch64-gnu links with the apt cross gcc; both musl legs cross-compile with `napi build … -x` (cargo-zigbuild 0.23.0) — the `-x` flag makes `@napi-rs/cli` 3.8.6 run `cargo zigbuild` instead of `cargo build`; cargo-zigbuild 0.23.0 is installed by the SHA-pinned `taiki-e/install-action` (v2.86.3) with `fallback: none` placed BEFORE `Swatinem/rust-cache` (rust-cache deletes `~/.cargo/bin` binaries on save, so installing after cache would be wiped on a warm hit) and asserted after rust-cache and re-asserted after the build (napi's detector is presence-only — `cargo help zigbuild` — and silently runs an unpinned `cargo install cargo-zigbuild` when zigbuild is absent, reverting the pin); zig 0.16.0 via the SHA-pinned `mlugg/setup-zig`; cargo-zigbuild 0.23.0 owns the full linker-arg filter (`--fix-cortex-a53-843419`, `--no-undefined-version`, `-lgcc_s` to `-lunwind`, self-contained musl CRT skip, response files), its CI tests zig 0.16.0, and nothing in 0.23.2–0.23.4 touches x86_64/aarch64 musl — bumping zig OR cargo-zigbuild is a deliberate, paired decision; never set a musl linker export (`CARGO_TARGET_*_MUSL_LINKER`) — cargo-zigbuild's `add_env_if_missing` yields to a pre-set value and a leftover export would silently revert the migration; the version-keyed wrapper cache (`~/.cache/cargo-zigbuild/0.23.0/…`) is always fresh per version and is deliberately NOT cached (post-build presence proves cargo-zigbuild 0.23.0 ran in that job, not a prior version from cache); the readelf gate uses `ALLOWED_NEEDED='libc\.so|libgcc_s\.so\.1'` with a planted `libunwind.so.1` positive control, and the Alpine load tests are the acceptance instrument for any linkage delta (a NEEDED soname Alpine does not ship is invisible to readelf — only a real `docker run node:22-alpine` catches it). Both musl addons are load-tested on `node:22-alpine` before anything publishes: the x64 load test is the last step of `stage-and-verify-napi` (placed after the staged artifact upload so the artifact is preserved even when the x64 test fails), and the arm64 load test runs in the separate unguarded `load-test-musl-arm64` job on a native `ubuntu-24.04-arm` runner using the `napi-staged` artifact. The fixture is `index.js` + `scripts/musl-load-probe.cjs` + only the musl platform package under `node_modules/@mdscript/`, so a pass is proof the loader's `isMusl()` returned true; a control fixture without the package must fail first (PF-013). `publish-crates` blocks on both via its `needs:` list AND its `if:` conjunct (PF-047). The readelf gate proves ELF metadata (no glibc soname) but not that the addon dlopens on Alpine — a NEEDED entry that Alpine does not ship (e.g. `libunwind.so.1`) is invisible to it; only a real load on `node:22-alpine` catches that (PF-038 shape). When the x64 load test fails, the arm64 job is skipped (its `if:` requires `stage-and-verify-napi` to succeed) and both tests are re-run together after the fix. The container runs with `-w /w`: `node:22-alpine` has no `WORKDIR`, and its default cwd of `/` made every mds-core string compile fail until #371 was fixed (surfaced by this gate's first run on PR #370). The flag is kept as a regression tripwire — `musl-load-probe.cjs` asserts `process.cwd() === '/w'`, so a dropped flag fails loudly.
- The 8 Python artifacts (7 `cp311-abi3` wheels + 1 sdist): manylinux x86_64 and aarch64, musllinux_1_2 x86_64 and aarch64, macOS x86_64 and arm64, Windows x86_64, plus one source distribution. Built by `PyO3/maturin-action@v1.51.0` (maturin 1.13.3). The musl and manylinux legs run inside Docker containers that maturin-action manages; the readelf linkage gate asserts the `.so` inside each Linux wheel links the correct libc (musl or glibc), with a positive control and a non-vacuity guard (PF-038). Platform wheels cannot be built or validated locally — use the branch dry-run workflow instead.
- Keep the host Rust install guard and per-leg cache keys in `build-python` (#351).
  Linux wheel containers supply Rust. In `maturin-action` v1.51.0, the cache purge
  checks `debugBuildDir` instead of `releaseBuildDir`. It misses proc-macro files
  in `target/release/deps` ([upstream issue](https://github.com/PyO3/maturin-action/issues/476)).
  Revisit the workaround only after an upstream fix ships and a release rehearsal
  with that fix passes.
- `crates/mds-wasm/Cargo.toml` enables `wasm-opt`. The
  [WASM setup action](.github/actions/setup-wasm/action.yml) installs `wasm-pack`
  and Binaryen `wasm-opt`. CI stops before the build if either tool differs from
  its pin. The local WASM build scripts warn but continue. If `wasm-opt` is absent
  from `PATH`, `wasm-pack` uses its bundled `wasm-opt` instead of the CI version.
  To match CI, put the exact versions from the setup action on `PATH`.
  A local size result does not replace the CI size check.
- Platform packages are generated in CI only — they cannot be validated with a local npm pack; use the dry-run workflow instead.
- `atomic_write_file` (`crates/mds-cli/src/write.rs`, the write funnel) is replace-by-rename: it does not preserve hard links, ACLs, extended attributes (xattrs), or owner/group of the original file (permission bits are restored on Unix). This applies to `mds fmt`, `mds lint --fix`, `mds init --force`'s starter file (without `--force`, `create_new` writes a starter that never replaces a file) and — since #227 — `mds build`/`mds watch` compiled outputs and `.map` sidecars (source rewrites fsync before the rename; compiled outputs, sidecars and the starter file rely on the rename alone). The user-facing contract lives in spec §7.2 ("Output writing") and `SECURITY.md`; hard-link preservation is won't-fix by construction, the rest is not planned (#226).
- `Swatinem/rust-cache` computes its key as `v0-rust[-<key>]-<job>-<runner.os>-<runner.arch>-<envhash>-<lockhash>`; the env hash covers `rustc -vV` (the HOST triple) plus `CARGO*`/`CC*`/`CFLAGS` env. The cross TARGET is not in it, so without `key:` all four ubuntu legs in `build-napi` and both macOS legs share one blob. Both matrix jobs carry per-leg keys: `build-napi` uses `key: ${{ matrix.settings.target }}` (#352, PF-041); `build-python` uses `key: ${{ matrix.target }}-${{ matrix.manylinux }}` (#347) so a containerised Linux leg never restores host-built build scripts or proc-macro `.so` files written by another leg. `publish-crates` and `publish-npm` are single-leg and use the automatic key. Spec S20 in `scripts/__test__/release-auth-probe.spec.mjs` pins this — dropping `with: key:` from a matrix job's rust-cache step causes S20 to fail `Version gate`. Validation: two runs in the SAME cache scope — first run shows `No cache found.`; second shows `Restored from cache key "v0-rust-<target>-build-napi-…" full match: true.` for every leg with the readelf gates green. `pull_request` caches live under `refs/pull/N/merge` and are invisible to a branch dispatch, so warm evidence comes from a second dispatch on the same branch (or a PR-run rerun), never from a dispatch that follows a PR run.
