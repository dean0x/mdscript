#!/usr/bin/env node
/**
 * D-PR1: Pre-merge check verifier — asserts that all required branch-protection
 * contexts are completed+success before an --admin merge.
 *
 * Addresses PF-017: a CANCELLED GitHub Actions run is neither success nor
 * failure. `gh pr merge --admin` treats a cancelled run as not-failing and
 * merges, bypassing the required-status gate. This tool explicitly checks
 * status=completed AND conclusion=success for every required context, and
 * treats cancelled/skipped/stale/in_progress as NOT passing.
 *
 * D-PR2: Required contexts are read LIVE from branch protection — never
 * hardcoded. On 403 the script exits 2. On 404 (unprotected base) the script
 * exits 2 unless --required-from <branch> is supplied.
 *
 * D-PR2a: A required context is resolved against the UNION of check-runs AND
 * commit statuses (GitHub branch protection accepts either namespace).
 *
 * D-PR3: Three tiers:
 *   Tier A (required): MUST be completed+success — missing/cancelled/etc = FAIL
 *   Tier B (non-required check-runs): failure/cancelled/timed_out = FAIL
 *   Tier C (legacy commit statuses): advisory unless the context is required
 *
 * D-PR4: Non-vacuity guard — zero check-runs = FAIL (the #239 case).
 *        Counts are always printed on every run (avoids PF-013).
 *
 * D-PR4a: Pagination is bounded at MAX_PAGES; reaching it exits 2.
 *         `filter=latest` is pinned explicitly (default today, but implicit
 *         defaults can change and this gate's verdict depends on it).
 *
 * D-PR5: On PASS the tool prints a merge command with --match-head-commit
 *        <headSha>, closing the TOCTOU window where the verified SHA diverges
 *        from HEAD by the time the merge runs.
 *
 * D-PR6: Exit codes — 0 PASS, 1 FAIL, 2 indeterminate. "Cannot tell" is
 *        never 0.
 *
 * Usage:
 *   node scripts/verify-pr-checks.mjs <pr-number>
 *   node scripts/verify-pr-checks.mjs <pr-number> --required-from <branch>
 *   node scripts/verify-pr-checks.mjs <pr-number> --head-sha <sha>
 *
 * Exit codes:
 *   0 — all required contexts completed+success; prints `gh pr merge` command
 *   1 — one or more required contexts missing/cancelled/failed/etc
 *   2 — tool error: gh missing/too old, protection unreadable, pagination error
 */
'use strict';

import { spawnSync } from 'node:child_process';

// D-PR4a: hard page cap — exit 2 rather than evaluating a partial result
const MAX_PAGES = 20;
// D-PR5: minimum gh version required for --match-head-commit
const MIN_GH_MAJOR = 2;
const MIN_GH_MINOR = 31;

// ---------------------------------------------------------------------------
// gh runner (thin IO shim; injected in tests for offline operation)
// ---------------------------------------------------------------------------

/**
 * Default runner: calls `gh api` and returns parsed JSON.
 * @param {string[]} args
 * @returns {any}
 */
function defaultGhRunner(args) {
  const r = spawnSync('gh', args, { encoding: 'utf8', maxBuffer: 16 * 1024 * 1024 });
  if (r.error) {
    if (r.error.code === 'ENOENT') {
      console.error('✖ verify-pr-checks: gh is not on PATH');
      process.exit(2);
    }
    console.error(`✖ verify-pr-checks: gh error: ${r.error.message}`);
    process.exit(2);
  }
  if (r.status !== 0) {
    // Return status code so caller can handle 404/403
    return { __error: true, status: r.status, stderr: r.stderr };
  }
  try {
    return JSON.parse(r.stdout);
  } catch {
    return { __error: true, status: r.status, raw: r.stdout, stderr: r.stderr };
  }
}

// ---------------------------------------------------------------------------
// D-PR1: Pure evaluation function (no I/O — fully testable offline)
// ---------------------------------------------------------------------------

/**
 * @typedef {{
 *   name: string;
 *   status: string;       // 'completed' | 'queued' | 'in_progress' | ...
 *   conclusion: string | null;  // 'success' | 'failure' | 'cancelled' | ...
 * }} CheckRun
 *
 * @typedef {{
 *   context: string;
 *   state: string;        // 'success' | 'failure' | 'error' | 'pending'
 * }} CommitStatus
 *
 * @typedef {{
 *   requiredContexts: string[];
 *   checkRuns: CheckRun[];
 *   statuses: CommitStatus[];
 *   headSha: string;
 * }} EvaluateInput
 *
 * @typedef {{
 *   pass: boolean;
 *   exitCode: number;    // 0, 1, or 2
 *   lines: string[];     // human-readable output lines
 *   mergeCommand?: string;
 * }} EvaluateResult
 */

/**
 * Evaluate check-run and status data against required contexts.
 * This is the pure decision function — inject any data source.
 *
 * applies ADR-009, avoids PF-013: prints counts on every call.
 * avoids PF-017: required contexts must be status=completed AND conclusion=success.
 *
 * @param {EvaluateInput} input
 * @returns {EvaluateResult}
 */
export function evaluateChecks({ requiredContexts, checkRuns, statuses, headSha }) {
  const lines = [];
  const failures = [];
  let pass = true;

  const nChecks = checkRuns.length;
  const nStatuses = statuses.length;
  const nRequired = requiredContexts.length;

  // D-PR4: Non-vacuity guard — zero check-runs is the #239 shape (not a pass)
  if (nChecks === 0) {
    lines.push(`  check-runs: ${nChecks}, statuses: ${nStatuses}, required contexts: ${nRequired}`);
    lines.push('✖ FAIL: zero check-runs (the #239 shape — not a pass, avoids PF-013)');
    return { pass: false, exitCode: 1, lines };
  }

  // Always print counts (D-PR4 / avoids PF-013)
  lines.push(`  check-runs: ${nChecks}, statuses: ${nStatuses}, required contexts: ${nRequired}`);

  // Build lookup maps
  const checkByName = new Map(); // name -> CheckRun (latest, filter=latest already applied)
  for (const cr of checkRuns) {
    checkByName.set(cr.name, cr);
  }
  const statusByContext = new Map(); // context -> CommitStatus
  for (const st of statuses) {
    statusByContext.set(st.context, st);
  }

  // ---- Tier A: required contexts ----
  // D-PR2a: resolved against the UNION of check-runs and commit statuses.
  // avoids PF-017: must be status=completed AND conclusion=success.
  for (const ctx of requiredContexts) {
    const cr = checkByName.get(ctx);
    const st = statusByContext.get(ctx);

    if (cr) {
      // Found in check-runs namespace
      if (cr.status !== 'completed' || cr.conclusion !== 'success') {
        failures.push(
          `Tier A (required): "${ctx}" — status=${cr.status}, conclusion=${cr.conclusion ?? 'null'}` +
          ` (avoids PF-017: cancelled/skipped/in_progress are not success)`,
        );
        pass = false;
      }
    } else if (st) {
      // Found in commit statuses namespace
      if (st.state !== 'success') {
        failures.push(`Tier A (required): "${ctx}" — status.state=${st.state} (must be "success")`);
        pass = false;
      }
    } else {
      // Not found in either namespace
      failures.push(`Tier A (required): "${ctx}" — not found in check-runs or statuses (never ran)`);
      pass = false;
    }
  }

  const requiredSet = new Set(requiredContexts);

  // ---- Tier B: non-required check-runs ----
  // failure/cancelled/timed_out/action_required/stale = FAIL
  // skipped/neutral = advisory (reported but not fatal)
  const TIER_B_FAIL = new Set(['failure', 'timed_out', 'cancelled', 'action_required', 'stale']);
  const TIER_B_ADVISORY = new Set(['skipped', 'neutral']);

  for (const cr of checkRuns) {
    if (requiredSet.has(cr.name)) continue; // Already handled in Tier A
    if (cr.status !== 'completed') continue; // Still running — skip advisory
    if (cr.conclusion == null) continue;

    if (TIER_B_FAIL.has(cr.conclusion)) {
      failures.push(`Tier B (non-required): "${cr.name}" — conclusion=${cr.conclusion}`);
      pass = false;
    } else if (TIER_B_ADVISORY.has(cr.conclusion)) {
      lines.push(`  advisory: "${cr.name}" — conclusion=${cr.conclusion}`);
    }
  }

  // ---- Tier C: legacy commit statuses ----
  // Advisory unless the context is required (Tier A already handled those).
  // Justified by #240 evidence: the sole status was security/snyk (dean0x)
  // in state=error due to account-plan limits — not a workflow in this repo.
  for (const st of statuses) {
    if (requiredSet.has(st.context)) continue; // Already handled in Tier A
    if (st.state !== 'success' && st.state !== 'pending') {
      lines.push(`  advisory (Tier C): "${st.context}" — state=${st.state}`);
    }
  }

  // ---- Compose result ----
  for (const f of failures) {
    lines.push(`✖ ${f}`);
  }

  if (pass) {
    const cmd = `gh pr merge --squash --match-head-commit ${headSha}`;
    lines.push(`✓ PASS — all ${nRequired} required contexts completed+success`);
    lines.push(`  Verified SHA: ${headSha}`);
    lines.push(`  Merge command: ${cmd}`);
    return { pass: true, exitCode: 0, lines, mergeCommand: cmd };
  } else {
    lines.push(`✖ FAIL — ${failures.length} required context(s) not satisfied`);
    return { pass: false, exitCode: 1, lines };
  }
}

// ---------------------------------------------------------------------------
// Main (live path with real gh API calls)
// ---------------------------------------------------------------------------

function ghVersion() {
  const r = spawnSync('gh', ['--version'], { encoding: 'utf8' });
  if (r.error || r.status !== 0) return null;
  // Output: "gh version 2.88.1 (2026-07-17)"
  const m = r.stdout.match(/gh version (\d+)\.(\d+)/);
  if (!m) return null;
  return { major: parseInt(m[1], 10), minor: parseInt(m[2], 10) };
}

/**
 * Fetch all pages of check-runs for a given sha, bounded at MAX_PAGES.
 * D-PR4a: filter=latest pinned; paginate with hard cap; exit 2 on incomplete.
 */
function fetchCheckRuns(headSha, runner) {
  // Use gh api with --paginate to collect all pages
  // gh api --paginate emits one JSON object per page (not merged)
  const perPage = 100;
  let page = 1;
  const allCheckRuns = [];
  let totalCount = null;

  while (page <= MAX_PAGES) {
    // D-PR4a: filter=latest pinned explicitly to prevent default-change surprises
    const url = `/repos/{owner}/{repo}/commits/${headSha}/check-runs?per_page=${perPage}&page=${page}&filter=latest`;
    const data = runner(['api', url]);
    if (data.__error) {
      console.error(`✖ verify-pr-checks: check-runs API error (page ${page}): ${data.stderr}`);
      process.exit(2);
    }
    if (totalCount === null) {
      totalCount = data.total_count ?? 0;
    }
    const runs = data.check_runs ?? [];
    allCheckRuns.push(...runs);
    if (runs.length < perPage || allCheckRuns.length >= totalCount) break;
    page++;
  }

  if (page > MAX_PAGES) {
    console.error(`✖ verify-pr-checks: pagination exceeded ${MAX_PAGES} pages; exiting 2 (D-PR4a)`);
    process.exit(2);
  }

  // D-PR4a: assert we collected everything declared by total_count
  if (totalCount !== null && allCheckRuns.length !== totalCount) {
    console.error(
      `✖ verify-pr-checks: collected ${allCheckRuns.length} check-runs but total_count=${totalCount}; exiting 2`,
    );
    process.exit(2);
  }

  return allCheckRuns;
}

/**
 * Fetch commit statuses for a sha.
 */
function fetchStatuses(headSha, runner) {
  const url = `/repos/{owner}/{repo}/commits/${headSha}/status`;
  const data = runner(['api', url]);
  if (data.__error) {
    console.error(`✖ verify-pr-checks: commit-status API error: ${data.stderr}`);
    process.exit(2);
  }
  return data.statuses ?? [];
}

/**
 * Fetch required contexts from branch protection.
 * D-PR2: exits 2 on 403 or when no protection and no fallback.
 * AC-29: unprotected base (404) exits 2 unless --required-from is given.
 */
function fetchRequiredContexts(baseBranch, requiredFrom, runner) {
  const branch = requiredFrom ?? baseBranch;
  const url = `/repos/{owner}/{repo}/branches/${branch}/protection`;
  const data = runner(['api', url]);

  if (data.__error) {
    if (data.status === 404) {
      if (requiredFrom) {
        console.error(
          `✖ verify-pr-checks: --required-from branch "${requiredFrom}" has no protection (404); exit 2`,
        );
      } else {
        console.error(
          `✖ verify-pr-checks: base branch "${baseBranch}" has no protection (404). ` +
          `Use --required-from <branch> to specify a protected branch, e.g. --required-from main. ` +
          `(AC-29: unprotected base is not a pass — D-PR2)`
        );
      }
      process.exit(2);
    }
    if (data.status === 403) {
      console.error(
        `✖ verify-pr-checks: branch protection unreadable (403 — insufficient permissions); exit 2`,
      );
      process.exit(2);
    }
    console.error(`✖ verify-pr-checks: protection API error: ${data.stderr}`);
    process.exit(2);
  }

  const contexts = data?.required_status_checks?.contexts ?? [];
  if (requiredFrom && requiredFrom !== baseBranch) {
    console.log(`  Required contexts read from: ${requiredFrom} (base branch "${baseBranch}" is unprotected)`);
  }
  return { contexts, resolvedBranch: branch };
}

export function main(argv = process.argv.slice(2), runner = defaultGhRunner) {
  // ---- Parse args ----
  const prArg = argv.find(a => /^\d+$/.test(a));
  if (!prArg) {
    console.error('Usage: node scripts/verify-pr-checks.mjs <pr-number> [--required-from <branch>] [--head-sha <sha>]');
    process.exit(2);
  }
  const prNumber = parseInt(prArg, 10);

  const rfIdx = argv.indexOf('--required-from');
  const requiredFrom = rfIdx !== -1 ? argv[rfIdx + 1] : null;

  const hsIdx = argv.indexOf('--head-sha');
  const headShaOverride = hsIdx !== -1 ? argv[hsIdx + 1] : null;

  // ---- Check gh version (D-PR5) ----
  const ver = ghVersion();
  if (!ver || ver.major < MIN_GH_MAJOR || (ver.major === MIN_GH_MAJOR && ver.minor < MIN_GH_MINOR)) {
    const found = ver ? `${ver.major}.${ver.minor}` : 'unknown';
    console.error(
      `✖ verify-pr-checks: gh >= ${MIN_GH_MAJOR}.${MIN_GH_MINOR} required (found ${found}); ` +
      `needed for --match-head-commit (D-PR5)`
    );
    process.exit(2);
  }

  // ---- Fetch PR metadata ----
  const prData = runner(['api', `/repos/{owner}/{repo}/pulls/${prNumber}`]);
  if (prData.__error) {
    console.error(`✖ verify-pr-checks: cannot read PR ${prNumber}: ${prData.stderr}`);
    process.exit(2);
  }
  const headSha = headShaOverride ?? prData.head?.sha;
  const baseBranch = prData.base?.ref;
  if (!headSha || !baseBranch) {
    console.error(`✖ verify-pr-checks: cannot determine head SHA or base branch for PR ${prNumber}`);
    process.exit(2);
  }

  console.log(`PR #${prNumber}: base=${baseBranch} head=${headSha.slice(0, 7)}`);

  // ---- Fetch required contexts (D-PR2) ----
  const { contexts: requiredContexts, resolvedBranch } = fetchRequiredContexts(baseBranch, requiredFrom, runner);
  console.log(`  Required contexts (${requiredContexts.length}) from ${resolvedBranch}: ${requiredContexts.join(', ') || '(none)'}`);

  // ---- Fetch check-runs (D-PR4a) ----
  const checkRuns = fetchCheckRuns(headSha, runner);

  // ---- Fetch commit statuses (D-PR2a) ----
  const statuses = fetchStatuses(headSha, runner);

  // ---- Evaluate (D-PR1: pure function) ----
  const result = evaluateChecks({ requiredContexts, checkRuns, statuses, headSha });

  for (const line of result.lines) {
    console.log(line);
  }

  process.exit(result.exitCode);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main();
}
