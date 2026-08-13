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
 * exits 2 unless --required-from <branch> is supplied. A protected branch that
 * lists ZERO required contexts also exits 2: "all 0 required contexts passed"
 * is a vacuous green, and vacuous greens are what PF-017 was made of.
 *
 * D-PR2a: A required context is resolved against the UNION of check-runs AND
 * commit statuses (GitHub branch protection accepts either namespace). Both
 * namespaces are checked independently: a failing commit status is not masked
 * by a passing check-run under the same name.
 *
 * D-PR3: Three tiers:
 *   Tier A  (required):          MUST be completed+success — missing/cancelled/etc = FAIL
 *   Tier A+ (expected, local):   Same semantics as Tier A, but sourced from EXPECTED_CONTEXTS
 *                                rather than branch protection. Absence = FAIL (not advisory).
 *   Tier B  (non-required runs): failure/cancelled/timed_out = FAIL; not-yet-completed = FAIL
 *   Tier C  (legacy statuses):   advisory unless the context is required
 *
 * D-PR3b: EXPECTED_CONTEXTS lists jobs that must be present and passing even though
 * they are not (yet) in branch protection. Currently: ['Source hygiene'] — the
 * control-byte gate added in #288. Tier B alone cannot make it binding because Tier B
 * only iterates runs that ALREADY EXIST in the check-run list; an absent job has
 * nothing to iterate. Tier A+ fills this gap by asserting presence (applies ADR-009,
 * avoids PF-013: absence is never evidence of success).
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
 *
 * Exit codes:
 *   0 — all required contexts completed+success; prints `gh pr merge` command
 *   1 — one or more required contexts missing/cancelled/failed/etc
 *   2 — tool error: gh missing/too old, protection unreadable, pagination error
 */
'use strict';

import { spawnSync } from 'node:child_process';
import { realpathSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

/**
 * True when this module is the process entry point.
 *
 * Deliberately not `import.meta.url === 'file://' + process.argv[1]`: that
 * comparison is false for any path a file URL percent-encodes (a space) and
 * for any symlinked path (Node resolves import.meta.url through realpath but
 * leaves argv[1] as typed — on macOS /tmp and /var/folders are symlinks). Both
 * failures are silent: main() never runs and the merge gate exits 0 having
 * verified nothing. Kept local so each scripts/verify-*.mjs stays standalone.
 */
function isMainModule(metaUrl) {
  const entry = process.argv[1];
  if (!entry) return false;
  const modulePath = fileURLToPath(metaUrl);
  try {
    return realpathSync(entry) === realpathSync(modulePath);
  } catch {
    return pathToFileURL(resolve(entry)).href === metaUrl;
  }
}

// D-PR4a: hard page cap — exit 2 rather than evaluating a partial result
const MAX_PAGES = 20;
// D-PR5: minimum gh version required for --match-head-commit
const MIN_GH_MAJOR = 2;
const MIN_GH_MINOR = 31;

/**
 * D-PR3b: Locally-expected contexts — enforced with Tier A semantics regardless
 * of whether they appear in branch protection. An absent job is FAIL, not
 * advisory (applies ADR-009, avoids PF-013).
 *
 * 'Source hygiene' is the control-byte gate added in #288. It is not yet a
 * required context in branch protection (Open Decision 1), so Tier B is the
 * only thing making it binding — but Tier B only iterates runs that ALREADY
 * APPEAR in the check-run list. When the job is absent (renamed, deleted, not
 * yet started, or cancelled before reporting), Tier B has nothing to iterate
 * and the verifier would emit a PASS. EXPECTED_CONTEXTS closes this gap by
 * asserting presence, mirroring Tier A semantics.
 */
export const EXPECTED_CONTEXTS = ['Source hygiene'];

// ---------------------------------------------------------------------------
// gh runner (thin IO shim; injected in tests for offline operation)
// ---------------------------------------------------------------------------

/**
 * Default runner: calls `gh api` and returns parsed JSON.
 *
 * On error, returns `{ __error: true, status: <process-exit>, httpStatus: <HTTP-code|null>, stderr }`.
 * `httpStatus` is parsed from gh's stderr format "gh: <message> (HTTP NNN)" and is the
 * value callers should branch on for 404/403 distinctions — the process exit code is
 * always 1 regardless of the HTTP status, so `status` alone cannot distinguish 404 from 403.
 *
 * @param {string[]} args
 * @returns {any}
 */
function defaultGhRunner(args) {
  const r = spawnSync('gh', args, { encoding: 'utf8', maxBuffer: 16 * 1024 * 1024 });
  if (r.error) {
    const stderr = r.error.code === 'ENOENT'
      ? 'gh is not on PATH'
      : `gh error: ${r.error.message}`;
    return { __error: true, status: -1, httpStatus: null, stderr };
  }
  if (r.status !== 0) {
    // Parse HTTP status from gh's stderr format: "gh: <message> (HTTP NNN)"
    // The process exit code is always 1 for API errors; only the parsed HTTP
    // status code reliably distinguishes 404 from 403 (avoids PF-013: dead
    // branches that only trigger on a value the runner never actually produces).
    const httpMatch = r.stderr ? r.stderr.match(/\(HTTP (\d+)\)/) : null;
    const httpStatus = httpMatch ? parseInt(httpMatch[1], 10) : null;
    return { __error: true, status: r.status, httpStatus, stderr: r.stderr };
  }
  try {
    return JSON.parse(r.stdout);
  } catch {
    return { __error: true, status: r.status, httpStatus: null, raw: r.stdout, stderr: r.stderr };
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
 *   expectedContexts?: string[];  // defaults to EXPECTED_CONTEXTS
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
export function evaluateChecks({
  requiredContexts,
  checkRuns,
  statuses,
  headSha,
  expectedContexts = EXPECTED_CONTEXTS,
}) {
  const lines = [];
  const failures = [];
  let pass = true;

  const nChecks = checkRuns.length;
  const nStatuses = statuses.length;
  const nRequired = requiredContexts.length;

  // D-PR4: Non-vacuity guard — an empty required set can never be evidence of
  // merge safety. "All 0 required contexts passed" is the same vacuous green
  // that ADR-009 forbids: the tool cannot tell, so it exits 2, never 0.
  // Reachable whenever protection exists but lists no required status checks,
  // or when a future API shape stops populating `contexts`.
  if (nRequired === 0) {
    lines.push(`  check-runs: ${nChecks}, statuses: ${nStatuses}, required contexts: ${nRequired}`);
    lines.push(
      '✖ INDETERMINATE: zero required contexts — nothing to verify, so this is not a pass ' +
      '(applies ADR-009). Point --required-from at a branch whose protection lists required checks.',
    );
    return { pass: false, exitCode: 2, lines };
  }

  // D-PR4: Non-vacuity guard — zero check-runs is the #239 shape (not a pass)
  if (nChecks === 0) {
    lines.push(`  check-runs: ${nChecks}, statuses: ${nStatuses}, required contexts: ${nRequired}`);
    lines.push('✖ FAIL: zero check-runs (the #239 shape — not a pass, avoids PF-013)');
    // AC-22: still name every required context that was absent so the caller
    // knows exactly what was missing, even though the non-vacuity guard is
    // already sufficient to FAIL. This matches the Tier A loop's behavior for
    // a partial check-run set and eliminates vacuous "zero check-runs" messages
    // that don't say which contexts were expected.
    for (const ctx of requiredContexts) {
      lines.push(`✖ Tier A (required): "${ctx}" — not found in check-runs (never ran)`);
    }
    return { pass: false, exitCode: 1, lines };
  }

  // Always print counts (D-PR4 / avoids PF-013)
  lines.push(`  check-runs: ${nChecks}, statuses: ${nStatuses}, required contexts: ${nRequired}`);

  // Build lookup maps.
  // A name maps to EVERY check-run carrying it, not just the last one seen:
  // `filter=latest` de-duplicates within a check-suite, but two suites (two
  // workflows) can publish the same name, and a required context is satisfied
  // by the name. Keeping only the last entry lets a later success mask an
  // earlier failure — a fail-open in a merge gate.
  const checksByName = new Map(); // name -> CheckRun[]
  for (const cr of checkRuns) {
    const list = checksByName.get(cr.name);
    if (list) list.push(cr);
    else checksByName.set(cr.name, [cr]);
  }
  const statusByContext = new Map(); // context -> CommitStatus
  for (const st of statuses) {
    statusByContext.set(st.context, st);
  }

  // ---- Tier A: required contexts ----
  // D-PR2a: resolved against BOTH check-runs AND commit statuses independently.
  // GitHub branch protection enforcement considers both namespaces; a failing
  // commit status is not masked by a passing check-run under the same name.
  // avoids PF-017: must be status=completed AND conclusion=success.
  for (const ctx of requiredContexts) {
    const crs = checksByName.get(ctx);
    const st = statusByContext.get(ctx);
    let found = false;

    if (crs) {
      found = true;
      // Every run under this name must pass — keeping only the last entry would
      // let a later success mask an earlier failure from a different check-suite.
      for (const cr of crs) {
        if (cr.status !== 'completed' || cr.conclusion !== 'success') {
          failures.push(
            `Tier A (required): "${ctx}" — status=${cr.status}, conclusion=${cr.conclusion ?? 'null'}` +
            (crs.length > 1 ? ` (1 of ${crs.length} runs sharing this name)` : '') +
            ` (avoids PF-017: cancelled/skipped/in_progress are not success)`,
          );
          pass = false;
        }
      }
    }
    if (st) {
      // Always check the status namespace too, even when a check-run was found.
      // D-PR2a: both namespaces are checked independently so a failing status
      // is not silently ignored when a check-run of the same name is green.
      found = true;
      if (st.state !== 'success') {
        failures.push(`Tier A (required): "${ctx}" — status.state=${st.state} (must be "success")`);
        pass = false;
      }
    }
    if (!found) {
      // Not found in either namespace
      failures.push(`Tier A (required): "${ctx}" — not found in check-runs or statuses (never ran)`);
      pass = false;
    }
  }

  const requiredSet = new Set(requiredContexts);

  // ---- Tier A+: locally-expected contexts (Tier A semantics, protection-independent) ----
  // D-PR3b: jobs in EXPECTED_CONTEXTS must be present and passing regardless of
  // whether they appear in branch protection. 'Source hygiene' is the primary
  // example: it is not yet a required context (Open Decision 1), so without
  // this tier, an absent or renamed job would yield exitCode=0. Absence is FAIL,
  // not advisory — applies ADR-009, avoids PF-013.
  for (const ctx of expectedContexts) {
    if (requiredSet.has(ctx)) continue; // Already enforced in Tier A with full context
    const crs = checksByName.get(ctx);
    const st = statusByContext.get(ctx);
    let found = false;

    if (crs) {
      found = true;
      for (const cr of crs) {
        if (cr.status !== 'completed' || cr.conclusion !== 'success') {
          failures.push(
            `Tier A+ (expected): "${ctx}" — status=${cr.status}, conclusion=${cr.conclusion ?? 'null'} ` +
            `(locally-expected job must be completed+success, D-PR3b)`,
          );
          pass = false;
        }
      }
    }
    if (st) {
      found = true;
      if (st.state !== 'success') {
        failures.push(`Tier A+ (expected): "${ctx}" — status.state=${st.state} (must be "success", D-PR3b)`);
        pass = false;
      }
    }
    if (!found) {
      // ABSENCE IS FAIL — this is the central defect this tier exists to close.
      // When source-hygiene is absent from check-runs, Tier B has nothing to
      // iterate and would have emitted a PASS. Tier A+ prevents that.
      failures.push(
        `Tier A+ (expected): "${ctx}" — not found in check-runs or statuses (never ran). ` +
        `This job must exist and pass; its absence is not evidence of success (D-PR3b, avoids PF-013)`,
      );
      pass = false;
    }
  }

  // ---- Tier B: non-required check-runs ----
  // failure/cancelled/timed_out/action_required/stale = FAIL
  // not-yet-completed (queued/in_progress) = FAIL: a PASS must not be emitted
  //   while an active check is still outstanding; the outstanding check might
  //   later fail, rendering the verified SHA stale and the merge command unsafe.
  //   avoids PF-017: indeterminate state must never read as success.
  // skipped/neutral = advisory (reported but not fatal)
  const TIER_B_FAIL = new Set(['failure', 'timed_out', 'cancelled', 'action_required', 'stale']);
  const TIER_B_ADVISORY = new Set(['skipped', 'neutral']);

  for (const cr of checkRuns) {
    if (requiredSet.has(cr.name)) continue; // Already handled in Tier A
    // Also skip if already handled in Tier A+
    if (expectedContexts.includes(cr.name)) continue;

    if (cr.status !== 'completed') {
      // Non-completed non-required run: a queued or in_progress check means the
      // CI suite is still running. Emitting a PASS while checks are outstanding
      // would defeat the gate — the outstanding check might later fail.
      // avoids PF-017: indeterminate state (not-yet-completed) is never success.
      failures.push(
        `Tier B (non-required): "${cr.name}" — status=${cr.status} (not yet completed; ` +
        `must not merge while checks are still running)`,
      );
      pass = false;
      continue;
    }
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
//
// Every step below returns a Result ({ ok: true, ... } | { ok: false, exitCode,
// message }) instead of calling process.exit. Only the CLI wrapper at the
// bottom of this file translates an exit code into a process exit, which is
// what makes the exit-2 paths (404, 403, stale gh, unbounded pagination)
// reachable from an offline test with an injected runner. A tool whose
// failure paths can only be asserted by grepping its own source text is
// exactly the vacuous verification this script exists to eliminate
// (applies ADR-009, avoids PF-013).
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
 *
 * @returns {{ ok: true, checkRuns: CheckRun[] } | { ok: false, exitCode: 2, message: string }}
 */
export function fetchCheckRuns(headSha, runner) {
  const perPage = 100;
  let page = 1;
  const allCheckRuns = [];
  let totalCount = null;

  // Bounded loop (reliability rule): at most MAX_PAGES iterations, always.
  while (page <= MAX_PAGES) {
    // D-PR4a: filter=latest pinned explicitly to prevent default-change surprises
    const url = `/repos/{owner}/{repo}/commits/${headSha}/check-runs?per_page=${perPage}&page=${page}&filter=latest`;
    const data = runner(['api', url]);
    if (data.__error) {
      return {
        ok: false,
        exitCode: 2,
        message: `check-runs API error (page ${page}): ${data.stderr}`,
      };
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
    return {
      ok: false,
      exitCode: 2,
      message: `pagination exceeded ${MAX_PAGES} pages (D-PR4a) — refusing to evaluate a partial set`,
    };
  }

  // D-PR4a: assert we collected everything declared by total_count
  if (totalCount !== null && allCheckRuns.length !== totalCount) {
    return {
      ok: false,
      exitCode: 2,
      message: `collected ${allCheckRuns.length} check-runs but total_count=${totalCount} — partial page set`,
    };
  }

  return { ok: true, checkRuns: allCheckRuns };
}

/**
 * Fetch commit statuses for a sha.
 *
 * The combined-status endpoint caps at 30 statuses per response and offers no
 * pagination. If total_count exceeds what was returned, a required context
 * backed by a status beyond position 30 would be falsely reported as 'never
 * ran'. Fail closed (exit 2) rather than silently evaluate a partial set —
 * consistent with D-PR4a's total_count assertion on check-runs.
 *
 * @returns {{ ok: true, statuses: CommitStatus[] } | { ok: false, exitCode: 2, message: string }}
 */
export function fetchStatuses(headSha, runner) {
  const url = `/repos/{owner}/{repo}/commits/${headSha}/status`;
  const data = runner(['api', url]);
  if (data.__error) {
    return { ok: false, exitCode: 2, message: `commit-status API error: ${data.stderr}` };
  }
  const statuses = data.statuses ?? [];
  // D-PR4a parity: combined-status returns at most 30 statuses with no pagination.
  // If total_count exceeds the returned count, we have a partial view and must
  // fail closed rather than evaluate an incomplete set.
  const totalCount = data.total_count ?? statuses.length;
  if (totalCount > statuses.length) {
    return {
      ok: false,
      exitCode: 2,
      message:
        `commit-status endpoint returned ${statuses.length} of ${totalCount} statuses — ` +
        `at least one status may be missing (API cap at 30). ` +
        `A required context beyond position 30 would be falsely reported as never-ran.`,
    };
  }
  return { ok: true, statuses };
}

/**
 * Fetch required contexts from branch protection.
 * D-PR2: exit 2 on 403 or when the base has no protection and no fallback.
 * AC-29: unprotected base (404) exits 2 unless --required-from is given.
 *
 * The required set is the UNION of the legacy `contexts` array and the newer
 * `checks[].context` array. GitHub populates both today; reading only the
 * deprecated `contexts` would silently yield an empty required set — and an
 * empty required set is a vacuous pass, not a pass.
 *
 * Error objects from defaultGhRunner carry `httpStatus` (parsed from gh's stderr
 * format "gh: <message> (HTTP NNN)") rather than the process exit code, which
 * is always 1 regardless of the HTTP status. Callers branch on `httpStatus`.
 *
 * @returns {{ ok: true, contexts: string[], resolvedBranch: string, notes: string[] }
 *          | { ok: false, exitCode: 2, message: string }}
 */
export function fetchRequiredContexts(baseBranch, requiredFrom, runner) {
  const branch = requiredFrom ?? baseBranch;
  const url = `/repos/{owner}/{repo}/branches/${branch}/protection`;
  const data = runner(['api', url]);

  if (data.__error) {
    if (data.httpStatus === 404) {
      const message = requiredFrom
        ? `--required-from branch "${requiredFrom}" has no protection (404)`
        : `base branch "${baseBranch}" has no protection (404). ` +
          `Use --required-from <branch> to name a protected branch, e.g. --required-from main. ` +
          `(AC-29: an unprotected base is not a pass — D-PR2)`;
      return { ok: false, exitCode: 2, message };
    }
    if (data.httpStatus === 403) {
      return {
        ok: false,
        exitCode: 2,
        message: 'branch protection unreadable (403 — insufficient permissions)',
      };
    }
    return { ok: false, exitCode: 2, message: `protection API error: ${data.stderr}` };
  }

  const rsc = data?.required_status_checks;
  const contexts = [...new Set([
    ...(rsc?.contexts ?? []),
    ...(rsc?.checks ?? []).map(c => c?.context).filter(c => typeof c === 'string'),
  ])];

  if (contexts.length === 0) {
    return {
      ok: false,
      exitCode: 2,
      message:
        `branch "${branch}" is protected but lists zero required status checks — ` +
        `there is nothing to verify, which is indeterminate, not a pass (applies ADR-009)`,
    };
  }

  const notes = [];
  if (requiredFrom && requiredFrom !== baseBranch) {
    notes.push(`  Required contexts read from: ${requiredFrom} (base branch "${baseBranch}" is unprotected)`);
  }
  return { ok: true, contexts, resolvedBranch: branch, notes };
}

const USAGE =
  'Usage: node scripts/verify-pr-checks.mjs <pr-number> [--required-from <branch>]';

/**
 * Live entry point. Returns an exit code; never calls process.exit, so tests
 * can drive it end-to-end with an injected runner.
 *
 * @param {string[]} argv
 * @param {(args: string[]) => any} runner       — gh API shim
 * @param {() => ({major:number,minor:number}|null)} ghVersionFn — version probe
 * @returns {0|1|2}
 */
export function main(argv = process.argv.slice(2), runner = defaultGhRunner, ghVersionFn = ghVersion) {
  const fail = (message) => {
    console.error(`✖ verify-pr-checks: ${message}`);
  };

  // ---- Parse args ----
  const prArg = argv.find(a => /^\d+$/.test(a));
  if (!prArg) {
    console.error(USAGE);
    return 2;
  }
  const prNumber = parseInt(prArg, 10);

  const rfIdx = argv.indexOf('--required-from');
  if (rfIdx !== -1 && !argv[rfIdx + 1]) {
    fail(`--required-from requires a branch name\n${USAGE}`);
    return 2;
  }
  const requiredFrom = rfIdx !== -1 ? argv[rfIdx + 1] : null;

  // ---- Check gh version (D-PR5) ----
  const ver = ghVersionFn();
  if (!ver || ver.major < MIN_GH_MAJOR || (ver.major === MIN_GH_MAJOR && ver.minor < MIN_GH_MINOR)) {
    const found = ver ? `${ver.major}.${ver.minor}` : 'unknown';
    fail(
      `gh >= ${MIN_GH_MAJOR}.${MIN_GH_MINOR} required (found ${found}); ` +
      `needed for --match-head-commit (D-PR5)`,
    );
    return 2;
  }

  // ---- Fetch PR metadata ----
  const prData = runner(['api', `/repos/{owner}/{repo}/pulls/${prNumber}`]);
  if (prData.__error) {
    fail(`cannot read PR ${prNumber}: ${prData.stderr}`);
    return 2;
  }
  const headSha = prData.head?.sha;
  const baseBranch = prData.base?.ref;
  if (!headSha || !baseBranch) {
    fail(`cannot determine head SHA or base branch for PR ${prNumber}`);
    return 2;
  }

  console.log(`PR #${prNumber}: base=${baseBranch} head=${headSha.slice(0, 7)}`);

  // ---- Fetch required contexts (D-PR2) ----
  const req = fetchRequiredContexts(baseBranch, requiredFrom, runner);
  if (!req.ok) {
    fail(req.message);
    return req.exitCode;
  }
  for (const note of req.notes) console.log(note);
  console.log(`  Required contexts (${req.contexts.length}) from ${req.resolvedBranch}: ${req.contexts.join(', ')}`);

  // ---- Fetch check-runs (D-PR4a) ----
  const cr = fetchCheckRuns(headSha, runner);
  if (!cr.ok) {
    fail(cr.message);
    return cr.exitCode;
  }

  // ---- Fetch commit statuses (D-PR2a) ----
  const st = fetchStatuses(headSha, runner);
  if (!st.ok) {
    fail(st.message);
    return st.exitCode;
  }

  // ---- Evaluate (D-PR1: pure function) ----
  const result = evaluateChecks({
    requiredContexts: req.contexts,
    checkRuns: cr.checkRuns,
    statuses: st.statuses,
    headSha,
  });

  for (const line of result.lines) {
    console.log(line);
  }

  return result.exitCode;
}

// Run only when executed directly (not imported by tests). See isMainModule.
if (isMainModule(import.meta.url)) {
  process.exit(main());
}
