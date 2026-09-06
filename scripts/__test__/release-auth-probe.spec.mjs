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
  for (const line of jobSection.split('\n')) {
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
  // S6: rehearse-publish-python must have NO job-level if: guard.
  // The job must run on pull_request, workflow_dispatch, AND tag push so that
  // the OIDC exchange is validated before any irreversible crates.io publish.
  // A job-level tag guard would re-introduce PF-039 for this job.
  // -------------------------------------------------------------------------
  test('S6: rehearse-publish-python has no job-level if: (runs on PR, dispatch, and tag push)', () => {
    const section = extractJobSection(yml, 'rehearse-publish-python');
    assert.ok(section !== null, 'rehearse-publish-python must exist (see S5)');
    const jobIf = extractJobIf(section);
    assert.equal(
      jobIf, null,
      'rehearse-publish-python must have NO job-level if: guard — it must run on all ' +
      'triggering events (pull_request, workflow_dispatch, push) so PRs exercise the ' +
      'OIDC exchange before the irreversible crates.io publish (PF-039); ' +
      `got if: ${jobIf}`,
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
  // S8: version-gate must contain a GHCR manifest probe for the
  // pypa/gh-action-pypi-publish pin (PF-040, #350 option 3).
  // An absent or stale pin (SHA instead of tag name) silently breaks the
  // publish-python job at runtime with "manifest unknown".
  // -------------------------------------------------------------------------
  test('S8: version-gate contains GHCR manifest probe for pypa/gh-action-pypi-publish', () => {
    const section = extractJobSection(yml, 'version-gate');
    assert.ok(section !== null, 'version-gate must exist');
    assert.ok(
      section.includes('ghcr.io/v2/pypa/gh-action-pypi-publish/manifests'),
      'version-gate must probe the GHCR manifest for the pypa/gh-action-pypi-publish ' +
      'pin (#350 option 3, PF-040); a missing or SHA-pinned image causes publish-python ' +
      'to fail at runtime with "manifest unknown"; ' +
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
  test('S10: on: block includes pull_request trigger with RELEASE_SURFACE paths', () => {
    const onBlock = extractOnBlock(yml);
    assert.ok(onBlock !== null, 'release.yml must have an on: block');
    assert.ok(
      onBlock.includes('pull_request:'),
      'release.yml must have a pull_request: trigger so release-surface PRs are validated; ' +
      `got on: block:\n${onBlock}`,
    );
    // At least one RELEASE_SURFACE entry must appear in the on: block paths list.
    const hasPath = RELEASE_SURFACE.some(entry => {
      const base = entry.endsWith('/**') ? entry.slice(0, -3) : entry;
      return onBlock.includes(base);
    });
    assert.ok(
      hasPath,
      'pull_request trigger paths must include at least one entry from RELEASE_SURFACE; ' +
      `RELEASE_SURFACE = [\n  ${RELEASE_SURFACE.join(',\n  ')}\n]; ` +
      `got on: block:\n${onBlock}`,
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
    let jobName = null;
    for (const line of section.split('\n')) {
      const m = /^    name:\s+(.+)$/.exec(line);
      if (m) { jobName = m[1].trim(); break; }
    }
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

});
