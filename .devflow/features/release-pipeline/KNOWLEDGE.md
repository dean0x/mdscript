---
feature: release-pipeline
name: Release pipeline gates (release.yml, verify-pr-checks.mjs, gate specs)
description: "Use when modifying release.yml, adding CI jobs, updating TIER_B_EXPECTED_SKIPPED, adjusting the pull_request surface trigger, debugging a publish failure, running the pre-merge verifier, or reasoning about the publish job ordering. Keywords: release, release.yml, verify-pr-checks, TIER_B_EXPECTED_SKIPPED, RELEASE_SURFACE, rehearse-publish-python, tag-push, TestPyPI, publish-crates, publish-npm, publish-python, github-release, version-gate, stage-and-verify-napi, ADR-013, PF-040."
category: architecture
directories: [.github/workflows, .github/actions, scripts, scripts/__test__]
created: 2026-09-07
updated: 2026-09-07
---

# Release Pipeline Gates

## Overview

The release pipeline is a single coordinated tag-push that ships every language surface
simultaneously: two Rust crates (crates.io), nine npm packages, and one Python wheel
matrix (PyPI). All live in `.github/workflows/release.yml`. The same workflow has two
rehearsal entry points — a `pull_request` trigger for release-surface PRs and a
`workflow_dispatch` for branch dry-runs — so that the tag-guarded publish path is exercised
before it runs for real.

`scripts/verify-pr-checks.mjs` is the mandatory pre-merge gate for EVERY PR; it asserts
that every required context is `completed+success` and emits the exact `gh pr merge`
command to use verbatim. Adding a job to either workflow is a multi-place change (see
ADR-013 below). `scripts/__test__/release-auth-probe.spec.mjs` and
`scripts/__test__/verify-pr-checks.spec.mjs` enforce those multi-place rules as
CI-checked specs.

## System Context

```
push.tags v*      ─┐
workflow_dispatch  ├─→  release.yml  →  registries (crates.io, npm, PyPI)
pull_request       ─┘                   (publish jobs tag-guarded / input-guarded)
```

Three entry points, one workflow:

- **`push.tags v*`** — full coordinated release; all eleven jobs run.
- **`workflow_dispatch`** — dry run; five publish jobs are `skipped` (tag guard fails). Add
  `-f testpypi=true` to also trigger the `publish-testpypi` opt-in leg.
- **`pull_request` (path-filtered)** — rehearsal only; paths filter is the release surface
  (see RELEASE_SURFACE below). Publish jobs report `skipped`. The mandatory verifier
  REQUIRES the three `RELEASE_SURFACE_CONTEXTS` jobs to be `completed+success`.

`concurrency: release-${{ github.ref }}` with `cancel-in-progress: false` is load-bearing
(avoids PF-017's cancelled-run-as-green shape mid-sequence and prevents a partial-publish
state between `cargo publish` and `npm publish`).

## Component Architecture — Eleven Jobs and Their DAG

```
version-gate
  ├─→ build-napi (7 legs)  ─→ stage-and-verify-napi ─→ load-test-musl-arm64 ─┐
  └─→ build-python (8 legs) ─→ rehearse-publish-python ──────────────────────┤
                                └─→ publish-testpypi     │ (dispatch+input only)
                                                          ↓
                                              publish-crates  (tag only)
                                                ↓           ↓
                                           publish-npm   publish-python  (tag only)
                                                └────────────┘
                                                      ↓
                                               github-release  (tag only)
```

Job details:

| Job id | Display name | Guarded by | Needs |
|---|---|---|---|
| `version-gate` | Version gate | nothing (always runs) | — |
| `build-napi` | Build napi (...) | nothing | version-gate |
| `stage-and-verify-napi` | Stage + verify platform packages | nothing | build-napi |
| `load-test-musl-arm64` | Alpine load test (linux-arm64-musl) | unguarded, not-cancelled + `needs.stage-and-verify-napi.result == 'success'` | stage-and-verify-napi |
| `build-python` | Build Python (...) | nothing | version-gate |
| `rehearse-publish-python` | Rehearse PyPI publish (no upload) | nothing | build-python |
| `publish-testpypi` | Publish to TestPyPI (rehearsal) | `workflow_dispatch && inputs.testpypi` | build-python, rehearse-publish-python |
| `publish-crates` | Publish to crates.io | `startsWith(ref, 'refs/tags/v')` | version-gate, stage-and-verify-napi, load-test-musl-arm64, build-python, rehearse-publish-python |
| `publish-npm` | Publish to npm | `startsWith(ref, 'refs/tags/v')` | stage-and-verify-napi, publish-crates |
| `publish-python` | Publish to PyPI | `startsWith(ref, 'refs/tags/v')` | build-python, rehearse-publish-python, publish-crates, publish-npm |
| `github-release` | GitHub Release | `startsWith(ref, 'refs/tags/v')` | publish-crates, publish-npm, publish-python |

`load-test-musl-arm64` proves the `linux-arm64-musl` addon dlopens on `node:22-alpine`
(the readelf gate proves ELF metadata but not runtime loadability — a missing NEEDED
entry like `libunwind.so.1` is invisible to readelf; PF-038 shape). The x64 equivalent
runs as the last step of `stage-and-verify-napi` after the staged artifact upload, so
an x64 failure never suppresses the artifact; `load-test-musl-arm64` is skipped when
x64 fails (both are re-run together after the fix).

Key ordering constraints:
- `publish-crates` blocks on `rehearse-publish-python`: a failed OIDC exchange aborts before the irreversible crates.io write (PF-039).
- `publish-crates` blocks on `build-python`: a Python wheel build failure aborts before ANY registry write (PF-023).
- `publish-python` blocks on `publish-crates` AND `publish-npm`: PyPI is last because it is the only surface with OIDC revocability; losing npm is less recoverable than losing PyPI.
- `github-release` is gated on all three publish jobs; a partial failure leaves no Release page (see ADR-014 on backfilling).

## Component Interactions — RELEASE_SURFACE and the Three-Place Rule (ADR-013)

The `pull_request` trigger fires on exactly six paths (the **release surface**):

```
.github/workflows/release.yml
.github/actions/**
crates/mds-napi/**
crates/mds-python/**
scripts/verify-napi-names.mjs
scripts/musl-load-probe.cjs
```

`crates/mds-core/**`, `Cargo.toml`, and `package.json` are excluded on purpose: they change
on most PRs and `ci.yml` already covers them. A dependency sweep that does not touch these
six paths still needs a manual `workflow_dispatch` dry run.

`scripts/verify-pr-checks.mjs` exports `RELEASE_SURFACE` (the same list) and spec S10 in
`release-auth-probe.spec.mjs` asserts set-equality between the two. They must be kept in
sync whenever the `on.pull_request.paths:` list changes.

**ADR-013 three-place rule**: Adding any job to `release.yml` is a three-place change:

1. The workflow file itself.
2. Branch protection required contexts (via the GitHub API, currently 15 contexts).
3. `EXPECTED_CONTEXTS` in `scripts/verify-pr-checks.mjs` (for `ci.yml` jobs) **or**
   `TIER_B_EXPECTED_SKIPPED` (for `release.yml` tag/input-guarded jobs).

A job added without all three places is silently decorative — it can never block a merge.
The length assertion in `verify-pr-checks.spec.mjs` is the mechanical enforcer; do not
relax it when adding a job.

**ADR-013 amendment (2026-09-06 — step-level guard rule)**: On a PR-triggered release run,
disable PR-inapplicable behaviour at **step** level, never at **job** level. Any job present
on a PR head must run through to `conclusion=success` — a job-level `if:` that produces
`conclusion=skipped` for an unlisted name hard-fails the mandatory verifier on the PR that
adds the trigger. The CI-history gate in `version-gate` is the concrete example: it uses a
step-level `if: github.event_name != 'pull_request'` with a `::notice::` sibling, because
a job-level skip would make `Version gate` report `skipped` and break the verifier.

## Component Interactions — `verify-pr-checks.mjs` Tier System

The verifier reads check-run name, status, conclusion, and check-suite identity — one
bounded `GET /actions/runs?head_sha=` call maps each check-suite id to its workflow file
(D-PR8, #341). The verifier exits 2 when that run-list cannot be enumerated.

| Tier | Membership | Passing condition |
|---|---|---|
| **Tier A** (required) | 15 contexts from live branch protection API | `completed+success` |
| **Tier A+ (local)** | `EXPECTED_CONTEXTS` (4 entries: Source hygiene, Python — build & test, examples/ gitignore coverage, Python — wheel install smoke) | `completed+success`, presence required |
| **Tier B** | Everything else | `completed+success`; `skipped` tolerated ONLY for the five names in `TIER_B_EXPECTED_SKIPPED` AND only when the check-run belongs to a `release.yml` check suite |

`TIER_B_EXPECTED_SKIPPED` (5 names):
- `Publish to crates.io`
- `Publish to npm`
- `Publish to PyPI`
- `GitHub Release`
- `Publish to TestPyPI (rehearsal)`

Any `skipped` conclusion under any other name, or for a name in the list whose check-suite
is NOT a `release.yml` run, fails Tier B. Any `cancelled`, `neutral`, `in_progress`, or
`queued` conclusion fails regardless of name (PF-017). A Tier-B-only rejection
(e.g. an advisory CodeQL `neutral`) must be adjudicated against the required-context set
before it is believed — it is never license to merge unverified.

**D-PR7 release-surface presence check**: When any changed file matches `RELEASE_SURFACE`,
the verifier additionally requires `Version gate`, `Stage + verify platform packages`, and
`Rehearse PyPI publish (no upload)` to each be `completed+success`. These are attributed
by suite — the check-run must belong to a `release.yml` run; any event counts. All runs
under each name must pass (duplicate names = all-must-pass).

`Alpine load test (linux-arm64-musl)` (`load-test-musl-arm64`) is a Tier-B-binding
unguarded check-run on release-surface PRs — it reaches `conclusion=success` on every PR
and dispatch run. It is deliberately NOT listed in `RELEASE_SURFACE_CONTEXTS` (the 2026-09
verifier fixtures predate it); spec S21 pins its existence, runner, guard shape, wiring,
and step order instead (ADR-013). It is not a branch-protection required context.

The verifier prints `gh pr merge N --squash --admin --match-head-commit <sha>` on PASS.
Always use this command verbatim — confirm the current branch resolves to the intended PR
before running it (the command names a SHA but not a PR; `gh pr merge` re-resolves the
subject from the current branch at execution time; avoids PF-017).

## Integration Patterns — Gate Details

### version-gate (always runs)

Every step in `version-gate` runs on every event **except** the CI-history gate, which is
step-skipped on `pull_request` with `if: github.event_name != 'pull_request'`. Reason: on
`pull_request` `github.sha` is the merge commit whose own `ci.yml` run is concurrent, and
the gate correctly fails closed on an in-progress run. A notice step fires instead so the
job still reaches `success`.

Steps (in order):
1. Verify publish credentials (npm `whoami` + cargo token non-empty + PyPI OIDC mint-token exchange).
2. Assert synchronized versions, no `file:` refs.
3. Assert no hazardous codepoints in tracked source.
4. Run `npm run test:gates` — all four spec files, 211 tests including pin-shape specs (S16), per-leg cache key spec (S20), and Alpine load test job spec (S21).
5. Assert tagged SHA has green CI history (step-skipped on `pull_request`).

Because `npm run test:gates` runs inside `version-gate`, a malformed pin (e.g. a commit SHA
instead of a `vX.Y.Z` tag) causes `Version gate` to fail before the rehearsal job ever runs.

**Fork / Dependabot PRs**: The "Verify publish credentials" step explicitly exits 1 with a
`::error::` when `NODE_AUTH_TOKEN` is empty on a `pull_request` event from Dependabot or a
fork (`IS_FORK=true`). Do not merge on these checks; supersede with a maintainer PR or a
manual `gh workflow run`.

### rehearse-publish-python — Four Gates with Positive Controls (PF-013)

`rehearse-publish-python` holds `permissions: contents: read` — no `id-token: write`, so it
structurally cannot upload even if an upload step were re-introduced. It NEVER `uses:` the
`pypa/gh-action-pypi-publish` action (the action has no dry-run input; a draft that wired
`dry-run: true` attempted a real upload from a PR).

Four gates, each with an in-step positive control:

1. **Pin shape** (`Gate 1/4`): exactly one distinct `pypa/gh-action-pypi-publish@` ref in the
   file, must match `^v\d+\.\d+\.\d+$`. Controls reject the commit sha `dc37677b…` and the
   annotated tag object sha `a892a5a6…`.
2. **Anonymous GHCR manifest probe** (`Gate 2/4`): control ref must return HTTP 404 +
   `MANIFEST_UNKNOWN`; pin must return HTTP 200.
3. **`docker pull`** (`Gate 3/4`): control tag must fail with manifest-unknown; real pull
   bounded to 3 attempts.
4. **`twine check`** (`Gate 4/4`): all 8 distributions checked via `docker run --rm --network
   none --entrypoint twine <image> check` using the image's own twine (7.0.0 at v1.14.2; no
   `--strict`). Includes a corrupt-wheel positive control.

The gate asserts 7 wheels + 1 sdist are present before running (non-vacuity guard).

### GHCR image rule for pypa/gh-action-pypi-publish (PF-040)

This action resolves `github.action_ref` to `ghcr.io/pypa/gh-action-pypi-publish:<ref>`.
GHCR holds images for release tags (`v1.14.2`) and commit SHAs on release branches, but
**never** for annotated tag objects (the v0.4.1 failure: `a892a5a6` was the tag object sha,
not the commit sha `dc37677b`). Both look like valid git refs but only one resolves to a
Docker image.

Policy: use a `vX.Y.Z` release tag pin for this one action. All other third-party actions
stay commit-SHA-pinned per PF-040's general rule.

### publish-testpypi (opt-in)

Triggered only by `workflow_dispatch` with `-f testpypi=true`. Publishes to
`https://test.pypi.org/legacy/` with `skip-existing: true` and `attestations: true`. Requires
a separate trusted publisher on test.pypi.org (project `markdown-script`, owner `dean0x`,
repo `mdscript`, workflow `release.yml`, environment blank). A missing record causes
`Trusted publishing exchange failure: invalid-publisher`. The pending publisher expires ~30
days unused.

### publish-crates — Irreversibility and Ordering (PF-023)

`cargo publish -p mds-core` is idempotent (treats `already (uploaded|exists|published)` as
success). `npm publish` calls in `publish-npm` have NO such guard — a partial npm failure is
unrecoverable by re-run. This makes `publish-crates` the single irreversible point of no
return; all correctness gates run before it.

### GitHub expression preprocessor trap

GitHub's expression preprocessor scans `run:` block text **including shell comments** without
skipping them. A comment containing the literal characters `${{` (even `${{ }}`) makes the
entire workflow invalid with "An expression was expected", producing a zero-job run that
completes in the same second. `js-yaml`, `actionlint`, and `@action-validator/cli` all pass
such a file. Never write `${{` in comments; describe it in words.

## Constraints

- `cancel-in-progress: false` is non-negotiable; `true` would produce a cancelled run that
  reads as non-failing under `--admin` merge (PF-017) while leaving registries in a partial state.
- `publish-testpypi` is intentionally NOT in `publish-crates`'s `needs:` — a TestPyPI failure
  should not abort the live release.
- The 2026-08 fixture `scripts/__test__/fixtures/protection-main.json` holds 6 contexts vs.
  15 live — kept byte-identical as a historical baseline. The 2026-09 fixtures added:
  `checks-pr366-e02bcf2.json` (check-runs with suite ids), `runs-pr366-e02bcf2.json`
  (workflow runs mapping suite ids to workflow files), and
  `protection-main-2026-09.json` (15 live contexts, the current shape).
- The `startup-race-probe` Cargo feature (`mds-cli`) must never ship enabled.
- `debug-panics` Cargo feature must never ship enabled (all three binding crates).

## Anti-Patterns

- **Job-level `if:` to skip PR-inapplicable behaviour**: produces `skipped` under a name the
  verifier does not allow, hard-failing the mandatory pre-merge check on the very PR that adds
  the trigger. Use step-level guards instead.
- **Reformatting `needs:` from inline array to block list**: `extractNeeds` in the spec parser
  reads INLINE arrays only; a block list breaks the spec.
- **Adding a `startsWith(ref, 'refs/tags/v')` guard without adding the job name to
  `TIER_B_EXPECTED_SKIPPED`**: that job's `skipped` conclusion will hard-fail the verifier on
  every branch dry-run.
- **Adding a `ci.yml` job without updating branch protection**: the new job is decorative —
  it can never block a merge (ADR-013).
- **Using `section.includes('refs/tags/v')` in specs to detect a job's guard**: the `# ====`
  banner comment above `publish-crates` contains `startsWith(github.ref, 'refs/tags/v')` and
  lands inside the preceding job's section when `extractJobSection` runs. Use `extractJobIf`
  (4-space `    if:` only) to read job-level guards.
- **SHA-pinning `pypa/gh-action-pypi-publish`**: this is a Docker-trampoline action; GHCR
  holds no image for annotated tag object SHAs. Use a `vX.Y.Z` release tag (PF-040).
- **Dispatching a dry run before that ref's `ci.yml` finishes**: the CI-history gate in
  `version-gate` fails closed on an in-progress or absent run.
- **Dropping `with: key:` from a matrix job's rust-cache step**: without a per-leg key all
  legs on the same runner OS restore each other's `target/<triple>/` artifacts and
  host-built build scripts (PF-041). Confirmed live in run 34065573775: every Linux leg in
  `build-napi` restored `v0-rust-build-napi-Linux-x64-6ff13d87-4c33221b`. Spec S20 in
  `release-auth-probe.spec.mjs` fails `Version gate` if a key is removed (#347, #352).
- **Using `job-level container:` on an arm64 runner for Alpine load tests**: GitHub-hosted
  arm64 runners reject `container:` at job level with "JavaScript Actions in Alpine
  containers are only supported on x64 Linux runners". Use `docker run` from a host job
  instead.
- **Adding a `needs:` edge to `publish-crates` without a matching `result == 'success'`
  conjunct in its `if:`** (PF-047): the `!cancelled()` opener removes the implicit
  success-of-needs gate, so a failed load-test job would not block crates.io publish;
  both `needs:` AND `if:` must name the dependency.
- **Counting `positive control` occurrences without `stripCommentLines` first**: the
  `build-python` banner comment contains the phrase `positive control`; stripping comment
  lines before counting is required to get the correct count.
- **Building a load-test fixture from the crate directory or the artifact root**: `napi
  artifacts --output-dir .` writes every `.node` into the crate root; the artifact also carries
  root-level `*.node` files — either source activates candidate 2 of the loader (`.node`
  beside `index.js`) and short-circuits the fixture, making the test vacuous.
- **Testing the musl addon through `@mdscript/mds`**: its WASM fallback makes the test
  vacuous — a successful load does not prove the native addon was reached.

## Gotchas

- **RELEASE_SURFACE set-equality is spec-enforced**: `RELEASE_SURFACE` in
  `verify-pr-checks.mjs` and `on.pull_request.paths:` in `release.yml` must be identical sets.
  Spec S10 in `release-auth-probe.spec.mjs` asserts this. If you add a path to one, add it
  to the other.
- **Positive controls must be exercised via a draft PR, not a bare branch dispatch**: a bare
  branch has no `ci.yml` run; a dispatch from it fails at the CI-history gate before the
  rehearsal job runs.
- **`npm run test:gates` runs inside `version-gate`**: pin-policy violations (S16: must match
  `^v\d+\.\d+\.\d+$`) fail `Version gate` before `rehearse-publish-python` ever starts.
  Observed in run 34064235589 (commit-sha pin → Version gate failed, rehearsal skipped).
- **`extractJobSection` in the spec parser runs from `  <id>:` to the next 2-space job id**:
  any banner comment above a job (e.g. the `# ====` separator above `publish-crates`)
  belongs to the PRECEDING job's section, not to `publish-crates`. Assert guards via
  `extractJobIf`, never `section.includes()`.
- **Dependabot / fork PRs fail credential probe and should not be merged on**: the step exits
  1 with `IS_FORK` "No Actions secrets on this run." The remedy is a maintainer PR or a
  manual dispatch.
- **TestPyPI pending publisher expires ~30 days after creation** if no upload lands. A missing
  or expired record causes `Trusted publishing exchange failure: invalid-publisher` in
  `publish-testpypi`.
- **`docker pull` in Gate 3/4 is bounded to 3 attempts**: a transient GHCR outage can cause
  the rehearsal to fail; `gh run rerun <id> --failed` is the recovery path, not a code change.
- **A second dispatch on the same ref queues behind the first** (concurrency group
  `release-${{ github.ref }}`); it does not cancel it.
- **`version-gate` prints a `::notice::` not a `::skip::`** when it skips the CI-history step
  on `pull_request`: the job still completes `success`, which is the required state.
- **rust-cache cache scope**: `pull_request` caches live under `refs/pull/N/merge` and are
  invisible to a `workflow_dispatch` on the branch. `main` has no `release.yml` caches
  (the workflow never runs on push-to-main), so tag runs are always cold. Warm-cache
  evidence comes from a second dispatch on the same branch ref (or a PR-run rerun) —
  never from a dispatch that follows a PR run.
- **crates.io token — no read-only probe**: `GET /api/v1/me` is `AuthCheck::only_cookie()`
  and returns HTTP 403 for any API token. The only token-accepting read route
  (`GET /api/v1/me/tokens/{id}`) rejects scoped tokens with HTTP 403. The non-empty guard
  in `version-gate` is therefore the strongest check available. A revoked token is first
  detected at `cargo publish` (fail-before-write, after the build matrix is paid for); see
  PF-023 and the v0.4.0 precedent (`gh run rerun --failed`). Durable fix tracked in #368.
- **`napi-staged` artifact carries root-level `*.node` files** in addition to `npm/**`: the
  artifact upload captures the crate root, which may contain multiple addon vintages from
  prior `build:native` or `napi build --platform` calls. Never place a `.node` file beside
  `index.js` in a load-test fixture — loader candidate 2 would short-circuit and bypass
  the platform-package lookup.
- **`extractNeeds` is comment-stripped**: the parser strips comment lines from the job
  section before matching `needs: [...]` so `# needs: [bogus]` is ignored. A comment above
  an inline `needs:` line would previously confuse parsers that did not strip first.
- **`/usr/bin/ldd` on `node:22-alpine` is musl-utils' script**: the file is a shell script
  containing the literal string `musl`; `readFileSync('/usr/bin/ldd', 'utf-8').includes('musl')`
  is the `isMusl()` predicate both in `index.js` and in `musl-load-probe.cjs`.
- **Docker Hub pull limit exemption for hosted runners**: GitHub-hosted runners are exempt
  from Docker Hub's anonymous pull limit for public images (documented at
  docs.github.com/en/actions/reference/limits). A mirror (`public.ecr.aws`) would operate
  under a tighter tier. The gate uses Docker Hub directly and is blocking.
- **Alpine container must run with `-w /w`**: `node:22-alpine` sets no `WORKDIR`; the default
  container cwd is `/`; mds-core rejects a filesystem-root base directory with "cannot resolve
  path /: file not found: /" (#371, surfaced by this gate's first run on PR #370). All four
  `docker run` invocations in the Alpine load-test steps pass `-w /w` so the probe executes
  from the fixture directory — the shape any real non-root cwd has. `musl-load-probe.cjs`
  asserts `process.cwd() === '/w'` so a dropped flag fails loudly rather than silently
  returning a spurious "file not found" error. S21 pins `-w /w` in the needle list.

## Key Files

- `.github/workflows/release.yml` — the complete 11-job release workflow (1430+ lines).
- `.github/workflows/ci.yml` — the build/test workflow whose contexts populate Tier A.
- `scripts/verify-pr-checks.mjs` — mandatory pre-merge verifier; exports `EXPECTED_CONTEXTS`,
  `TIER_B_EXPECTED_SKIPPED`, `RELEASE_SURFACE`, `RELEASE_SURFACE_CONTEXTS`.
- `scripts/musl-load-probe.cjs` — Alpine container smoke-test for musl napi addons; accepts
  `linux-x64-musl` or `linux-arm64-musl` as argv[2]; run inside `node:22-alpine` via
  `docker run --rm --network none --pull=never -w /w -v <staged-dir>:/w:ro <image> node /w/probe.cjs <platform>`.
- `scripts/__test__/verify-pr-checks.spec.mjs` — specs for the verifier (M10c, S13, S18 rules;
  length assertion for `EXPECTED_CONTEXTS`).
- `scripts/__test__/release-auth-probe.spec.mjs` — specs for release.yml structure: pin shape
  (S16), set equality S10, guard detection, no `${{ }}` literal (S19), `uses:` count
  (S14), per-leg cache key (S20), cargo token -z guard (S3 extension), Alpine load test
  job structure and wiring (S21).
- `scripts/__test__/fixtures/protection-main.json` — 6-context branch protection (historical,
  2026-08 baseline; kept byte-identical).
- `scripts/__test__/fixtures/protection-main-2026-09.json` — 15-context branch protection
  (current live shape, 2026-09).
- `scripts/__test__/fixtures/checks-pr366-e02bcf2.json` — check-runs with suite ids (2026-09).
- `scripts/__test__/fixtures/runs-pr366-e02bcf2.json` — workflow runs mapping suite ids to
  workflow files (2026-09).
- `RELEASING.md` — full release runbook including pre-flight checklist, tag-push procedure,
  and post-release verification.

## Related

- **ADR-013** — Branch protection / three-place rule; step-level guard amendment (2026-09-06).
- **ADR-014** — Every tag that reached a registry gets a GitHub Release; backfill protocol.
- **PF-013** — Positive-control discipline in security/gate tests; every absence assertion
  needs a paired presence assertion and a non-vacuity guard.
- **PF-017** — Cancelled runs read as non-failing; `--admin` bypasses required checks;
  `verify-pr-checks.mjs` is the remedy; always confirm current branch before running the
  emitted merge command.
- **PF-023** — `publish-npm` has no idempotency guard; a partial npm failure is unrecoverable
  by re-run; ordering (publish-crates then publish-npm) is load-bearing.
- **PF-036** — CI steps with no local counterpart are undetectable locally; the intra-doc-link
  gate is an example.
- **PF-039** — Tag-guarded steps are the LEAST-tested lines; every `if:` guard is a hole in
  the dry run; give each a dispatch-mode execution path.
- **PF-040** — SHA-pinning breaks Docker-trampoline actions; `pypa/gh-action-pypi-publish`
  requires a `vX.Y.Z` tag, not a commit SHA or annotated tag object SHA.
