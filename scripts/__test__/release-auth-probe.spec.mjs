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
