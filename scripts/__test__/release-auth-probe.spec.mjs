/**
 * Tests for the npm auth probe in version-gate (security-08).
 *
 * Parses .github/workflows/release.yml with structural text analysis — no
 * external YAML parser; none is present in package.json. Indentation-aware
 * section extraction pins tests against the actual workflow structure rather
 * than source-text patterns that prove nothing about execution order.
 * (applies ADR-009, avoids PF-013)
 *
 * Ordering invariant:
 *   version-gate (probe) → build-napi → stage-and-verify-napi → publish-crates
 * A revoked or absent NPM_TOKEN must fail in version-gate — before any
 * cargo publish makes a crates.io release irreversible.
 *
 * What npm whoami proves and does NOT prove:
 *   Proves  — the token is accepted by the npm registry (authentication).
 *   Does NOT prove — publish rights to the @mdscript scope. A read-only or
 *   wrongly-scoped token passes whoami but would fail at publish time.
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  RELEASE_SURFACE,
  RELEASE_SURFACE_CONTEXTS,
  TIER_B_EXPECTED_SKIPPED,
} from '../verify-pr-checks.mjs';

const ROOT = resolve(fileURLToPath(import.meta.url), '../../..');
const RELEASE_YML = join(ROOT, '.github/workflows/release.yml');

const yml = readFileSync(RELEASE_YML, 'utf8');

// ---------------------------------------------------------------------------
// Structural helpers
//
// YAML indentation contract in release.yml:
//   0-space: top-level keys (on, env, permissions, concurrency, jobs)
//   2-space: job IDs under jobs:
//   4-space: job-level fields (name, runs-on, needs, if, steps, strategy)
//   6-space: step list items (- uses: / - name:) or strategy sub-keys
//   8-space: step fields (with, env, run) or matrix sub-keys
// ---------------------------------------------------------------------------

/**
 * Extract a job section's text. A section starts at `  <jobId>:` (2-space
 * indent) and ends before the next 2-space-indented identifier-colon line.
 */
function extractJobSection(source, jobId) {
  const lines = source.split('\n');
  const start = lines.findIndex(l => l === `  ${jobId}:`);
  if (start === -1) return null;

  let end = lines.length;
  for (let i = start + 1; i < lines.length; i++) {
    // Next top-level job starts with exactly 2-space indent + lowercase identifier + colon
    if (/^  [a-z][a-zA-Z0-9_-]+:\s*$/.test(lines[i])) {
      end = i;
      break;
    }
  }
  return lines.slice(start, end).join('\n');
}

/**
 * Find all job IDs declared under the `jobs:` key.
 */
function findAllJobIds(source) {
  const lines = source.split('\n');
  const jobsIdx = lines.findIndex(l => /^jobs:\s*$/.test(l));
  if (jobsIdx === -1) return [];

  const ids = [];
  for (let i = jobsIdx + 1; i < lines.length; i++) {
    // Stop at a new 0-indent top-level key (safety: there are none after jobs: here)
    if (/^[a-z]/.test(lines[i])) break;
    const m = /^  ([a-z][a-zA-Z0-9_-]+):\s*$/.exec(lines[i]);
    if (m) ids.push(m[1]);
  }
  return ids;
}

/**
 * Extract the direct `needs:` list from a job section.
 * Handles the inline form used throughout this file: `needs: [a, b]`.
 */
function extractNeeds(jobSection) {
  if (!jobSection) return [];
  for (const line of stripCommentLines(jobSection).split('\n')) {
    // 4-space indent, inline array: `    needs: [a, b, c]`
    const m = /^\s+needs:\s+\[(.+)\]\s*$/.exec(line);
    if (m) return m[1].split(',').map(s => s.trim());
    // Multi-line needs (not used in this file, but handled for robustness):
    // `    needs:` followed by `      - item` lines
    if (/^\s+needs:\s*$/.test(line)) return []; // multi-line: caller gets [] and must handle
  }
  return [];
}

/**
 * Build a needs graph: Map<jobId, string[]> of direct dependencies.
 */
function buildNeedsGraph(source) {
  const graph = new Map();
  for (const id of findAllJobIds(source)) {
    graph.set(id, extractNeeds(extractJobSection(source, id)));
  }
  return graph;
}

/**
 * BFS reachability: does `start` (transitively) need `target` in the graph?
 * "A needs B" means A depends on B — B is a prerequisite of A.
 */
function transitivelyNeeds(graph, start, target) {
  const visited = new Set();
  const queue = [start];
  while (queue.length > 0) {
    const current = queue.shift();
    if (current === target) return true;
    if (visited.has(current)) continue;
    visited.add(current);
    for (const dep of (graph.get(current) ?? [])) {
      queue.push(dep);
    }
  }
  return false;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe('security-08: npm auth probe in version-gate', () => {

  // -------------------------------------------------------------------------
  // S1: The setup-node step in version-gate must set registry-url so that
  // actions/setup-node writes `//registry.npmjs.org/:_authToken=${NODE_AUTH_TOKEN}`
  // into .npmrc. Without this line, `npm whoami` cannot read the token even
  // when NODE_AUTH_TOKEN is set in the step environment.
  // -------------------------------------------------------------------------
  test('version-gate setup-node sets registry-url (ensures .npmrc auth line is written)', () => {
    const section = extractJobSection(yml, 'version-gate');
    assert.ok(section, 'version-gate job section must exist in release.yml');
    assert.ok(
      section.includes('registry-url') && section.includes('registry.npmjs.org'),
      'version-gate setup-node must declare registry-url: "https://registry.npmjs.org"; ' +
      'without this, setup-node does not write the auth token line into .npmrc and ' +
      'npm whoami cannot authenticate. ' +
      `Found section:\n${section}`,
    );
  });

  // -------------------------------------------------------------------------
  // S2: version-gate must contain a step that binds NPM_TOKEN through env:
  // and calls npm whoami to verify the token is accepted by the registry.
  // -------------------------------------------------------------------------
  test('version-gate contains credential probe step referencing secrets.NPM_TOKEN via env', () => {
    const section = extractJobSection(yml, 'version-gate');
    assert.ok(section, 'version-gate job section must exist');
    // Probe must bind through env: (standard script-injection guard — the secret
    // value must not be substituted into the shell source).
    assert.ok(
      section.includes('secrets.NPM_TOKEN'),
      'version-gate must contain a step whose env: references secrets.NPM_TOKEN; ' +
      `got section:\n${section}`,
    );
    // npm whoami is the observable check: it contacts the registry and returns the
    // authenticated username, proving the token is valid at workflow time.
    assert.ok(
      section.includes('npm whoami'),
      'version-gate credential probe must call npm whoami to verify token validity; ' +
      `got section:\n${section}`,
    );
  });

  // -------------------------------------------------------------------------
  // S3: version-gate must also guard CARGO_REGISTRY_TOKEN — catching a missing
  // cargo token before the 7-target cross-compile matrix starts, not after.
  // -------------------------------------------------------------------------
  test('version-gate guards CARGO_REGISTRY_TOKEN for non-empty (security-08)', () => {
    const section = extractJobSection(yml, 'version-gate');
    assert.ok(section, 'version-gate job section must exist');
    assert.ok(
      section.includes('secrets.CARGO_REGISTRY_TOKEN'),
      'version-gate must guard CARGO_REGISTRY_TOKEN via env: in the probe step; ' +
      `got section:\n${section}`,
    );
    // S3 extension: the -z guard must be in executable code, not inside a comment.
    // Positive control (avoids PF-013): a section that binds the secret but places
    // the guard line only inside a comment must return false.
    const guardLineInComment = [
      '        env:',
      '          CARGO_REG_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}',
      '        run: |',
      '          # if [ -z "$CARGO_REG_TOKEN" ]; then',
      '          echo "do something else"',
    ].join('\n');
    assert.ok(
      !hasEmptyGuard(guardLineInComment),
      'positive control: hasEmptyGuard must return false when the guard line is only inside a comment',
    );
    assert.ok(
      hasEmptyGuard(section),
      'version-gate must contain an executable `if [ -z "$CARGO_REG_TOKEN" ]; then` guard ' +
      '(#345, RELEASING.md "credential probe") — the strongest check crates.io allows for an ' +
      'API token; a missing token is first detected at cargo publish (fail-before-write) but ' +
      'this guard catches it before the expensive 7-target cross-compile matrix runs; ' +
      `got section:\n${section}`,
    );
  });

  // -------------------------------------------------------------------------
  // S4: Every job with cargo publish must transitively need version-gate.
  //
  // This is the core ordering invariant: cargo publish (irreversible) must not
  // start unless the credential probe has already passed in version-gate.
  // Checked via BFS over the needs graph so future job reorderings are caught.
  //
  // Non-vacuity guard (ADR-009): assert at least one cargo-publish job exists
  // so the loop cannot trivially pass by returning no jobs to check.
  // -------------------------------------------------------------------------
  test('every cargo-publish job transitively needs version-gate (ordering invariant)', () => {
    const graph = buildNeedsGraph(yml);
    const allJobIds = [...graph.keys()];

    // Find all jobs whose step text contains the `cargo publish` command.
    const cargoPublishJobs = allJobIds.filter(id => {
      const section = extractJobSection(yml, id);
      return section !== null && section.includes('cargo publish');
    });

    // Non-vacuity: if no cargo-publish job exists, the loop passes vacuously.
    // A release workflow without cargo publish is indeterminate — not a pass (ADR-009).
    assert.ok(
      cargoPublishJobs.length > 0,
      'release.yml must contain at least one job with `cargo publish` (ADR-009 non-vacuity guard); ' +
      `found jobs: ${allJobIds.join(', ')}`,
    );

    for (const job of cargoPublishJobs) {
      assert.ok(
        transitivelyNeeds(graph, job, 'version-gate'),
        `Job "${job}" contains cargo publish but does not transitively need "version-gate". ` +
        `The credential probe in version-gate would be bypassed, meaning a revoked NPM_TOKEN ` +
        `would not be caught until after crates.io publish (irreversible). ` +
        `Direct needs of "${job}": [${(graph.get(job) ?? []).join(', ')}]`,
      );
    }
  });

});

// ---------------------------------------------------------------------------
// Additional structural helpers for B1 checks (S5-S13)
// ---------------------------------------------------------------------------

/**
 * Extract the `on:` block — from `on:` (0-indent) to the next 0-indent key.
 */
function extractOnBlock(source) {
  const lines = source.split('\n');
  const start = lines.findIndex(l => /^on:\s*$/.test(l));
  if (start === -1) return null;
  let end = lines.length;
  for (let i = start + 1; i < lines.length; i++) {
    if (/^[a-z]/.test(lines[i])) { end = i; break; }
  }
  return lines.slice(start, end).join('\n');
}

/**
 * Extract the job-level `if:` condition (4-space indent).
 * Returns the condition string, or null when none is present.
 */
function extractJobIf(jobSection) {
  if (!jobSection) return null;
  for (const line of jobSection.split('\n')) {
    const m = /^    if:\s+(.+)$/.exec(line);
    if (m) return m[1].trim();
  }
  return null;
}

/**
 * Drop whole-line YAML comments.
 *
 * Checks that ask "does this job invoke X?" must read executable YAML only: a
 * comment naming `uses: pypa/gh-action-pypi-publish` (as the rehearsal's own
 * warning-not-to does) is documentation, not an invocation, and a guard that
 * cannot tell them apart fires on the text that exists to prevent the defect.
 */
function stripCommentLines(text) {
  return text.split('\n').filter(l => !/^\s*#/.test(l)).join('\n');
}

/**
 * True iff the text contains an executable (non-comment) line that matches
 * `if [ -z "$CARGO_REG_TOKEN" ]; then` — the -z guard for the cargo token
 * (#345). Uses stripCommentLines so a line that appears only inside a comment
 * does not satisfy the check (positive control in S3).
 */
function hasEmptyGuard(text) {
  return /^\s*if \[ -z "\$CARGO_REG_TOKEN" \]; then\s*$/m.test(stripCommentLines(text));
}

/**
 * Extract the `on.pull_request.paths:` list as an array of path patterns.
 * Returns null when the trigger or its paths filter is absent.
 *
 * Indentation contract inside the `on:` block:
 *   2-space: event names (push, pull_request, workflow_dispatch)
 *   4-space: event fields (paths, tags, inputs)
 *   6-space: list items (`      - '<pattern>'`)
 */
function extractPullRequestPaths(source) {
  const onBlock = extractOnBlock(source);
  if (onBlock === null) return null;
  const lines = onBlock.split('\n');
  const prIdx = lines.findIndex(l => /^  pull_request:\s*$/.test(l));
  if (prIdx === -1) return null;
  let pathsIdx = -1;
  for (let i = prIdx + 1; i < lines.length; i++) {
    if (/^  [a-z_]+:/.test(lines[i])) break;   // next event — paths not found
    if (/^    paths:\s*$/.test(lines[i])) { pathsIdx = i; break; }
  }
  if (pathsIdx === -1) return null;
  const out = [];
  for (let i = pathsIdx + 1; i < lines.length; i++) {
    const m = /^      - ['"]?([^'"]+)['"]?\s*$/.exec(lines[i]);
    if (!m) break;
    out.push(m[1]);
  }
  return out;
}

// ---------------------------------------------------------------------------
// B1 structural invariants (S5-S13)
// ---------------------------------------------------------------------------

describe('B1: release-surface PR gate and rehearsal jobs', () => {

  // -------------------------------------------------------------------------
  // S5: rehearse-publish-python job must exist.
  // Non-vacuity: without this job the D-PR6 release-surface check would never
  // be exercised on PRs, defeating PF-039 (tag-guarded steps are untested).
  // -------------------------------------------------------------------------
  test('S5: rehearse-publish-python job exists in release.yml', () => {
    const section = extractJobSection(yml, 'rehearse-publish-python');
    assert.ok(
      section !== null,
      'rehearse-publish-python job must exist; PF-039 — a tag-guarded publish step ' +
      'that is never rehearsed on PRs cannot be validated before the release',
    );
  });

  // -------------------------------------------------------------------------
  // S6: rehearse-publish-python must have a job-level if: that is conditioned
  // on needs.build-python.result == 'success' but NOT on refs/tags/v or any
  // startsWith(github.ref) guard, so PRs and dispatches both exercise it.
  //
  // PF-039 rationale: a tag guard would make rehearse-publish-python skip on
  // pull_request, removing the only pre-tag validation of the pypa action pin
  // and GHCR image. The job must run on all three triggers; using a needs
  // result-condition (not an event guard) is the correct pattern.
  // -------------------------------------------------------------------------
  test('S6: rehearse-publish-python has a job-level if: conditioned on build-python success (not a tag guard)', () => {
    const section = extractJobSection(yml, 'rehearse-publish-python');
    assert.ok(section !== null, 'rehearse-publish-python must exist (see S5)');
    const jobIf = extractJobIf(section);
    assert.ok(
      jobIf !== null,
      'rehearse-publish-python must have a job-level if: conditioned on needs.build-python.result ' +
      '(the job is decoupled from stage-and-verify-napi so a napi failure cannot block the rehearsal); ' +
      `got if: ${jobIf}`,
    );
    assert.ok(
      jobIf.includes("needs.build-python.result == 'success'"),
      'rehearse-publish-python if: must contain needs.build-python.result == \'success\'; ' +
      `got: ${jobIf}`,
    );
    // The guard must NOT be a refs/tags/v or startsWith(github.ref) condition —
    // those would re-introduce PF-039 by skipping on pull_request events.
    assert.ok(
      !jobIf.includes('refs/tags') && !jobIf.includes('startsWith(github.ref'),
      'rehearse-publish-python if: must NOT contain a refs/tags/v or startsWith guard — ' +
      'that would skip on pull_request, removing pre-tag validation (PF-039); ' +
      `got: ${jobIf}`,
    );
  });

  // -------------------------------------------------------------------------
  // S7: rehearse-publish-python must transitively need version-gate.
  // This ensures the credential probe runs before the OIDC exchange contacts
  // the registry, preserving the security-08 ordering invariant.
  // -------------------------------------------------------------------------
  test('S7: rehearse-publish-python transitively needs version-gate (ordering invariant)', () => {
    const graph = buildNeedsGraph(yml);
    assert.ok(
      graph.has('rehearse-publish-python'),
      'rehearse-publish-python must be in the jobs graph (see S5)',
    );
    assert.ok(
      transitivelyNeeds(graph, 'rehearse-publish-python', 'version-gate'),
      'rehearse-publish-python must transitively need version-gate so the credential ' +
      'probe runs before the OIDC exchange; ' +
      `direct needs: [${(graph.get('rehearse-publish-python') ?? []).join(', ')}]`,
    );
  });

  // -------------------------------------------------------------------------
  // S8: the rehearsal must contain a GHCR manifest probe for the
  // pypa/gh-action-pypi-publish pin (PF-040, #350 option 3).
  // An absent or stale pin (SHA instead of tag name) silently breaks the
  // publish-python job at runtime with "manifest unknown".
  //
  // The probe lives in rehearse-publish-python, not version-gate: publish-crates
  // needs the rehearsal, so a bad pin still aborts the release before the
  // irreversible crates.io write, and keeping one copy means there is one place
  // to bump when the pin moves (a second copy is a place to forget).
  // -------------------------------------------------------------------------
  test('S8: rehearse-publish-python contains GHCR manifest probe for pypa/gh-action-pypi-publish', () => {
    const section = extractJobSection(yml, 'rehearse-publish-python');
    assert.ok(section !== null, 'rehearse-publish-python must exist');
    assert.ok(
      section.includes('ghcr.io/v2/pypa/gh-action-pypi-publish/manifests'),
      'rehearse-publish-python must probe the GHCR manifest for the ' +
      'pypa/gh-action-pypi-publish pin (#350 option 3, PF-040); a missing or SHA-pinned ' +
      'image causes publish-python to fail at runtime with "manifest unknown"; ' +
      `got section (first 600 chars):\n${section.slice(0, 600)}`,
    );
  });

  // -------------------------------------------------------------------------
  // S9: The CI-history step in version-gate must use a step-level if: guard
  // (not a job-level guard).
  //
  // If version-gate itself were guarded at the job level to skip on PRs, the
  // Tier-B verifier would see version-gate as skipped and fail the PR
  // (ADR-013 amendment 2026-09-06). The fix is a step-level if: so the job
  // runs (and succeeds) but the CI-history step is skipped on non-tag events.
  // -------------------------------------------------------------------------
  test('S9: CI-history step in version-gate uses step-level if: (not job-level)', () => {
    const section = extractJobSection(yml, 'version-gate');
    assert.ok(section !== null, 'version-gate must exist');

    // Confirm the CI-history step still exists (non-vacuity).
    assert.ok(
      section.includes('Assert tagged SHA has green CI history'),
      'version-gate must still contain the CI-history step (non-vacuity guard)',
    );

    // Step-level if: is at 8-space indent (step field); job-level at 4-space.
    // We accept step-level if: anywhere in the section (the step is the only
    // consumer of a conditional skip in version-gate).
    const hasStepLevelIf = section.split('\n').some(l => /^        if:/.test(l));
    assert.ok(
      hasStepLevelIf,
      'version-gate must use a step-level if: (8-space indent) on the CI-history step ' +
      'so that PRs can skip that step without marking version-gate itself skipped — a ' +
      'skipped version-gate would fail the Tier-B verifier (ADR-013 amendment 2026-09-06); ' +
      `got section (first 800 chars):\n${section.slice(0, 800)}`,
    );
  });

  // -------------------------------------------------------------------------
  // S10: The on: block must include a pull_request trigger with at least one
  // path from RELEASE_SURFACE (ensuring release-surface PRs exercise the gate).
  // -------------------------------------------------------------------------
  test('S10: on.pull_request.paths equals RELEASE_SURFACE exactly (as a set)', () => {
    const onBlock = extractOnBlock(yml);
    assert.ok(onBlock !== null, 'release.yml must have an on: block');
    assert.ok(
      onBlock.includes('pull_request:'),
      'release.yml must have a pull_request: trigger so release-surface PRs are validated; ' +
      `got on: block:\n${onBlock}`,
    );

    const paths = extractPullRequestPaths(yml);
    assert.ok(
      Array.isArray(paths) && paths.length > 0,
      'on.pull_request.paths must be a non-empty list — an unfiltered trigger would run the ' +
      'whole release matrix on every PR, and an unparseable one would make this test vacuous ' +
      `(PF-013); got on: block:\n${onBlock}`,
    );

    // Positive control (PF-013): the extractor must actually FIND a planted entry.
    // Without this, a regex that silently returns [] would satisfy nothing and the
    // set comparison below would be comparing two empty sets.
    const control = extractPullRequestPaths([
      'on:',
      '  pull_request:',
      '    paths:',
      "      - 'planted/control/**'",
      '  workflow_dispatch:',
      'permissions:',
    ].join('\n'));
    assert.deepEqual(
      control, ['planted/control/**'],
      'positive control: extractPullRequestPaths must parse a planted paths list; ' +
      `got ${JSON.stringify(control)}`,
    );

    // The two lists must be EQUAL as sets. This is the invariant the D-PR6 header
    // comment in verify-pr-checks.mjs asserts: RELEASE_SURFACE is the verifier's
    // model of what triggers this workflow, and a filter the verifier does not
    // know about is a path whose PR silently gets no release run (ADR-013
    // amendment 2026-09-06 — a mis-specified paths filter must not pass as a
    // silent no-run).
    assert.deepEqual(
      [...paths].sort(), [...RELEASE_SURFACE].sort(),
      'on.pull_request.paths and RELEASE_SURFACE (verify-pr-checks.mjs) must be the same set. ' +
      'A path in the workflow but not in RELEASE_SURFACE runs the release matrix without the ' +
      'verifier requiring it; a path in RELEASE_SURFACE but not in the workflow makes the ' +
      'verifier demand release check-runs that can never appear, hard-failing the PR. ' +
      `workflow paths = ${JSON.stringify(paths)}; RELEASE_SURFACE = ${JSON.stringify(RELEASE_SURFACE)}`,
    );
  });

  // -------------------------------------------------------------------------
  // S11: publish-testpypi job must exist (opt-in TestPyPI leg, #350).
  // -------------------------------------------------------------------------
  test('S11: publish-testpypi job exists in release.yml', () => {
    const section = extractJobSection(yml, 'publish-testpypi');
    assert.ok(
      section !== null,
      'publish-testpypi job must exist for the opt-in TestPyPI leg (#350)',
    );
  });

  // -------------------------------------------------------------------------
  // S12: publish-testpypi must be guarded by a job-level if: that references
  // inputs.testpypi so it only runs when the dispatch input is set to true.
  // Without this guard it would run on every PR and tag push, causing
  // unintended TestPyPI uploads.
  // -------------------------------------------------------------------------
  test('S12: publish-testpypi has job-level if: referencing inputs.testpypi', () => {
    const section = extractJobSection(yml, 'publish-testpypi');
    assert.ok(section !== null, 'publish-testpypi must exist (see S11)');
    const jobIf = extractJobIf(section);
    assert.ok(
      jobIf !== null && jobIf.includes('inputs.testpypi'),
      'publish-testpypi must have a job-level if: referencing inputs.testpypi; ' +
      'this keeps it out of PR and standard dispatch runs — it runs ONLY when the ' +
      'workflow_dispatch testpypi input is true (#350); ' +
      `got if: ${jobIf}`,
    );
  });

  // -------------------------------------------------------------------------
  // S13: publish-testpypi's name: field must appear in TIER_B_EXPECTED_SKIPPED
  // so the Tier-B verifier tolerates the skipped conclusion on standard PRs
  // and dispatch runs (ADR-013).
  // -------------------------------------------------------------------------
  test('S13: publish-testpypi name is listed in TIER_B_EXPECTED_SKIPPED', () => {
    const section = extractJobSection(yml, 'publish-testpypi');
    assert.ok(section !== null, 'publish-testpypi must exist (see S11)');

    // Extract the job name: field (4-space indent).
    const jobNameOf = (text) => {
      for (const line of text.split('\n')) {
        const m = /^    name:\s+(.+)$/.exec(line);
        if (m) return m[1].trim();
      }
      return null;
    };

    // Positive control (PF-013): the pair (extractor, membership check) must be
    // able to FAIL. A planted section whose name is not in the allowlist has to
    // be flagged — otherwise `TIER_B_EXPECTED_SKIPPED.has(jobName)` could be
    // passing on a name nobody set, or on an extractor that never returns.
    const plantedName = jobNameOf([
      '  publish-testpypi:',
      '    name: Publish to Somewhere Nobody Allowed',
      '    runs-on: ubuntu-latest',
    ].join('\n'));
    assert.equal(plantedName, 'Publish to Somewhere Nobody Allowed',
      'positive control: the name: extractor must read a planted name');
    assert.ok(!TIER_B_EXPECTED_SKIPPED.has(plantedName),
      'positive control: an unlisted job name must NOT be treated as an allowed skip');

    const jobName = jobNameOf(section);
    assert.ok(
      jobName !== null,
      `publish-testpypi must have a name: field; got section:\n${section}`,
    );

    assert.ok(
      TIER_B_EXPECTED_SKIPPED.has(jobName),
      `publish-testpypi name "${jobName}" must be listed in TIER_B_EXPECTED_SKIPPED ` +
      `in verify-pr-checks.mjs so skipped conclusions are tolerated on non-dispatch runs ` +
      `(ADR-013); current set: [${[...TIER_B_EXPECTED_SKIPPED].join(', ')}]`,
    );
  });

  // -------------------------------------------------------------------------
  // S14: pypa/gh-action-pypi-publish must be invoked ONLY by the two jobs that
  // genuinely publish. This is a drift guard for a defect that actually shipped.
  //
  // Run 34060146952 (a pull_request run of this workflow) executed
  // `uses: pypa/gh-action-pypi-publish@v1.14.2` with `dry-run: true` inside
  // rehearse-publish-python. v1.14.2 has NO dry-run input: the runner logged
  // "Unexpected input(s) 'dry-run'", the action ignored it, and then performed a
  // REAL upload to https://upload.pypi.org/legacy/ from a pull request. It failed
  // only because the workspace version (0.4.2) was already on PyPI — on a
  // version-bump PR (which touches crates/mds-python/Cargo.toml and therefore
  // matches the release-surface paths filter) the upload would have SUCCEEDED,
  // publishing an unreleased version from an unmerged branch.
  //
  // There is no no-upload mode to configure. The rehearsal must reproduce what
  // the action does — pull its GHCR image and run twine out of it — never call it.
  // -------------------------------------------------------------------------
  test('S14: pypa/gh-action-pypi-publish is invoked ONLY by publish-python and publish-testpypi', () => {
    const USES = 'uses: pypa/gh-action-pypi-publish';

    // Positive control (PF-013): `!section.includes(USES)` is satisfied by ANY
    // string, including the empty one a broken extractor would return. Prove the
    // check flags a section that does carry the invocation before trusting it on
    // the real ones.
    const plantedRehearsal = [
      '  rehearse-publish-python:',
      '    name: Rehearse PyPI publish (no upload)',
      '    steps:',
      '      - uses: pypa/gh-action-pypi-publish@v1.14.2',
      '        with:',
      '          dry-run: true',
    ].join('\n');
    assert.ok(
      plantedRehearsal.includes(USES),
      'positive control: a section carrying the invocation must be detected by this check',
    );

    assert.ok(
      !stripCommentLines(plantedRehearsal.replace('      - uses:', '      # - uses:')).includes(USES),
      'positive control: a commented-out invocation must NOT count as an invocation',
    );

    for (const jobId of ['rehearse-publish-python', 'version-gate']) {
      const section = stripCommentLines(extractJobSection(yml, jobId) ?? '');
      assert.ok(section !== '', `${jobId} must exist`);
      assert.ok(
        !section.includes(USES),
        `${jobId} must NOT invoke pypa/gh-action-pypi-publish. The action has no ` +
        `dry-run/no-upload mode: an unrecognised input is warned about and ignored, and ` +
        `the action then uploads for real (run 34060146952 did exactly that from a ` +
        `pull_request). Reproduce the action instead of calling it — GHCR probe, ` +
        `docker pull, twine check (PF-039, PF-040);\ngot section:\n${section}`,
      );
    }

    const invocations = stripCommentLines(yml).split('\n').filter(l => l.includes(USES));
    assert.equal(
      invocations.length, 2,
      `release.yml must invoke pypa/gh-action-pypi-publish exactly twice — once in ` +
      `publish-python (PyPI) and once in publish-testpypi (TestPyPI). Every other ` +
      `invocation is an upload nobody asked for; found ${invocations.length}: ` +
      `${JSON.stringify(invocations.map(l => l.trim()))}`,
    );
    for (const jobId of ['publish-python', 'publish-testpypi']) {
      const section = extractJobSection(yml, jobId);
      assert.ok(section !== null, `${jobId} must exist`);
      assert.ok(
        section.includes(USES),
        `${jobId} is one of the two jobs that must invoke pypa/gh-action-pypi-publish; ` +
        `got section:\n${section}`,
      );
    }
  });

  // -------------------------------------------------------------------------
  // S15: the rehearsal must not hold id-token: write.
  //
  // Defence in depth behind S14: without id-token the job cannot mint a PyPI
  // trusted-publishing token, so even a re-introduced upload step has no
  // credential to upload with. `contents: read` is the whole permission set.
  // -------------------------------------------------------------------------
  test('S15: rehearse-publish-python has contents: read and NO id-token permission', () => {
    const raw = extractJobSection(yml, 'rehearse-publish-python');
    assert.ok(raw !== null, 'rehearse-publish-python must exist (see S5)');
    const section = stripCommentLines(raw);
    assert.ok(
      section.includes('contents: read'),
      `rehearse-publish-python must declare permissions: contents: read; got:\n${section}`,
    );
    assert.ok(
      !section.includes('id-token'),
      'rehearse-publish-python must NOT be granted id-token — a job that cannot mint a ' +
      'PyPI trusted-publishing token cannot upload even if an upload step is ' +
      're-introduced (defence in depth behind S14); ' +
      `got section:\n${section}`,
    );
  });

  // -------------------------------------------------------------------------
  // S16: the pin the rehearsal validates must be the pin the publish jobs use.
  //
  // The rehearsal proves an image exists for PIN_REF. If PIN_REF and the
  // `uses: ...@<ref>` pins drift, the rehearsal proves the wrong image and the
  // release fails on the pin it never checked — the v0.4.1 shape (PF-040).
  // -------------------------------------------------------------------------
  test('S16: rehearse-publish-python PIN_REF equals every pypa/gh-action-pypi-publish pin', () => {
    const raw = extractJobSection(yml, 'rehearse-publish-python');
    assert.ok(raw !== null, 'rehearse-publish-python must exist (see S5)');
    const section = stripCommentLines(raw);
    const m = /^\s+PIN_REF:\s*(\S+)\s*$/m.exec(section);
    assert.ok(
      m !== null,
      'rehearse-publish-python must declare a PIN_REF env var naming the pin under test; ' +
      `got section:\n${section}`,
    );
    const pin = m[1];

    const refs = [...stripCommentLines(yml)
      .matchAll(/uses:\s*pypa\/gh-action-pypi-publish@(\S+)/g)].map(x => x[1]);
    // Non-vacuity (PF-013): an empty ref list would make the loop below pass
    // without comparing anything.
    assert.ok(
      refs.length > 0,
      'release.yml must invoke pypa/gh-action-pypi-publish somewhere (see S14); found none',
    );
    for (const ref of refs) {
      assert.equal(
        ref, pin,
        `pypa/gh-action-pypi-publish is pinned to "${ref}" but the rehearsal validates ` +
        `PIN_REF="${pin}". The rehearsal would prove an image that the publish job never ` +
        `pulls (PF-040). Bump both together.`,
      );
    }

    // The single distinct action ref must be a vX.Y.Z release tag (M17 / PF-040).
    // The gate enforces policy — not just existence — so a commit SHA or annotated-tag
    // object SHA that happens to have a GHCR image is not acceptable (the policy is
    // "pin by release tag name so humans can read the version at a glance").
    const distinctRefs = [...new Set(refs)];
    assert.equal(
      distinctRefs.length, 1,
      `all pypa/gh-action-pypi-publish uses must pin the same ref; found: [${distinctRefs.join(', ')}]`,
    );
    assert.ok(
      /^v\d+\.\d+\.\d+$/.test(distinctRefs[0]),
      `the single distinct pypa/gh-action-pypi-publish pin "${distinctRefs[0]}" must match ` +
      '/^v\\d+\\.\\d+\\.\\d+$/ (a vX.Y.Z release tag) — a commit SHA or annotated-tag object ' +
      'SHA is indistinguishable by eye but has no GHCR image for annotated objects (PF-040); ' +
      `positive control: "a892a5a61159132606e93a2fa6f4358831b04d26" must be REJECTED ` +
      `(it matches /^[0-9a-f]{40}$/ but not /^v\\d+\\.\\d+\\.\\d+$/)`,
    );
  });

  // -------------------------------------------------------------------------
  // S17: the rehearsal's four gates, each with a positive control.
  //
  // PF-013: a gate that has never been observed rejecting anything is not
  // evidence. Each gate below runs a known-bad input first and fails if that
  // input is ACCEPTED.
  // -------------------------------------------------------------------------
  test('S17: rehearsal gates the pin shape, GHCR manifest, docker pull and twine — with positive controls', () => {
    const section = extractJobSection(yml, 'rehearse-publish-python');
    assert.ok(section !== null, 'rehearse-publish-python must exist (see S5)');

    const gates = [
      ['ghcr.io/v2/pypa/gh-action-pypi-publish/manifests',
        'GHCR manifest probe — asks GHCR the same question the runner asks at publish time'],
      ['docker pull',
        'docker pull — proves the artifact the RUNTIME fetches, not just that the git ref resolves (PF-040)'],
      ['--entrypoint twine',
        'twine check must run out of the publish image, bypassing its upload entrypoint'],
      ['--network none',
        'the twine check must run with the network switched off so the rehearsal physically cannot reach pypi.org'],
    ];
    for (const [needle, why] of gates) {
      assert.ok(
        section.includes(needle),
        `rehearse-publish-python must contain "${needle}": ${why};\ngot section:\n${section}`,
      );
    }

    // Each of the four gates must carry a positive control (PF-013).
    const controlLines = section.split('\n').filter(l => l.includes('positive control'));
    assert.ok(
      controlLines.length >= gates.length,
      `each of the ${gates.length} rehearsal gates needs a positive control (PF-013: a gate ` +
      `never observed rejecting anything is not evidence); found ${controlLines.length} ` +
      `line(s) mentioning one`,
    );

    // The controls must be concrete known-bad inputs, not prose.
    assert.ok(
      section.includes('a892a5a61159132606e93a2fa6f4358831b04d26'),
      'the pin-shape gate must be exercised against the v0.4.1 annotated-tag-object SHA ' +
      'that actually broke a release (PF-040), so the gate is proven to reject it',
    );
    assert.ok(
      section.includes('BOGUS_REF'),
      'the GHCR and docker-pull gates must be exercised against a ref that cannot exist, ' +
      'so a probe that returns 200 for everything is caught (PF-013)',
    );

    // Gate 4: the rehearsal must print the twine version from the publish image
    // so logs capture which twine validated the distributions (plan spec item).
    assert.ok(
      section.includes('--version'),
      'Gate 4: rehearsal must invoke the image twine with --version (--entrypoint twine ' +
      '... --version) so the log captures which twine version validated the distributions',
    );
  });

  // -------------------------------------------------------------------------
  // S18: every name the verifier REQUIRES on a release-surface PR must be a
  // real job display-name in release.yml.
  //
  // RELEASE_SURFACE_CONTEXTS is Tier A semantics applied to release check-runs:
  // absence is FAIL. So a job renamed in the workflow without the verifier being
  // updated makes the verifier demand a check-run that can never appear, and the
  // mandatory pre-merge gate hard-fails every release-surface PR — whose natural
  // workaround is bypassing the gate, the PF-017 shape ADR-013's 2026-09-06
  // amendment warns about. Same three-place accounting, third place pinned.
  // -------------------------------------------------------------------------
  test('S18: every RELEASE_SURFACE_CONTEXTS name is a real job name: in release.yml', () => {
    const jobNames = new Set(
      findAllJobIds(yml)
        .map(id => {
          const section = stripCommentLines(extractJobSection(yml, id) ?? '');
          const m = /^    name:\s+(.+)$/m.exec(section);
          return m ? m[1].trim() : null;
        })
        .filter(n => n !== null),
    );

    // Non-vacuity (PF-013): an empty set would satisfy nothing and make every
    // membership assertion below unreachable.
    assert.ok(
      jobNames.size > 0,
      `release.yml must declare job name: fields; found none among jobs [${findAllJobIds(yml).join(', ')}]`,
    );
    // Positive control: a name nobody declared must NOT be found.
    assert.ok(
      !jobNames.has('Rehearse PyPI publish (no upload) — renamed'),
      'positive control: an undeclared job name must not be reported as present',
    );

    for (const ctx of RELEASE_SURFACE_CONTEXTS) {
      assert.ok(
        jobNames.has(ctx),
        `RELEASE_SURFACE_CONTEXTS lists "${ctx}" but no job in release.yml carries that ` +
        `name:. The verifier would require a check-run that can never appear, hard-failing ` +
        `every release-surface PR (ADR-013 amendment 2026-09-06, PF-017). ` +
        `Declared job names: [${[...jobNames].join(' | ')}]`,
      );
    }
  });

  // -------------------------------------------------------------------------
  // M10a: every job whose section contains `cargo publish` must transitively
  // need rehearse-publish-python, so a failed OIDC exchange aborts before the
  // irreversible crates.io write (PF-039/PF-023).
  //
  // Non-vacuity: assert at least one such job exists (PF-013).
  // Positive control: a graph where publish-crates needs only version-gate
  // must be flagged (rehearsal edge is absent → not transitive).
  // -------------------------------------------------------------------------
  test('M10a: every cargo-publish job transitively needs rehearse-publish-python', () => {
    const graph = buildNeedsGraph(yml);
    assert.ok(
      graph.has('rehearse-publish-python'),
      'non-vacuity: rehearse-publish-python must be a graph node; PF-013',
    );

    const allIds = findAllJobIds(yml);
    const cargoPublishJobs = allIds.filter(id => {
      // Use stripCommentLines and then look for lines where "cargo publish" is
      // the actual command being run (not in echo strings or error messages).
      // A line that is the command starts with leading whitespace then "cargo ",
      // as opposed to being inside an echo/error string.
      const section = stripCommentLines(extractJobSection(yml, id) ?? '');
      return section.split('\n').some(line => {
        const trimmed = line.trimStart();
        return trimmed.startsWith('cargo publish') ||
               // inside if/OUTPUT=$(...) patterns
               trimmed.includes('cargo publish -p ') ||
               // bare cargo publish invocation
               /^\s*cargo publish\b/.test(line);
      });
    });
    assert.ok(
      cargoPublishJobs.length > 0,
      'non-vacuity (PF-013): at least one job must contain "cargo publish"; found none',
    );

    // Positive control: a graph without the rehearsal edge must be flagged.
    const controlGraph = new Map([
      ['publish-crates', new Set(['version-gate'])],
      ['version-gate', new Set()],
      ['rehearse-publish-python', new Set(['build-python'])],
      ['build-python', new Set(['version-gate'])],
    ]);
    assert.ok(
      !transitivelyNeeds(controlGraph, 'publish-crates', 'rehearse-publish-python'),
      'positive control: graph without rehearsal edge must report NOT transitive (PF-013)',
    );

    for (const id of cargoPublishJobs) {
      assert.ok(
        transitivelyNeeds(graph, id, 'rehearse-publish-python'),
        `job "${id}" contains "cargo publish" but does not transitively need ` +
        `rehearse-publish-python — a failed GHCR or twine rehearsal cannot abort ` +
        `before the crates.io write (irreversible, PF-039)`,
      );
    }
  });

  // -------------------------------------------------------------------------
  // M10b: publish-crates must NOT transitively need publish-testpypi.
  // publish-testpypi is the opt-in TestPyPI leg and must never gate the tag
  // path — a failing or skipped TestPyPI upload must not block a release.
  //
  // Positive control: a graph with publish-crates → publish-testpypi must be
  // flagged as transitive (so we know the check can actually detect the edge).
  // -------------------------------------------------------------------------
  test('M10b: publish-crates must NOT transitively need publish-testpypi (opt-in leg must never gate the tag path)', () => {
    const graph = buildNeedsGraph(yml);
    assert.ok(
      graph.has('publish-crates'),
      'non-vacuity: publish-crates must be a graph node; PF-013',
    );
    assert.ok(
      graph.has('publish-testpypi'),
      'non-vacuity: publish-testpypi must be a graph node; PF-013',
    );

    // Positive control: adding the edge must make the check fire.
    const controlGraph = new Map([...graph].map(([k, v]) => [k, new Set(v)]));
    const pcNeeds = controlGraph.get('publish-crates') ?? new Set();
    pcNeeds.add('publish-testpypi');
    controlGraph.set('publish-crates', pcNeeds);
    assert.ok(
      transitivelyNeeds(controlGraph, 'publish-crates', 'publish-testpypi'),
      'positive control: graph with publish-crates → publish-testpypi edge must report transitive (PF-013)',
    );

    assert.ok(
      !transitivelyNeeds(graph, 'publish-crates', 'publish-testpypi'),
      'publish-crates must NOT transitively need publish-testpypi — the opt-in TestPyPI ' +
      'upload must never block a tag release (ADR-013)',
    );
  });

  // -------------------------------------------------------------------------
  // M10c: ADR-013 three-place rule — for every job id, a job-level if:
  // containing "refs/tags/v" or "inputs." implies its display name: is in
  // TIER_B_EXPECTED_SKIPPED, and every member of TIER_B_EXPECTED_SKIPPED is
  // such a guarded job. Set-equality with size 5.
  //
  // Positive control: a fake job section with a guarded if: and a name not in
  // the set must be flagged.
  // -------------------------------------------------------------------------
  test('M10c: ADR-013 three-place rule — guarded jobs and TIER_B_EXPECTED_SKIPPED are the same set (size 5)', () => {
    const allIds = findAllJobIds(yml);

    // Helper: extract job name field from a section.
    const jobNameOf = (text) => {
      const section = stripCommentLines(text);
      const m = /^    name:\s+(.+)$/m.exec(section);
      return m ? m[1].trim() : null;
    };

    // Collect guarded job names: jobs whose if: contains refs/tags/v or inputs.
    const guardedNames = new Set();
    for (const id of allIds) {
      const section = extractJobSection(yml, id) ?? '';
      const jobIf = extractJobIf(section);
      if (jobIf && (jobIf.includes('refs/tags/v') || jobIf.includes('inputs.'))) {
        const name = jobNameOf(section);
        if (name) guardedNames.add(name);
      }
    }

    // Positive control (PF-013): a fake section with guarded if: and unlisted
    // name must be detected as missing from TIER_B_EXPECTED_SKIPPED.
    const fakeSection = [
      '  fake-publish-somewhere:',
      "    if: ${{ startsWith(github.ref, 'refs/tags/v') }}",
      '    name: Publish to Somewhere Unlisted',
      '    runs-on: ubuntu-latest',
    ].join('\n');
    const fakeIf = extractJobIf(fakeSection);
    const fakeName = jobNameOf(fakeSection);
    assert.ok(fakeIf && fakeIf.includes('refs/tags/v'), 'positive control: extractJobIf must find the guarded if:');
    assert.ok(fakeName === 'Publish to Somewhere Unlisted', 'positive control: jobNameOf must read the name');
    assert.ok(!TIER_B_EXPECTED_SKIPPED.has(fakeName), 'positive control: unlisted name must not be in TIER_B_EXPECTED_SKIPPED');

    // TIER_B_EXPECTED_SKIPPED must equal the guarded-jobs set exactly.
    for (const name of guardedNames) {
      assert.ok(
        TIER_B_EXPECTED_SKIPPED.has(name),
        `job with guarded if: (refs/tags/v or inputs.) named "${name}" is not in ` +
        `TIER_B_EXPECTED_SKIPPED — a skipped run for this job would fail the Tier B verifier ` +
        `on a dry-run or dispatch run (ADR-013 three-place rule)`,
      );
    }
    for (const name of TIER_B_EXPECTED_SKIPPED) {
      assert.ok(
        guardedNames.has(name),
        `TIER_B_EXPECTED_SKIPPED contains "${name}" but no job in release.yml has a guarded ` +
        `if: (refs/tags/v or inputs.) and that display name — the set has drifted (ADR-013)`,
      );
    }
    assert.equal(
      guardedNames.size, 5,
      `expected exactly 5 guarded jobs (ADR-013 three-place rule); found ${guardedNames.size}: ` +
      `[${[...guardedNames].join(', ')}]`,
    );
  });

  // -------------------------------------------------------------------------
  // S19: no run: block in release.yml may contain the empty expression ${{ }}
  // (dollar-brace-brace-whitespace*-brace-brace). GitHub's expression
  // preprocessor scans run: block text WITHOUT stripping shell comments, and
  // the empty expression is a parse error that makes GitHub reject the entire
  // workflow with zero jobs emitted (run 34061583304 confirmed).
  //
  // Positive control: a planted string containing the pattern must be flagged.
  // -------------------------------------------------------------------------
  test('S19: no run: block in release.yml contains the empty GitHub expression ${{ }} (parse rejection guard)', () => {
    const EMPTY_EXPR = /\$\{\{\s*\}\}/;

    // Positive control (PF-013): the regex must match a planted occurrence.
    const planted = 'echo "Actions only interpolates ${{ }}, not bare {{ }}"';
    assert.ok(
      EMPTY_EXPR.test(planted),
      'positive control: the empty-expression regex must match the planted string; ' +
      'if this fails, the guard is broken',
    );

    // Count occurrences in the real file.
    const matches = yml.match(new RegExp(EMPTY_EXPR.source, 'g')) ?? [];
    assert.equal(
      matches.length, 0,
      `release.yml contains ${matches.length} occurrence(s) of the empty expression ` +
      '\\$\\{\\{\\s*\\}\\} — GitHub\'s parser rejects this even inside shell comments ' +
      `within run: blocks (run 34061583304, P-fix). Found at: ` +
      matches.map((_, i) => {
        const idx = yml.indexOf(matches[i] ?? '');
        const lineNum = yml.slice(0, idx).split('\n').length;
        return `line ~${lineNum}`;
      }).join(', '),
    );
  });

});

// ---------------------------------------------------------------------------
// B2: per-leg rust-cache helpers and S20 spec
// ---------------------------------------------------------------------------

/**
 * Returns one entry per Swatinem/rust-cache step in the comment-stripped job
 * section, with the `key:` value from its `with:` block, or null when absent.
 *
 * Steps are segmented FIRST — a step runs from its 6-space `- ` line to the
 * next one — and each segment is then tested for a `uses: Swatinem/rust-cache@`
 * line at ANY position. Anchoring detection on `- uses:` would miss a step
 * written `- name: …` / `  uses: Swatinem/rust-cache@…`, so a benign reorder
 * would silently stop S20 from gating that step (avoids PF-013). Reading the
 * key from the segment (rather than scanning forward until the next step)
 * likewise cannot borrow a `key:` belonging to a different step.
 */
function rustCacheSteps(jobSection) {
  const lines = stripCommentLines(jobSection).split('\n');
  const starts = [];
  for (let i = 0; i < lines.length; i++) {
    if (/^      - /.test(lines[i])) starts.push(i);
  }
  const steps = [];
  for (const [n, start] of starts.entries()) {
    const body = lines.slice(start, starts[n + 1] ?? lines.length);
    if (!body.some(l => /^\s*(- )?uses:\s*Swatinem\/rust-cache@/.test(l))) continue;
    let key = null;
    for (const line of body) {
      const km = /^\s+key:\s+(.+)$/.exec(line);
      if (km) { key = km[1].trim(); break; }
    }
    steps.push({ key });
  }
  return steps;
}

/**
 * True iff a 4-space `strategy:` key exists after comment stripping.
 * Matrix jobs declare `    strategy:` inside the job body.
 *
 * Trailing content is deliberately NOT anchored: `strategy:` followed by an
 * inline comment or written as a flow mapping must still count, or a matrix
 * job would be silently exempted from S20 (avoids PF-013).
 */
function hasMatrix(jobSection) {
  return stripCommentLines(jobSection).split('\n').some(l => /^    strategy:(\s|$)/.test(l));
}

describe('B2: per-leg rust-cache keys in matrix jobs (#352, PF-041)', () => {

  // -------------------------------------------------------------------------
  // S20: every Swatinem/rust-cache step in a matrix job must carry a `key:`
  // that includes `matrix.` so each leg's compiled artifacts stay isolated.
  //
  // Without a per-leg key the automatic key (job-id + runner-os/arch + rustc
  // host hash + lock hash) is SHARED across all legs on the same runner OS:
  // all four ubuntu legs restore each other's target/<triple>/ blobs, and both
  // macOS legs do the same (confirmed live: run 34065573775, every Linux leg
  // restored `v0-rust-build-napi-Linux-x64-6ff13d87-4c33221b`).
  // `build-python` already carries `key: matrix.target-matrix.manylinux` (#347);
  // this spec extends that gate to `build-napi` (#352, PF-041).
  //
  // Non-vacuity: build-napi and build-python must have rust-cache steps, and
  // publish-crates must NOT qualify (single-leg, no matrix → exempt).
  // Failure message names PF-041, #347, #352.
  // -------------------------------------------------------------------------
  test('S20: every rust-cache step in every matrix job carries a key containing matrix. (PF-041, #352)', () => {

    // --- Positive controls (PF-013) ---

    // PC1: bare step in a matrix job → key must be null
    const bareJobSection = [
      '  fake-matrix:',
      '    strategy:',
      '      matrix:',
      '        include:',
      '          - target: aarch64',
      '    steps:',
      '      - uses: Swatinem/rust-cache@v2',
      '      - name: Next step',
      '        run: echo done',
    ].join('\n');
    const bareSteps = rustCacheSteps(bareJobSection);
    assert.equal(bareSteps.length, 1, 'PC1: bare step must be detected');
    assert.equal(bareSteps[0].key, null, 'PC1: bare step key must be null');

    // PC2: static key → read back verbatim
    const staticKeySection = [
      '  fake-matrix:',
      '    strategy:',
      '      matrix:',
      '        include:',
      '          - target: aarch64',
      '    steps:',
      '      - uses: Swatinem/rust-cache@v2',
      '        with:',
      '          key: my-static-key',
      '      - name: Next step',
      '        run: echo done',
    ].join('\n');
    const staticSteps = rustCacheSteps(staticKeySection);
    assert.equal(staticSteps.length, 1, 'PC2: static-key step must be detected');
    assert.equal(staticSteps[0].key, 'my-static-key', 'PC2: static key must be read verbatim');

    // PC3: commented-out key → key must be null
    const commentedKeySection = [
      '  fake-matrix:',
      '    strategy:',
      '      matrix:',
      '        include:',
      '          - target: aarch64',
      '    steps:',
      '      - uses: Swatinem/rust-cache@v2',
      '        # with:',
      '        #   key: ${{ matrix.target }}',
      '      - name: Next step',
      '        run: echo done',
    ].join('\n');
    const commentedSteps = rustCacheSteps(commentedKeySection);
    assert.equal(commentedSteps.length, 1, 'PC3: step with commented key must be detected');
    assert.equal(commentedSteps[0].key, null, 'PC3: commented-out key must yield null');

    // PC4: matrix-derived key → read back verbatim and contains matrix.
    const matrixKeySection = [
      '  fake-matrix:',
      '    strategy:',
      '      matrix:',
      '        include:',
      '          - target: aarch64',
      '    steps:',
      '      - uses: Swatinem/rust-cache@v2',
      '        with:',
      '          key: ${{ matrix.settings.target }}',
      '      - name: Next step',
      '        run: echo done',
    ].join('\n');
    const matrixSteps = rustCacheSteps(matrixKeySection);
    assert.equal(matrixSteps.length, 1, 'PC4: matrix-key step must be detected');
    assert.equal(
      matrixSteps[0].key, '${{ matrix.settings.target }}',
      'PC4: matrix key must be read back verbatim',
    );
    assert.ok(matrixSteps[0].key.includes('matrix.'), 'PC4: matrix key must contain "matrix."');

    // PC5: job with strategy: only in a comment → not a matrix job (exempt)
    const commentedStrategySection = [
      '  fake-single:',
      '    # strategy: not a real matrix',
      '    steps:',
      '      - uses: Swatinem/rust-cache@v2',
    ].join('\n');
    assert.ok(
      !hasMatrix(commentedStrategySection),
      'PC5: a job with strategy: only in a comment must not be treated as a matrix job',
    );

    // PC5b: `strategy:` carrying trailing content (inline comment, flow mapping)
    // must STILL count as a matrix job — anchoring on end-of-line would exempt a
    // real matrix job from S20 without any test failing (avoids PF-013).
    assert.ok(
      hasMatrix(['  fake:', '    strategy:  # fail-fast tuned below', '    steps:'].join('\n')),
      'PC5b: strategy: with a trailing inline comment must still be a matrix job',
    );
    assert.ok(
      hasMatrix(['  fake:', '    strategy: { matrix: { target: [a, b] } }', '    steps:'].join('\n')),
      'PC5b: strategy: written as a flow mapping must still be a matrix job',
    );

    // PC6: a rust-cache step whose `uses:` is NOT the first key must still be
    // detected, and its key read. Anchoring detection on `- uses:` would let a
    // benign reorder (adding a `name:`) silently disable S20 for that step.
    const nameFirstSection = [
      '  fake-matrix:',
      '    strategy:',
      '      matrix:',
      '        include:',
      '          - target: aarch64',
      '    steps:',
      '      - name: Cache Rust artifacts',
      '        uses: Swatinem/rust-cache@v2',
      '        with:',
      '          key: ${{ matrix.target }}',
      '      - name: Next step',
      '        run: echo done',
    ].join('\n');
    const nameFirstSteps = rustCacheSteps(nameFirstSection);
    assert.equal(nameFirstSteps.length, 1,
      'PC6: a rust-cache step whose uses: is not the first key must still be detected');
    assert.equal(nameFirstSteps[0].key, '${{ matrix.target }}',
      'PC6: key must be read from a step whose uses: is not the first key');

    // PC7 (negative): a `key:` that belongs to a LATER, non-rust-cache step must
    // not be borrowed by a bare rust-cache step that precedes it.
    const borrowedKeySection = [
      '  fake-matrix:',
      '    strategy:',
      '      matrix:',
      '        include:',
      '          - target: aarch64',
      '    steps:',
      '      - uses: Swatinem/rust-cache@v2',
      '      - uses: actions/cache@v4',
      '        with:',
      '          key: someone-elses-key',
    ].join('\n');
    const borrowedSteps = rustCacheSteps(borrowedKeySection);
    assert.equal(borrowedSteps.length, 1, 'PC7: only the rust-cache step must be collected');
    assert.equal(borrowedSteps[0].key, null,
      'PC7: a bare rust-cache step must not borrow the next step\'s key');

    // --- Non-vacuity: confirm checked set membership ---

    const buildNapiSection = extractJobSection(yml, 'build-napi');
    assert.ok(buildNapiSection !== null, 'non-vacuity: build-napi must exist');
    assert.ok(
      rustCacheSteps(buildNapiSection).length > 0,
      'non-vacuity: build-napi must have at least one Swatinem/rust-cache step',
    );
    assert.ok(hasMatrix(buildNapiSection), 'non-vacuity: build-napi must be a matrix job');

    const buildPythonSection = extractJobSection(yml, 'build-python');
    assert.ok(buildPythonSection !== null, 'non-vacuity: build-python must exist');
    assert.ok(
      rustCacheSteps(buildPythonSection).length > 0,
      'non-vacuity: build-python must have at least one Swatinem/rust-cache step',
    );
    assert.ok(hasMatrix(buildPythonSection), 'non-vacuity: build-python must be a matrix job');

    // publish-crates must NOT qualify (single-leg, exempt)
    const publishCratesSection = extractJobSection(yml, 'publish-crates');
    assert.ok(publishCratesSection !== null, 'non-vacuity: publish-crates must exist');
    assert.ok(
      !hasMatrix(publishCratesSection),
      'non-vacuity: publish-crates must NOT be a matrix job (single-leg, exempt from S20)',
    );

    // --- Core assertion: every rust-cache step in every matrix job has a matrix. key ---
    const matrixJobIds = findAllJobIds(yml).filter(id => {
      const section = extractJobSection(yml, id);
      return section !== null && hasMatrix(section);
    });

    // Non-vacuity: checked set must include build-napi and build-python
    assert.ok(
      matrixJobIds.includes('build-napi'),
      'S20 non-vacuity: build-napi must be in the matrix job set',
    );
    assert.ok(
      matrixJobIds.includes('build-python'),
      'S20 non-vacuity: build-python must be in the matrix job set',
    );

    for (const id of matrixJobIds) {
      const section = extractJobSection(yml, id);
      const steps = rustCacheSteps(section);
      for (const step of steps) {
        assert.ok(
          step.key !== null && step.key.includes('matrix.'),
          `Matrix job "${id}" has a Swatinem/rust-cache step whose key is ` +
          `${JSON.stringify(step.key)} — without a per-leg key all legs on the same ` +
          `runner OS restore each other's target/<triple>/ artifacts (PF-041, confirmed ` +
          `live in run 34065573775). build-python already keys on matrix.target + ` +
          `matrix.manylinux (#347); build-napi must key on matrix.settings.target (#352). ` +
          `Fix: add \`with:\\n            key: \${{ matrix.settings.target }}\` under the step.`,
        );
      }
    }
  });

});

// ---------------------------------------------------------------------------
// B3a: Alpine musl load-test helpers and S21 spec (#340)
//
// Two `docker run node:22-alpine` steps prove the musl addons actually dlopen
// on Alpine — x64 as the last step of stage-and-verify-napi, arm64 in a new
// unguarded job load-test-musl-arm64 on a native ubuntu-24.04-arm runner.
// The probe script is scripts/musl-load-probe.cjs.
// ---------------------------------------------------------------------------

/**
 * Extract the `runs-on:` value from a job section (4-space indent).
 * Returns the trimmed value string, or null when absent.
 */
function runsOnOf(section) {
  const m = /^    runs-on:\s+(.+)$/m.exec(section);
  return m ? m[1].trim() : null;
}

/**
 * Return the `run: |` block text for the step whose segment contains
 * `name: "Alpine load test (`. Segments steps exactly like `rustCacheSteps`
 * (comment-stripped, `/^      - /` boundaries), finds the load-test step, and
 * returns the lines after `run: |` that are indented deeper than the `run:`
 * key, joined with '\n'. Returns null when no such step or run block is found.
 *
 * Trailing blank lines are dropped before joining. When the load-test step is
 * the LAST step of its job, the scan runs to the end of the job section, so the
 * blank separator lines between that step and the next job's comment banner
 * (the banner itself is removed by stripCommentLines) would otherwise land
 * inside the returned block. That would make the byte-equality assertion below
 * sensitive to blank lines OUTSIDE either run block — a purely cosmetic edit to
 * one job's spacing would fail S21 with "must be BYTE-EQUAL", a true verdict for
 * a false reason. Blank lines are not shell code; only the script text is
 * compared. Control PC-I pins both halves: trailing blanks are ignored, and a
 * real trailing command difference is still detected.
 */
function loadTestRunBlock(section) {
  const lines = stripCommentLines(section).split('\n');
  const starts = [];
  for (let i = 0; i < lines.length; i++) {
    if (/^      - /.test(lines[i])) starts.push(i);
  }
  for (const [n, start] of starts.entries()) {
    const body = lines.slice(start, starts[n + 1] ?? lines.length);
    if (!body.some(l => l.includes('name: "Alpine load test ('))) continue;
    const runIdx = body.findIndex(l => /^\s+run: \|/.test(l));
    if (runIdx === -1) return null;
    const runLineIndent = (body[runIdx].match(/^(\s*)/) ?? ['', ''])[1].length;
    const runLines = [];
    for (let i = runIdx + 1; i < body.length; i++) {
      const line = body[i];
      if (line.trim() === '') { runLines.push(line); continue; }
      const lineIndent = (line.match(/^(\s*)/) ?? ['', ''])[1].length;
      if (lineIndent <= runLineIndent) break;
      runLines.push(line);
    }
    while (runLines.length > 0 && runLines[runLines.length - 1].trim() === '') runLines.pop();
    return runLines.join('\n');
  }
  return null;
}

/**
 * Return the 0-based index of the step (in the comment-stripped section)
 * whose segment contains `needle`, or -1 when not found.
 * Steps are segmented at `/^      - /` boundaries, matching `rustCacheSteps`.
 */
function stepIndexOf(section, needle) {
  const lines = stripCommentLines(section).split('\n');
  const starts = [];
  for (let i = 0; i < lines.length; i++) {
    if (/^      - /.test(lines[i])) starts.push(i);
  }
  for (const [n, start] of starts.entries()) {
    const body = lines.slice(start, starts[n + 1] ?? lines.length);
    if (body.some(l => l.includes(needle))) return n;
  }
  return -1;
}

/**
 * Return the env: key-value lines for the Alpine load test step in the given
 * section, comment-stripped, as "KEY: value" strings joined by '\n'.
 * Returns null when the step or env block is absent.
 */
function loadTestEnvBlock(section) {
  const lines = stripCommentLines(section).split('\n');
  const starts = [];
  for (let i = 0; i < lines.length; i++) {
    if (/^      - /.test(lines[i])) starts.push(i);
  }
  for (const [n, start] of starts.entries()) {
    const body = lines.slice(start, starts[n + 1] ?? lines.length);
    if (!body.some(l => l.includes('name: "Alpine load test ('))) continue;
    const envIdx = body.findIndex(l => /^\s+env:\s*$/.test(l));
    if (envIdx === -1) return null;
    const envIndent = (body[envIdx].match(/^(\s*)/) ?? ['', ''])[1].length;
    const envLines = [];
    for (let i = envIdx + 1; i < body.length; i++) {
      const line = body[i];
      if (line.trim() === '') break;
      const lineIndent = (line.match(/^(\s*)/) ?? ['', ''])[1].length;
      if (lineIndent <= envIndent) break;
      envLines.push(line.trim());
    }
    return envLines.join('\n');
  }
  return null;
}

describe('B3a: Alpine musl load tests (#340)', () => {

  // -------------------------------------------------------------------------
  // S21: real Alpine dlopen tests for musl napi addons (#340).
  //
  // The release pipeline cross-compiles two musl addons (linux-x64-musl,
  // linux-arm64-musl). The existing readelf gate proves ELF metadata but cannot
  // prove the addon dlopens on Alpine. Two `docker run node:22-alpine` steps
  // close this gap using scripts/musl-load-probe.cjs.
  //
  // Ordering invariant (PF-047): load-test-musl-arm64 must be in publish-crates
  // needs: AND its result == 'success' must be in the if: conjunct. With
  // !cancelled() present, needs: is ordering-only — only the if: conjunct gates.
  //
  // ADR-013 step-level-guard rule: the arm64 job must reach success on PRs so
  // the mandatory verifier counts it. A job-level tag/input guard would make it
  // skip, producing a skipped conclusion the verifier rejects.
  //
  // PF-013: parser controls run on planted YAML first; absence-only checks are
  // vacuous. Failure message names #340, ADR-013, PF-013, PF-023.
  // -------------------------------------------------------------------------
  test('S21: load-test-musl-arm64 job exists, is unguarded, wired into publish-crates, run blocks match (PF-047, ADR-013, #340)', () => {

    // -----------------------------------------------------------------------
    // Parser positive controls (PF-013) — all run against planted YAML strings,
    // not the real release.yml. All must pass in both RED and GREEN states.
    // -----------------------------------------------------------------------

    // S21/PC-A: loadTestRunBlock finds a planted Alpine load test step and
    // returns its run block content. Proves the helper does not always return null.
    const plantedWithLoadTest = [
      '  fake-job:',
      '    steps:',
      '      - name: "Alpine load test (linux-x64-musl)"',
      '        env:',
      '          ALPINE_IMAGE: node:22-alpine',
      '        run: |',
      '          set -euo pipefail',
      '          docker run --network none :/w:ro',
      '      - name: Next step',
      '        run: echo done',
    ].join('\n');
    const plantedRunBlock = loadTestRunBlock(plantedWithLoadTest);
    assert.ok(
      plantedRunBlock !== null,
      'S21/PC-A: loadTestRunBlock must find a planted Alpine load test step (PF-013)',
    );
    assert.ok(
      plantedRunBlock.includes('set -euo pipefail'),
      'S21/PC-A: returned run block must include the planted run block content',
    );

    // S21/PC-B: "positive control" appearing ONLY in a comment yields zero
    // matching lines after stripCommentLines. Proves the strip is effective.
    const plantedCommentSection = [
      '  fake-job:',
      '    steps:',
      '      - name: fake step',
      '        run: |',
      '          # positive control: this line is a comment',
      '          echo "all clear"',
    ].join('\n');
    const pcLinesAfterStrip = stripCommentLines(plantedCommentSection)
      .split('\n').filter(l => l.includes('positive control'));
    assert.equal(
      pcLinesAfterStrip.length, 0,
      'S21/PC-B: "positive control" in a comment only must yield zero matching lines ' +
      'after stripCommentLines (PF-013)',
    );

    // S21/PC-C: runsOnOf returns the correct value, and the equality check against
    // the required runner fails for the wrong runner name.
    const plantedArm64Section = '  fake-job:\n    runs-on: ubuntu-24.04-arm64';
    assert.equal(
      runsOnOf(plantedArm64Section), 'ubuntu-24.04-arm64',
      'S21/PC-C: runsOnOf must parse the runs-on value from a planted section',
    );
    assert.notEqual(
      runsOnOf(plantedArm64Section), 'ubuntu-24.04-arm',
      'S21/PC-C: ubuntu-24.04-arm64 must not equal ubuntu-24.04-arm (the required runner)',
    );

    // S21/PC-D: a needs-graph WITHOUT the publish-crates -> load-test-musl-arm64
    // edge makes transitivelyNeeds return false. Proves the edge is load-bearing
    // and the check cannot be trivially satisfied. (PF-047)
    const controlGraphMissingEdge = new Map([
      ['publish-crates', ['stage-and-verify-napi', 'version-gate']],
      ['load-test-musl-arm64', ['stage-and-verify-napi']],
      ['stage-and-verify-napi', ['build-napi']],
      ['version-gate', []],
      ['build-napi', ['version-gate']],
    ]);
    assert.ok(
      !transitivelyNeeds(controlGraphMissingEdge, 'publish-crates', 'load-test-musl-arm64'),
      'S21/PC-D: a graph without the publish-crates -> load-test-musl-arm64 edge must ' +
      'report NOT transitive (PF-047, PF-013)',
    );

    // S21/PC-E: a publish-crates-shaped if: string lacking the load-test result
    // conjunct is detectable. With !cancelled(), needs: is ordering-only — only
    // the if: conjunct gates the job (PF-047).
    const incompleteIf =
      "${{ !cancelled() && needs.stage-and-verify-napi.result == 'success'" +
      " && startsWith(github.ref, 'refs/tags/v') }}";
    assert.ok(
      !incompleteIf.includes("needs.load-test-musl-arm64.result == 'success'"),
      'S21/PC-E: a publish-crates if: without the load-test result conjunct must be ' +
      'flagged as incomplete (PF-047)',
    );

    // S21/PC-F: two run blocks differing by exactly one character are unequal.
    const runBlockA = 'set -euo pipefail\n  echo "hello alpine"';
    const runBlockB = 'set -euo pipefail\n  echo "hello alpinex"';
    assert.notEqual(runBlockA, runBlockB,
      'S21/PC-F: run blocks differing by one character must be unequal');

    // S21/PC-G: a step text missing --network none is detectable; the same
    // planted text also lacks timeout 600 docker run (pin E2, #340, PF-013).
    const missingNetwork = 'docker run --rm :/w:ro --pull=never alpine sh';
    assert.ok(
      !missingNetwork.includes('--network none'),
      'S21/PC-G: a step text missing --network none must be detectable (PF-013)',
    );
    assert.ok(
      !missingNetwork.includes('timeout 600 docker run'),
      'S21/PC-G: a step text missing timeout 600 docker run must be detectable (PF-013, #340)',
    );

    // S21/PC-H: extractNeeds strips comment lines before matching (hardening).
    // A `# needs: [bogus]` comment above `    needs: [real-dep]` must yield
    // ['real-dep'], not ['bogus']. (stripCommentLines call added to extractNeeds)
    const commentedNeedsSection = [
      '  fake-job:',
      '    # needs: [bogus]',
      '    needs: [real-dep]',
      '    steps:',
    ].join('\n');
    assert.deepEqual(
      extractNeeds(commentedNeedsSection), ['real-dep'],
      'S21/PC-H: extractNeeds must strip comment lines; # needs: [bogus] above ' +
      'needs: [real-dep] must yield [\'real-dep\'] (PF-013)',
    );

    // S21/PC-I: the byte-equality comparison below must ignore blank lines that
    // sit OUTSIDE the run block — when the load-test step is the last step of a
    // job, the scan reaches the end of the section and would otherwise absorb
    // the blank separator before the next job's (comment-stripped) banner. Both
    // halves are pinned so the trim cannot silently swallow a divergent script.
    const plantLoadTestStep = (tail) => [
      '  fake-job:',
      '    steps:',
      '      - name: "Alpine load test (linux-x64-musl)"',
      '        run: |',
      '          set -euo pipefail',
      '          docker run --network none :/w:ro',
      ...tail,
    ].join('\n');
    assert.equal(
      loadTestRunBlock(plantLoadTestStep([])),
      loadTestRunBlock(plantLoadTestStep(['', '', ''])),
      'S21/PC-I: two run blocks differing ONLY in trailing blank lines must compare ' +
      'EQUAL — a blank separator outside the block is not shell code, and letting it ' +
      'in makes S21 fail for a cosmetic edit to an unrelated job (PF-013)',
    );
    assert.notEqual(
      loadTestRunBlock(plantLoadTestStep([])),
      loadTestRunBlock(plantLoadTestStep(['          echo extra', ''])),
      'S21/PC-I: a run block carrying a real extra trailing COMMAND must still compare ' +
      'UNEQUAL — the trailing-blank trim must not swallow a divergent script (PF-013)',
    );

    // S21/PC-J: a section without 'Upload staged napi tree' yields stepIndexOf === -1,
    // so the non-vacuity guard on the step-ordering check is demonstrably reachable —
    // renaming that step cannot make the ordering check pass vacuously (PF-013, #340).
    const sectionWithoutUpload = [
      '  fake-job:',
      '    steps:',
      '      - name: "Alpine load test (linux-x64-musl)"',
      '        run: |',
      '          echo hi',
    ].join('\n');
    assert.strictEqual(
      stepIndexOf(sectionWithoutUpload, 'name: Upload staged napi tree'),
      -1,
      'S21/PC-J: stepIndexOf must return -1 when "Upload staged napi tree" is absent (PF-013, #340)',
    );

    // S21/PC-K: a planted upload step without if-no-files-found: error is flagged by
    // the pin-E1 assertion below (the staged upload must fail loudly on an empty tree; #340).
    const uploadStepWithoutIfNoFiles = [
      '  fake-job:',
      '    steps:',
      '      - name: Upload staged napi tree',
      '        uses: actions/upload-artifact@v7',
      '        with:',
      '          name: napi-staged',
    ].join('\n');
    assert.ok(
      !uploadStepWithoutIfNoFiles.includes('if-no-files-found: error'),
      'S21/PC-K: a planted upload step without if-no-files-found: error must not include it (PF-013, #340)',
    );

    // S21/PC-L: a planted arm64-shaped load-test step with PLATFORM: linux-x64-musl is
    // detectable — an arch flip would only fail at runtime (#340, PF-013).
    const plantedArmWithWrongPlatform = [
      '  load-test-musl-arm64:',
      '    steps:',
      '      - name: "Alpine load test (linux-arm64-musl)"',
      '        env:',
      '          ALPINE_IMAGE: node:22-alpine',
      '          PLATFORM: linux-x64-musl',
      '          ARCHKEY: linux-arm64',
      '          NPM_DIR: staged/npm',
      '        run: |',
      '          echo hi',
    ].join('\n');
    const plantedArmEnv = loadTestEnvBlock(plantedArmWithWrongPlatform);
    assert.ok(
      plantedArmEnv !== null && !plantedArmEnv.includes('PLATFORM: linux-arm64-musl'),
      'S21/PC-L: a planted arm64 load-test step with PLATFORM: linux-x64-musl must not ' +
      'contain PLATFORM: linux-arm64-musl — demonstrating the per-arch env check is reachable (#340, PF-013)',
    );

    // -----------------------------------------------------------------------
    // Real-file assertions (S21) — these fail in the RED state because
    // load-test-musl-arm64 does not exist in release.yml yet (#340, Phase A2).
    // -----------------------------------------------------------------------

    // S21: job must exist. Without it, linux-arm64-musl is never proven to dlopen
    // on Alpine before an irreversible crates.io publish (PF-023, ADR-013).
    const arm64JobSection = extractJobSection(yml, 'load-test-musl-arm64');
    assert.ok(
      arm64JobSection !== null,
      'S21: load-test-musl-arm64 job must exist in release.yml. ' +
      'This unguarded job runs real Alpine load tests on a native ubuntu-24.04-arm ' +
      'runner (no QEMU) and must reach success on every PR/dispatch run (ADR-013). ' +
      'Without it, linux-arm64-musl addon is never proven to dlopen on Alpine before ' +
      'an irreversible crates.io publish (PF-023, #340). Phase A2 adds this job.',
    );

    assert.ok(
      arm64JobSection.includes('name: Alpine load test (linux-arm64-musl)'),
      'S21: load-test-musl-arm64 must declare name: Alpine load test (linux-arm64-musl)',
    );

    // Must run on the native arm64 runner (no QEMU/cross-emulation).
    assert.equal(
      runsOnOf(arm64JobSection), 'ubuntu-24.04-arm',
      'S21: load-test-musl-arm64 must declare runs-on: ubuntu-24.04-arm (native arm64)',
    );

    // Must be unguarded at job level (ADR-013 step-level-guard rule: the job must
    // reach success on every PR so the mandatory verifier can count it as success;
    // a job-level tag/input guard makes it skip, which the Tier-B verifier rejects).
    const arm64If = extractJobIf(arm64JobSection);
    assert.ok(
      arm64If === null ||
        (!arm64If.includes('refs/tags') &&
         !arm64If.includes('startsWith(github.ref') &&
         !arm64If.includes('inputs.')),
      'S21: load-test-musl-arm64 must not have a job-level if: guarded on refs/tags, ' +
      'startsWith(github.ref), or inputs. — a tag/input guard would skip the job on PRs, ' +
      'producing a skipped conclusion the Tier-B verifier rejects (ADR-013)',
    );

    // Must not use container: (x64-only for JS actions) or QEMU.
    const arm64Stripped = stripCommentLines(arm64JobSection);
    assert.ok(
      !arm64Stripped.split('\n').some(l => /^    container:/.test(l)),
      'S21: load-test-musl-arm64 must not declare a job-level container: ' +
      '(container: is x64-only for JS actions; use docker run from the host job)',
    );
    assert.ok(!arm64Stripped.includes('setup-qemu'),
      'S21: load-test-musl-arm64 must not use setup-qemu (native runner eliminates QEMU)');
    assert.ok(!arm64Stripped.includes('--platform'),
      'S21: load-test-musl-arm64 must not pass --platform to docker (native runner)');

    // Required structural fields.
    assert.ok(arm64JobSection.includes('timeout-minutes: 15'),
      'S21: load-test-musl-arm64 must declare timeout-minutes: 15');
    assert.ok(arm64JobSection.includes('contents: read'),
      'S21: load-test-musl-arm64 must declare permissions: contents: read');
    assert.ok(arm64JobSection.includes('uses: actions/checkout@'),
      'S21: load-test-musl-arm64 must include a checkout step');
    assert.ok(arm64JobSection.includes('name: napi-staged'),
      'S21: load-test-musl-arm64 must download the napi-staged artifact');

    // Needs must be exactly [stage-and-verify-napi].
    assert.deepEqual(
      extractNeeds(arm64JobSection), ['stage-and-verify-napi'],
      'S21: load-test-musl-arm64 needs must be exactly [stage-and-verify-napi]',
    );

    // --- Wiring (PF-047 guard) ---
    // publish-crates must list load-test-musl-arm64 in BOTH needs: AND if:.
    // With !cancelled(), needs: is ordering-only; only the if: conjunct gates
    // the irreversible cargo publish (PF-047, PF-023).

    const publishCratesSection = extractJobSection(yml, 'publish-crates');
    assert.ok(publishCratesSection !== null, 'S21 non-vacuity: publish-crates must exist');

    assert.ok(
      extractNeeds(publishCratesSection).includes('load-test-musl-arm64'),
      'S21: publish-crates needs: must include load-test-musl-arm64 (PF-047, #340)',
    );

    assert.ok(
      transitivelyNeeds(buildNeedsGraph(yml), 'publish-crates', 'load-test-musl-arm64'),
      'S21: publish-crates must transitively need load-test-musl-arm64 (#340, PF-047)',
    );

    const publishCratesIf = extractJobIf(publishCratesSection);
    assert.ok(
      publishCratesIf !== null &&
        publishCratesIf.includes("needs.load-test-musl-arm64.result == 'success'"),
      'S21: publish-crates if: must include needs.load-test-musl-arm64.result == \'success\'. ' +
      'With !cancelled() present, needs: is ordering-only — the if: conjunct is the real gate ' +
      'preventing an irreversible cargo publish when the Alpine load test failed ' +
      '(PF-047, PF-023, #340).',
    );

    // --- Step content checks ---

    const stageSection = extractJobSection(yml, 'stage-and-verify-napi');
    assert.ok(stageSection !== null, 'S21 non-vacuity: stage-and-verify-napi must exist');

    // The two job sections must be distinct strings (sanity check).
    assert.notEqual(stageSection, arm64JobSection,
      'S21: stage-and-verify-napi and load-test-musl-arm64 sections must be distinct');

    // Both must contain an Alpine load test step with a run block.
    const stageRunBlock = loadTestRunBlock(stageSection);
    const arm64RunBlock = loadTestRunBlock(arm64JobSection);
    assert.ok(stageRunBlock !== null,
      'S21: stage-and-verify-napi must contain an Alpine load test step with run: |');
    assert.ok(arm64RunBlock !== null,
      'S21: load-test-musl-arm64 must contain an Alpine load test step with run: |');

    // Run blocks must be BYTE-EQUAL — two divergent scripts create two failure modes.
    assert.equal(stageRunBlock, arm64RunBlock,
      'S21: Alpine load test run blocks in stage-and-verify-napi and load-test-musl-arm64 ' +
      'must be BYTE-EQUAL (#340)');

    // Validate required content in both run blocks (comment-stripped).
    const needles = [
      '--network none',
      ':/w:ro',
      '--pull=never',
      'timeout 300',
      'timeout 600 docker run',
      'probe.cjs',
      'NODE_PATH=',
    ];
    // Non-vacuity: the needle list must be non-empty.
    assert.ok(needles.length > 0, 'S21 non-vacuity: needle list must be non-empty');

    for (const block of [stageRunBlock, arm64RunBlock]) {
      const strippedBlock = stripCommentLines(block);
      for (const needle of needles) {
        assert.ok(strippedBlock.includes(needle),
          `S21: Alpine load test run block must contain "${needle}" in executable code (#340)`);
      }
      // At least 2 executable lines containing "positive control" (PF-013: both a
      // probe-level and a fixture-level control must be present).
      const pcLines = strippedBlock.split('\n').filter(l => l.includes('positive control'));
      assert.ok(pcLines.length >= 2,
        `S21: Alpine load test run block must have at least 2 executable lines containing ` +
        `"positive control" (PF-013); found ${pcLines.length}`);
      // Safety: neither $PWD:/w nor GITHUB_WORKSPACE:/w (prevents host-path leakage).
      assert.ok(!strippedBlock.includes('$PWD:/w'),
        'S21: Alpine load test run block must not use $PWD:/w');
      assert.ok(!strippedBlock.includes('GITHUB_WORKSPACE:/w'),
        'S21: Alpine load test run block must not use GITHUB_WORKSPACE:/w');
    }

    // Each load-test step env must include ALPINE_IMAGE: node:22-alpine.
    assert.ok(stageSection.includes('ALPINE_IMAGE: node:22-alpine'),
      'S21: stage-and-verify-napi Alpine load test step env must include ALPINE_IMAGE: node:22-alpine');
    assert.ok(arm64JobSection.includes('ALPINE_IMAGE: node:22-alpine'),
      'S21: load-test-musl-arm64 step env must include ALPINE_IMAGE: node:22-alpine');

    // Pin E1: the staged upload must fail loudly on an empty tree (#340).
    assert.ok(
      stageSection.includes('if-no-files-found: error'),
      'S21 Pin E1: stage-and-verify-napi must contain if-no-files-found: error — ' +
      'the staged upload must fail loudly when the napi tree is empty (#340)',
    );

    // Per-arch env values — an arch flip would only fail at runtime (#340).
    const stageEnv = loadTestEnvBlock(stageSection);
    assert.ok(stageEnv !== null,
      'S21: stage-and-verify-napi Alpine load test step must have an env: block');
    assert.ok(stageEnv.includes('PLATFORM: linux-x64-musl'),
      'S21: stage-and-verify-napi load-test env must set PLATFORM: linux-x64-musl (#340)');
    assert.ok(stageEnv.includes('ARCHKEY: linux-x64'),
      'S21: stage-and-verify-napi load-test env must set ARCHKEY: linux-x64 (#340)');
    assert.ok(stageEnv.includes('NPM_DIR: crates/mds-napi/npm'),
      'S21: stage-and-verify-napi load-test env must set NPM_DIR: crates/mds-napi/npm (#340)');

    const arm64Env = loadTestEnvBlock(arm64JobSection);
    assert.ok(arm64Env !== null,
      'S21: load-test-musl-arm64 Alpine load test step must have an env: block');
    assert.ok(arm64Env.includes('PLATFORM: linux-arm64-musl'),
      'S21: load-test-musl-arm64 load-test env must set PLATFORM: linux-arm64-musl (#340)');
    assert.ok(arm64Env.includes('ARCHKEY: linux-arm64'),
      'S21: load-test-musl-arm64 load-test env must set ARCHKEY: linux-arm64 (#340)');
    assert.ok(arm64Env.includes('NPM_DIR: staged/npm'),
      'S21: load-test-musl-arm64 load-test env must set NPM_DIR: staged/npm (#340)');

    // In stage-and-verify-napi, the load-test step must come AFTER Upload staged napi tree.
    const uploadStepIdx = stepIndexOf(stageSection, 'name: Upload staged napi tree');
    const loadTestStepIdx = stepIndexOf(stageSection, 'name: "Alpine load test (');
    // Non-vacuity: both steps must exist; -1 > -1 is false but N > -1 holds for any N >= 0,
    // making the ordering check vacuous when the upload step is renamed (PF-013, #340).
    assert.ok(
      uploadStepIdx !== -1 && loadTestStepIdx !== -1,
      'S21 non-vacuity: "Upload staged napi tree" and Alpine load test steps must both ' +
      'exist in stage-and-verify-napi (stepIndexOf returns -1 when absent; a missing ' +
      'upload step would let N > -1 pass vacuously; #340, PF-013)',
    );
    // Exact-name check: stepIndexOf uses substring matching, so a suffix like " (v2)"
    // would still return a non-(-1) index — this end-of-line regex catches any suffix rename
    // (#340, PF-013). The YAML step line is "      - name: Upload staged napi tree" so the
    // regex anchors to EOL (no trailing chars after the name).
    assert.ok(
      /name: Upload staged napi tree\s*$/m.test(stageSection),
      'S21 non-vacuity: stage-and-verify-napi must contain a step named exactly ' +
      '"Upload staged napi tree" — a rename like (v2) bypasses the stepIndexOf check ' +
      'via substring matching but is caught here (#340, PF-013)',
    );
    assert.ok(
      loadTestStepIdx > uploadStepIdx,
      `S21: in stage-and-verify-napi the Alpine load test step (index ${loadTestStepIdx}) ` +
      `must come AFTER the Upload staged napi tree step (index ${uploadStepIdx}) so the ` +
      'artifact is available before the container mounts it (#340)',
    );

    // RELEASE_SURFACE must include scripts/musl-load-probe.cjs so that S10 forces
    // it into on.pull_request.paths (ADR-013 three-place rule: probe changes must
    // trigger the release rehearsal on PRs).
    assert.ok(
      RELEASE_SURFACE.includes('scripts/musl-load-probe.cjs'),
      'S21: RELEASE_SURFACE must include "scripts/musl-load-probe.cjs" so changes to the ' +
      'probe trigger release.yml on release-surface PRs (ADR-013, #340). Add the path to ' +
      'RELEASE_SURFACE in verify-pr-checks.mjs AND to on.pull_request.paths in release.yml.',
    );

    // Non-vacuity: job id must appear in the full job list.
    assert.ok(
      findAllJobIds(yml).includes('load-test-musl-arm64'),
      'S21 non-vacuity: load-test-musl-arm64 must appear in findAllJobIds output (#340)',
    );
  });

});
