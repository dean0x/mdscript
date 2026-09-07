/**
 * Tests for scripts/verify-pr-checks.mjs
 *
 * All tests drive the pure `evaluateChecks` function with fixture data so they
 * run offline — no real GitHub API calls. The fixtures are captured verbatim
 * from the live API at planning time (see scripts/__test__/fixtures/).
 *
 * applies ADR-009, avoids PF-013: every test prints counts; absence of checks
 * is explicitly FAIL (zero check-runs test).
 * avoids PF-017: cancelled/skipped/in_progress are all tested as NOT-PASS.
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

import {
  evaluateChecks,
  main,
  fetchRequiredContexts,
  fetchStatuses,
  fetchCheckRuns,
  fetchWorkflowRuns,
  parseGhStderrHttpStatus,
  EXPECTED_CONTEXTS,
  TIER_B_EXPECTED_SKIPPED,
  RELEASE_SURFACE,
  RELEASE_SURFACE_CONTEXTS,
  RELEASE_WORKFLOW_PATH,
  matchesReleaseSurface,
  releaseSuiteIdsFrom,
} from '../verify-pr-checks.mjs';

const ROOT = resolve(fileURLToPath(import.meta.url), '../../..');
const FIXTURES = join(ROOT, 'scripts/__test__/fixtures');

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

function loadProtection() {
  const raw = JSON.parse(readFileSync(join(FIXTURES, 'protection-main.json'), 'utf8'));
  return raw.required_status_checks.contexts;
}

function loadCheckRuns(fixtureName) {
  const raw = JSON.parse(readFileSync(join(FIXTURES, fixtureName), 'utf8'));
  return raw.check_runs ?? [];
}

function loadStatuses(fixtureName) {
  const raw = JSON.parse(readFileSync(join(FIXTURES, fixtureName), 'utf8'));
  return raw.statuses ?? [];
}

const REQUIRED = loadProtection();
// From the live protection fixture, the 6 required contexts are:
// "Rust — fmt, clippy, test", "MSRV (Rust 1.88)", "WASM — build & test",
// "JS packages — build & test (ubuntu-latest)",
// "JS packages — build & test (macos-latest)",
// "JS packages — build & test (windows-latest)"
assert.equal(REQUIRED.length, 6, 'historical fixture (2026-08) must have 6 required contexts');

const HEAD_113F472 = '113f472684d6ee7e398d54c1aadc22b2ad747ae1';
const HEAD_F168944 = 'f168944'; // PR #239
const HEAD_E9DACE1 = 'e9dace1'; // PR #240

// D-PR8: release.yml check-suite identity constants.
// RELEASE_SUITE: the release.yml pull_request suite for PR #366 head e02bcf2.
// CI_SUITE_113F472: the ci.yml suite in the 2026-08 113f472 fixture (a NON-release suite).
const RELEASE_SUITE = 92290758559;
const CI_SUITE_113F472 = 84976779019;
const RELEASE_SUITES = new Set([RELEASE_SUITE]);

/** Returns a copy of run with check_suite: { id } injected (defaults to RELEASE_SUITE). */
function withSuite(run, id = RELEASE_SUITE) {
  return { ...run, check_suite: { id } };
}

// Minimal runs stubs for the fetchWorkflowRuns call added to main() (D-PR8).
// Every main() stub test that reaches evaluateChecks needs one of these routes.
const RUNS_NONE = { total_count: 0, workflow_runs: [] };
const RUNS_RELEASE = {
  total_count: 1,
  workflow_runs: [{
    id: 34065573775,
    path: '.github/workflows/release.yml',
    event: 'pull_request',
    check_suite_id: RELEASE_SUITE,
    conclusion: 'success',
  }],
};

// A synthetic Source hygiene run (D-PR3b). The 113f472 fixture predates the
// source-hygiene job (#288); tests that verify a PASSING run today must inject one.
const SOURCE_HYGIENE_PASS = { name: 'Source hygiene', status: 'completed', conclusion: 'success' };

// ---------------------------------------------------------------------------
// AC-22: Historical fixtures reproduce correctly
// ---------------------------------------------------------------------------
describe('AC-21 AC-22: historical fixture evaluation', () => {

  test('113f472 (main baseline + Source hygiene) → PASS (exit 0)', () => {
    // The 113f472 fixture predates the source-hygiene job (added in #288).
    // A passing run today requires Source hygiene to be present and successful
    // (D-PR3b, EXPECTED_CONTEXTS). We inject a synthetic run to represent the
    // current expected state.
    const checkRuns = [...loadCheckRuns('checks-main-113f472.json'), SOURCE_HYGIENE_PASS];
    assert.equal(checkRuns.length, 19, 'fixture must have 18+1 check-runs');
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 0, `expected PASS; lines: ${result.lines.join('\n')}`);
    assert.ok(result.pass, 'evaluateChecks must return pass=true');
  });

  test('f168944 (PR #239, zero check-runs) → FAIL (exit 1) naming all 6 required contexts', () => {
    const checkRuns = loadCheckRuns('checks-pr239-f168944.json');
    const statuses = loadStatuses('status-pr239-f168944.json');
    assert.equal(checkRuns.length, 0, 'PR #239 fixture must have 0 check-runs');
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns, statuses, headSha: HEAD_F168944 });
    assert.equal(result.exitCode, 1, `expected FAIL; lines: ${result.lines.join('\n')}`);
    assert.ok(!result.pass);
    const allLines = result.lines.join('\n');
    // Non-vacuity guard fires: zero check-runs → FAIL
    assert.ok(allLines.includes('zero check-runs'), `must mention zero check-runs; got: ${allLines}`);
    // AC-22: every required context must be named so the operator knows what was absent,
    // not just that "zero check-runs" occurred (avoids vacuous failure messages).
    for (const ctx of REQUIRED) {
      assert.ok(allLines.includes(ctx),
        `must name absent required context "${ctx}"; got:\n${allLines}`);
    }
  });

  test('e9dace1 (PR #240, zero check-runs, Snyk error status) → FAIL (exit 1)', () => {
    const checkRuns = loadCheckRuns('checks-pr240-e9dace1.json');
    const statuses = loadStatuses('status-pr240-e9dace1.json');
    assert.equal(checkRuns.length, 0, 'PR #240 fixture must have 0 check-runs');
    const snykStatus = statuses.find(s => s.context === 'security/snyk (dean0x)');
    assert.ok(snykStatus, 'PR #240 fixture must have snyk status');
    assert.equal(snykStatus.state, 'error');
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns, statuses, headSha: HEAD_E9DACE1 });
    assert.equal(result.exitCode, 1, `expected FAIL; lines: ${result.lines.join('\n')}`);
    // Zero check-runs triggers non-vacuity guard; Snyk status is Tier C (advisory)
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('zero check-runs'), `must fail on zero check-runs; got: ${allLines}`);
  });

});

// ---------------------------------------------------------------------------
// AC-23: Partial case — the one `gh pr checks --required` exits 0 on
// ---------------------------------------------------------------------------
describe('AC-23: partial case (5 of 6 required present)', () => {

  test('17 of 18 check-runs (MSRV deleted) + Source hygiene → FAIL naming MSRV', () => {
    // Synthesize by removing the MSRV check-run from the 113f472 fixture.
    // This is the case `gh pr checks --required` exits 0 on (all present checks are green)
    // but the tool catches: a required context is absent.
    const allRuns = loadCheckRuns('checks-main-113f472.json');
    const msrvName = 'MSRV (Rust 1.88)';
    const withoutMsrv = [...allRuns.filter(cr => cr.name !== msrvName), SOURCE_HYGIENE_PASS];
    assert.equal(withoutMsrv.length, 18, 'should have 17+1 runs after removing MSRV');

    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns: withoutMsrv,
      statuses: [],
      headSha: HEAD_113F472,
    });
    assert.equal(result.exitCode, 1, 'must FAIL when one required context is absent');
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes(msrvName),
      `failure message must name "${msrvName}"; got: ${allLines}`);
    assert.ok(allLines.includes('not found') || allLines.includes('never ran'),
      `message must indicate the context never ran; got: ${allLines}`);
  });

});

// ---------------------------------------------------------------------------
// AC-24: All non-success terminal and non-terminal states fail (avoids PF-017)
// ---------------------------------------------------------------------------
describe('AC-24: non-success states → FAIL, quoting the observed state', () => {

  // Build a passing baseline from the 113f472 fixture + Source hygiene.
  function buildPassingRuns() {
    return [...loadCheckRuns('checks-main-113f472.json').map(cr => ({ ...cr })), { ...SOURCE_HYGIENE_PASS }];
  }

  const NON_SUCCESS_CASES = [
    { status: 'completed', conclusion: 'cancelled' },
    { status: 'completed', conclusion: 'skipped' },
    { status: 'completed', conclusion: 'neutral' },
    { status: 'completed', conclusion: 'timed_out' },
    { status: 'completed', conclusion: 'action_required' },
    { status: 'completed', conclusion: 'stale' },
    { status: 'queued',    conclusion: null },
    { status: 'in_progress', conclusion: null },
  ];

  for (const { status, conclusion } of NON_SUCCESS_CASES) {
    test(`required check with status=${status} conclusion=${conclusion ?? 'null'} → FAIL`, () => {
      const runs = buildPassingRuns();
      const target = runs.find(cr => REQUIRED.includes(cr.name));
      assert.ok(target, 'must find a required check-run to mutate');
      target.status = status;
      target.conclusion = conclusion;

      const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: runs, statuses: [], headSha: HEAD_113F472 });
      assert.equal(result.exitCode, 1, `status=${status} conclusion=${conclusion} must exit 1`);
      const allLines = result.lines.join('\n');
      // Message must quote the observed status verbatim (avoids PF-017)
      assert.ok(allLines.includes(status), `failure must quote observed status "${status}"`);
      if (conclusion) {
        assert.ok(allLines.includes(conclusion), `failure must quote observed conclusion "${conclusion}"`);
      }
    });
  }

  test('control: all-success baseline (with Source hygiene) still exits 0 (suite is not failing unconditionally)', () => {
    const runs = buildPassingRuns();
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: runs, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 0, 'all-success baseline must pass');
  });

  // D-PR3b: non-required check in queued/in_progress must also FAIL (Tier B fix).
  // PoC from the review: "6 required contexts completed+success plus
  // {name:'Source hygiene', status:'queued', conclusion:null} → exits 0" — WRONG.
  // After the fix, Tier B FAILs on any non-completed non-required run.
  test('non-required check with status=queued → FAIL (Tier B, D-PR3 fix)', () => {
    const runs = [
      ...loadCheckRuns('checks-main-113f472.json'),
      { name: 'Source hygiene', status: 'queued', conclusion: null },
    ];
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: runs, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 1,
      'a non-required queued run must prevent PASS (Tier B fix, D-PR3)');
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('Source hygiene'), `must name the pending job; got: ${allLines}`);
    assert.ok(allLines.includes('queued'), `must quote the observed status; got: ${allLines}`);
  });

  test('non-required check with status=in_progress → FAIL (Tier B, D-PR3 fix)', () => {
    const runs = [
      ...loadCheckRuns('checks-main-113f472.json'),
      { name: 'Some other job', status: 'in_progress', conclusion: null },
      { ...SOURCE_HYGIENE_PASS },
    ];
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: runs, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 1, 'a non-required in_progress run must prevent PASS');
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('in_progress'), `must quote the observed status; got: ${allLines}`);
  });

});

// ---------------------------------------------------------------------------
// D-PR3b: EXPECTED_CONTEXTS (Source hygiene) — absence detection
// Closing the gap: source-hygiene ABSENT → exit 0 was the described PoC.
// ---------------------------------------------------------------------------
describe('D-PR3b: Source hygiene absence detection (EXPECTED_CONTEXTS)', () => {

  test('EXPECTED_CONTEXTS entries each match a job name: in .github/workflows/ci.yml', () => {
    // avoids PF-013: pinning the constant against itself is a tautology — it proves nothing
    // about the real CI workflow. Renaming the job in ci.yml must make this test fail so the
    // developer knows EXPECTED_CONTEXTS needs updating too, rather than silently shipping a
    // verifier that reports "never ran" at merge time with a misleading diagnosis.
    const ciYml = readFileSync(join(ROOT, '.github/workflows/ci.yml'), 'utf8');
    // Job-level names appear at exactly 4-space indent: "    name: ..."
    // Step-level names have a leading dash:             "      - name: ..."
    const jobNames = ciYml
      .split('\n')
      .filter(line => /^    name: /.test(line))
      .map(line => line.replace(/^    name:\s+/, '').trim());
    assert.ok(EXPECTED_CONTEXTS.length > 0, 'EXPECTED_CONTEXTS must be non-empty');
    for (const ctx of EXPECTED_CONTEXTS) {
      assert.ok(
        jobNames.includes(ctx),
        `EXPECTED_CONTEXTS entry "${ctx}" must be a job name: in .github/workflows/ci.yml; ` +
        `found job names: ${jobNames.join(', ')}`,
      );
    }
  });

  test('Source hygiene ABSENT from check-runs → FAIL (the described PoC, D-PR3b)', () => {
    // PoC: 6 required contexts completed+success, Source hygiene absent entirely.
    // Before the fix, Tier B had nothing to iterate and emitted exitCode=0.
    // After the fix (EXPECTED_CONTEXTS with Tier A semantics), absence = FAIL.
    const checkRuns = loadCheckRuns('checks-main-113f472.json'); // no Source hygiene
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns,
      statuses: [],
      headSha: HEAD_113F472,
    });
    assert.equal(result.exitCode, 1,
      'Source hygiene absent must exit 1, not 0 (PoC from the review finding)');
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('Source hygiene'),
      `failure must name "Source hygiene"; got: ${allLines}`);
    // The absence message must indicate the job was not found
    assert.ok(
      allLines.includes('not found') || allLines.includes('never ran') || allLines.includes('absence'),
      `failure must indicate the job was not found; got: ${allLines}`,
    );
  });

  test('Source hygiene queued → FAIL (Tier A+ catches non-completed expected run)', () => {
    const checkRuns = [
      ...loadCheckRuns('checks-main-113f472.json'),
      { name: 'Source hygiene', status: 'queued', conclusion: null },
    ];
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 1, 'queued expected run must FAIL');
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('Source hygiene'), `must name the job; got: ${allLines}`);
    assert.ok(allLines.includes('queued'), `must quote the status; got: ${allLines}`);
  });

  test('Source hygiene in_progress → FAIL (Tier A+ catches non-completed expected run)', () => {
    const checkRuns = [
      ...loadCheckRuns('checks-main-113f472.json'),
      { name: 'Source hygiene', status: 'in_progress', conclusion: null },
    ];
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 1, 'in_progress expected run must FAIL');
  });

  test('Source hygiene failure → FAIL (Tier A+ catches failed expected run)', () => {
    const checkRuns = [
      ...loadCheckRuns('checks-main-113f472.json'),
      { name: 'Source hygiene', status: 'completed', conclusion: 'failure' },
    ];
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 1, 'failed expected run must FAIL');
  });

  test('Source hygiene present+success → does not fail (Tier A+ does not false-fail)', () => {
    const checkRuns = [
      ...loadCheckRuns('checks-main-113f472.json'),
      SOURCE_HYGIENE_PASS,
    ];
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 0, 'present-and-successful Source hygiene must not fail');
  });

  test('Source hygiene already in requiredContexts → not double-reported by Tier A+', () => {
    // If Open Decision 1 is applied and Source hygiene enters branch protection,
    // it appears in both requiredContexts and EXPECTED_CONTEXTS. The Tier A+
    // loop must skip it (already handled in Tier A), not double-fail it.
    const requiredWithHygiene = [...REQUIRED, 'Source hygiene'];
    const checkRuns = [...loadCheckRuns('checks-main-113f472.json'), SOURCE_HYGIENE_PASS];
    const result = evaluateChecks({ requiredContexts: requiredWithHygiene, checkRuns, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 0, 'Source hygiene in required set must not be double-reported');
    const allLines = result.lines.join('\n');
    const tierAplusCount = (allLines.match(/Tier A\+/g) ?? []).length;
    assert.equal(tierAplusCount, 0, 'Tier A+ must not fire when context is already in Tier A');
  });

});

// ---------------------------------------------------------------------------
// D-PR2a: Tier A checks BOTH namespaces independently (not if/else-if)
// A failing commit status must not be masked by a passing check-run.
// ---------------------------------------------------------------------------
describe('D-PR2a: Tier A checks both check-runs AND statuses independently', () => {

  test('required context in check-runs (success) AND statuses (failure) → FAIL', () => {
    // Before the fix, if/else-if meant the status branch was only reached when
    // no check-run existed. A failing status was silently ignored when a
    // check-run of the same name was green (narrow divergence from GitHub's
    // enforcement model per D-PR2a).
    const allRuns = [...loadCheckRuns('checks-main-113f472.json'), SOURCE_HYGIENE_PASS];
    const msrvName = 'MSRV (Rust 1.88)';
    // MSRV exists in check-runs (success), also in statuses (failure)
    const statuses = [{ context: msrvName, state: 'failure' }];
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns: allRuns,
      statuses,
      headSha: HEAD_113F472,
    });
    assert.equal(result.exitCode, 1,
      'failing status must not be masked by passing check-run (D-PR2a)');
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes(msrvName), `must name the failing context; got: ${allLines}`);
    assert.ok(allLines.includes('failure'), `must quote the failing state; got: ${allLines}`);
  });

  test('required context in check-runs (success) AND statuses (success) → PASS', () => {
    const allRuns = [...loadCheckRuns('checks-main-113f472.json'), SOURCE_HYGIENE_PASS];
    const msrvName = 'MSRV (Rust 1.88)';
    const statuses = [{ context: msrvName, state: 'success' }];
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns: allRuns,
      statuses,
      headSha: HEAD_113F472,
    });
    assert.equal(result.exitCode, 0,
      'required context in both namespaces (both success) must pass (D-PR2a)');
  });

});

// ---------------------------------------------------------------------------
// AC-25: Zero check-runs is never a pass (avoids PF-013)
// ---------------------------------------------------------------------------
describe('AC-25: zero check-runs never passes', () => {

  test('total_count=0, empty check_runs, even with success status → FAIL', () => {
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns: [],
      statuses: [{ context: 'some-check', state: 'success' }],
      headSha: HEAD_F168944,
    });
    assert.equal(result.exitCode, 1, 'zero check-runs must exit 1 regardless of statuses');
    const allLines = result.lines.join('\n');
    // Must print counts (avoids PF-013)
    assert.ok(allLines.includes('check-runs: 0'), `must print check-run count; got: ${allLines}`);
  });

  test('output always includes counts (applies ADR-009)', () => {
    const runs = [...loadCheckRuns('checks-main-113f472.json'), SOURCE_HYGIENE_PASS];
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: runs, statuses: [], headSha: HEAD_113F472 });
    const allLines = result.lines.join('\n');
    // Counts must appear whether pass or fail
    assert.ok(allLines.includes('check-runs:'), `must print check-runs count; got: ${allLines}`);
    assert.ok(allLines.includes('required contexts:'), `must print required-contexts count; got: ${allLines}`);
  });

});

// ---------------------------------------------------------------------------
// AC-26: Three-valued exit contract
// ---------------------------------------------------------------------------
describe('AC-26 AC-27: exit codes and merge command', () => {

  test('PASS → exit 0 with --admin --match-head-commit <sha> in output', () => {
    const runs = [...loadCheckRuns('checks-main-113f472.json'), SOURCE_HYGIENE_PASS];
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: runs, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 0);
    assert.ok(result.mergeCommand, 'PASS must produce a mergeCommand');
    // D-PR5: merge command must include --match-head-commit <headSha> (TOCTOU protection)
    assert.ok(result.mergeCommand.includes('--match-head-commit'), 'merge command must include --match-head-commit');
    assert.ok(result.mergeCommand.includes(HEAD_113F472), 'merge command must include the verified SHA');
    // --admin is required: main is protected and the sole code-owner cannot self-approve.
    // Emitting it here ensures the operator can copy the command verbatim — hand-editing
    // is where --match-head-commit gets dropped (avoids PF-017 recurrence).
    assert.ok(result.mergeCommand.includes('--admin'), 'merge command must include --admin');
  });

  test('FAIL → exit 1 (not 0, not 2)', () => {
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: [], statuses: [], headSha: HEAD_F168944 });
    assert.equal(result.exitCode, 1);
    assert.ok(!result.pass);
  });

  test('evaluateChecks never returns exit 0 when pass=false', () => {
    // Verify the invariant: exitCode===0 iff pass===true
    const failResult = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: [], statuses: [], headSha: 'abc' });
    assert.equal(failResult.exitCode === 0, failResult.pass,
      'exitCode===0 must equal pass===true');

    const passResult = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns: [...loadCheckRuns('checks-main-113f472.json'), SOURCE_HYGIENE_PASS],
      statuses: [],
      headSha: HEAD_113F472,
    });
    assert.equal(passResult.exitCode === 0, passResult.pass,
      'exitCode===0 must equal pass===true on pass case');
  });

});

// ---------------------------------------------------------------------------
// AC-26, AC-28, AC-29: the live path, driven end-to-end with an injected gh
// runner. These replace source-text greps: asserting that a file CONTAINS the
// string "process.exit(2)" proves nothing about whether that branch is
// reachable (applies ADR-009, avoids PF-013). Each case below drives main()
// and asserts the returned exit code.
// ---------------------------------------------------------------------------

const OK_GH_VERSION = () => ({ major: 2, minor: 88 });

/**
 * Build a gh runner stub from a route table. Each entry is matched against the
 * API path by substring; the value is either a JSON object (success) or an
 * error shape mirroring defaultGhRunner's output.
 *
 * Error shape uses `httpStatus` (not `status`) for HTTP error codes — the
 * process exit code is always 1 regardless of HTTP status, so `status` alone
 * cannot distinguish 404 from 403. The stub mirrors the parsed shape that
 * defaultGhRunner produces after fixing the medium finding (avoids PF-013:
 * dead branches that only trigger on a value the runner never produces).
 */
function stubRunner(routes, callLog = []) {
  return (args) => {
    const url = args[args.length - 1];
    callLog.push(url);
    for (const [needle, value] of routes) {
      if (url.includes(needle)) {
        return typeof value === 'function' ? value(url) : value;
      }
    }
    return { __error: true, httpStatus: 404, stderr: `no stub route for ${url}` };
  };
}

const PR_OK = { head: { sha: HEAD_113F472 }, base: { ref: 'main' } };
const PROTECTION_OK = JSON.parse(readFileSync(join(FIXTURES, 'protection-main.json'), 'utf8'));
const CHECKS_OK = JSON.parse(readFileSync(join(FIXTURES, 'checks-main-113f472.json'), 'utf8'));

// CHECKS_OK_WITH_HYGIENE: the 113f472 fixture + Source hygiene run, for happy-path
// tests that drive main() and expect exit 0. The 113f472 fixture predates #288;
// a PASS today requires Source hygiene to be present (D-PR3b, EXPECTED_CONTEXTS).
const CHECKS_OK_WITH_HYGIENE = {
  ...CHECKS_OK,
  check_runs: [...CHECKS_OK.check_runs, SOURCE_HYGIENE_PASS],
  total_count: CHECKS_OK.total_count + 1,
};

describe('AC-26 AC-28 AC-29: live path exit codes (injected runner)', () => {

  test('happy path → exit 0', () => {
    const runner = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', PROTECTION_OK],
      ['/check-runs', CHECKS_OK_WITH_HYGIENE],
      ['/status', { statuses: [], total_count: 0 }],
      ['/actions/runs', RUNS_NONE],
    ]);
    assert.equal(main(['1'], runner, OK_GH_VERSION), 0);
  });

  test('AC-29: unprotected base (404 on protection) → exit 2, never 0', () => {
    // httpStatus mirrors what defaultGhRunner produces after parsing "(HTTP NNN)"
    // from gh's stderr — the process exit code is always 1, not 404.
    const runner = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', { __error: true, httpStatus: 404, stderr: 'gh: Not Found (HTTP 404)' }],
      ['/check-runs', CHECKS_OK_WITH_HYGIENE],
      ['/status', { statuses: [], total_count: 0 }],
    ]);
    assert.equal(main(['1'], runner, OK_GH_VERSION), 2);
  });

  test('AC-29: --required-from branch also unprotected → exit 2', () => {
    const runner = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', { __error: true, httpStatus: 404, stderr: 'gh: Not Found (HTTP 404)' }],
    ]);
    assert.equal(main(['1', '--required-from', 'nope'], runner, OK_GH_VERSION), 2);
  });

  test('AC-26: protection unreadable (403) → exit 2', () => {
    const runner = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', { __error: true, httpStatus: 403, stderr: 'gh: Forbidden (HTTP 403)' }],
    ]);
    assert.equal(main(['1'], runner, OK_GH_VERSION), 2);
  });

  test('AC-29: message for unprotected base names the branch and suggests --required-from', () => {
    // Drives fetchRequiredContexts 404 path directly to verify message content.
    const runner = (_args) => ({ __error: true, httpStatus: 404, stderr: 'gh: Not Found (HTTP 404)' });
    const result = fetchRequiredContexts('wave/v0.4.0-wave1', null, runner);
    assert.ok(!result.ok);
    assert.equal(result.exitCode, 2);
    assert.ok(result.message.includes('wave/v0.4.0-wave1'),
      `message must name the base branch; got: ${result.message}`);
    assert.ok(result.message.includes('--required-from'),
      `message must mention --required-from; got: ${result.message}`);
  });

  test('AC-26: gh older than 2.31 → exit 2 before any API call', () => {
    const calls = [];
    const runner = stubRunner([['/pulls/', PR_OK]], calls);
    assert.equal(main(['1'], runner, () => ({ major: 2, minor: 30 })), 2);
    assert.equal(calls.length, 0, 'must not query the API when gh is too old');
  });

  test('AC-26: gh missing entirely (version probe returns null) → exit 2', () => {
    const runner = stubRunner([['/pulls/', PR_OK]]);
    assert.equal(main(['1'], runner, () => null), 2);
  });

  test('AC-26: no PR number argument → exit 2', () => {
    const runner = stubRunner([]);
    assert.equal(main([], runner, OK_GH_VERSION), 2);
  });

  test('AC-26: --required-from with no value → exit 2', () => {
    const runner = stubRunner([['/pulls/', PR_OK]]);
    assert.equal(main(['1', '--required-from'], runner, OK_GH_VERSION), 2);
  });

  test('protected branch listing ZERO required contexts → exit 2, not 0', () => {
    // The vacuous-green shape: protection exists, required set is empty.
    const runner = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', { required_status_checks: { contexts: [], checks: [] } }],
      ['/check-runs', CHECKS_OK_WITH_HYGIENE],
      ['/status', { statuses: [], total_count: 0 }],
    ]);
    assert.equal(main(['1'], runner, OK_GH_VERSION), 2);
  });

  test('required contexts are read from the UNION of contexts[] and checks[]', () => {
    // A protection payload that populates only the newer `checks` array must
    // still yield a required set — reading `contexts` alone would be empty.
    const onlyChecks = {
      required_status_checks: {
        contexts: [],
        checks: REQUIRED.map(c => ({ context: c, app_id: 15368 })),
      },
    };
    const runner = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', onlyChecks],
      ['/check-runs', CHECKS_OK_WITH_HYGIENE],
      ['/status', { statuses: [], total_count: 0 }],
      ['/actions/runs', RUNS_NONE],
    ]);
    assert.equal(main(['1'], runner, OK_GH_VERSION), 0);

    const res = fetchRequiredContexts('main', null, runner);
    assert.ok(res.ok);
    assert.deepEqual([...res.contexts].sort(), [...REQUIRED].sort());
  });

  test('AC-28: pagination stops at the page bound and exits 2 (never loops)', () => {
    // Stub a server that always reports more pages than it will ever deliver.
    let pages = 0;
    const fullPage = {
      total_count: 100000,
      check_runs: Array.from({ length: 100 }, (_, i) => ({
        name: `job-${i}`, status: 'completed', conclusion: 'success',
      })),
    };
    const runner = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', PROTECTION_OK],
      ['/check-runs', () => { pages++; return fullPage; }],
      ['/status', { statuses: [], total_count: 0 }],
    ]);
    assert.equal(main(['1'], runner, OK_GH_VERSION), 2, 'page cap must exit 2');
    assert.ok(pages <= 20, `pagination must be bounded; issued ${pages} page requests`);
    assert.ok(pages >= 2, 'the stub must actually have been paginated');
  });

  test('AC-28: total_count larger than the collected set → exit 2, not a partial verdict', () => {
    const truncated = { total_count: 18, check_runs: CHECKS_OK.check_runs.slice(0, 5) };
    const runner = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', PROTECTION_OK],
      ['/check-runs', truncated],
      ['/status', { statuses: [], total_count: 0 }],
    ]);
    assert.equal(main(['1'], runner, OK_GH_VERSION), 2);
  });

  test('AC-30: the live path issues at most page-bound + 3 fixed API calls (6 with single-page stubs)', () => {
    const calls = [];
    const runner = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', PROTECTION_OK],
      ['/check-runs', CHECKS_OK_WITH_HYGIENE],
      ['/status', { statuses: [], total_count: 0 }],
      ['/actions/runs', RUNS_NONE],
    ], calls);
    const start = Date.now();
    assert.equal(main(['1'], runner, OK_GH_VERSION), 0);
    const elapsed = Date.now() - start;
    // AC-30 (clause b): verifier must complete in under 15 s wall-clock.
    // With a synchronous stub runner, elapsed time reflects the verifier's own
    // CPU cost and any unexpected loops — network latency is zero.
    assert.ok(elapsed < 15000,
      `verifier must complete in < 15 s wall-clock (AC-30 clause b); took ${elapsed}ms`);
    // Fixed calls: pr, protection, checks, status, files, runs = 6.
    // Budget: fixed 3 + pages ≤ 20 + 30 + 5 = 58 worst case (was 53).
    // The /pulls/ stub matches the files URL (/pulls/1/files) and returns PR_OK (no .files → ok).
    assert.equal(calls.length, 6, `expected 6 API calls (pr, protection, checks, status, files, runs); got ${calls.length}`);
    const checkCall = calls.find(u => u.includes('/check-runs'));
    assert.ok(checkCall.includes('filter=latest'), 'filter=latest must be pinned explicitly (D-PR4a)');
    // D-PR4a parity: combined-status endpoint must request per_page=100 so a context
    // at position 31+ is not silently absent from the set (low finding fix).
    const statusCall = calls.find(u => u.includes('/status'));
    assert.ok(statusCall && statusCall.includes('per_page=100'),
      `status URL must include per_page=100 (D-PR4a parity); got: ${statusCall}`);
    // D-PR8: runs call must include head_sha and per_page=100
    const runsCall = calls.find(u => u.includes('/actions/runs'));
    assert.ok(runsCall, 'must have made a /actions/runs call');
    assert.ok(runsCall.includes('head_sha='), 'runs call must include head_sha=');
    assert.ok(runsCall.includes('per_page=100'), 'runs call must include per_page=100');
  });

  test('check-runs API error → exit 2 (indeterminate), not 1', () => {
    const runner = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', PROTECTION_OK],
      ['/check-runs', { __error: true, httpStatus: 500, stderr: 'server error' }],
    ]);
    assert.equal(main(['1'], runner, OK_GH_VERSION), 2);
  });

  test('a real FAIL still exits 1, so exit 2 has not swallowed the FAIL path', () => {
    const runner = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', PROTECTION_OK],
      ['/check-runs', { total_count: 0, check_runs: [] }],
      ['/status', { statuses: [], total_count: 0 }],
      ['/actions/runs', RUNS_NONE],
    ]);
    assert.equal(main(['1'], runner, OK_GH_VERSION), 1);
  });

  // D-PR4a parity: fetchStatuses total_count guard.
  // The combined-status endpoint caps at 30 statuses. If total_count > returned
  // count, a required context beyond position 30 would be falsely absent — fail
  // closed rather than silently evaluate a partial set (consistent with D-PR4a).
  test('fetchStatuses: total_count > returned statuses → exit 2 (partial set)', () => {
    const runner = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', PROTECTION_OK],
      ['/check-runs', CHECKS_OK_WITH_HYGIENE],
      // total_count claims 35 but only 2 are returned (API cap at 30 simulated)
      ['/status', { total_count: 35, statuses: [
        { context: 'foo', state: 'success' },
        { context: 'bar', state: 'success' },
      ] }],
    ]);
    assert.equal(main(['1'], runner, OK_GH_VERSION), 2,
      'truncated status list (total_count > returned) must exit 2');
  });

  test('fetchStatuses: total_count === returned statuses → proceeds normally', () => {
    // Non-truncated status response — should not block a passing run.
    const runner = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', PROTECTION_OK],
      ['/check-runs', CHECKS_OK_WITH_HYGIENE],
      ['/status', { total_count: 1, statuses: [{ context: 'foo', state: 'success' }] }],
      ['/actions/runs', RUNS_NONE],
    ]);
    assert.equal(main(['1'], runner, OK_GH_VERSION), 0,
      'non-truncated status list must not block a passing run');
  });

  test('fetchStatuses: status omitting total_count → proceeds (no false-fail)', () => {
    // Some stubs (and older fixtures) omit total_count; guard must not
    // fire when total_count is absent.
    const runner = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', PROTECTION_OK],
      ['/check-runs', CHECKS_OK_WITH_HYGIENE],
      ['/status', { statuses: [] }],  // no total_count
      ['/actions/runs', RUNS_NONE],
    ]);
    assert.equal(main(['1'], runner, OK_GH_VERSION), 0,
      'missing total_count must not cause a false exit 2');
  });

});

// ---------------------------------------------------------------------------
// High finding: parseGhStderrHttpStatus — pin stub contract to production parsing.
//
// defaultGhRunner calls parseGhStderrHttpStatus (an exported pure function) to
// extract the HTTP code from gh's stderr. Testing it here with captured real gh
// stderr strings ensures the stubs used throughout this file mirror the value the
// production runner actually produces (applies ADR-009, avoids PF-013: dead
// branches that only trigger on a value the runner never produces).
//
// Stub shape reminder: stubRunner error objects use `httpStatus: 404` (not
// `status: 404`). The process exit code is always 1 regardless of HTTP status;
// `status` alone cannot distinguish 404 from 403. The stubRunner JSDoc documents
// this contract; these tests pin the parser output that defines it.
// ---------------------------------------------------------------------------
describe('high finding: parseGhStderrHttpStatus parses real gh stderr format', () => {

  test('extracts 404 from the real gh Not Found format', () => {
    // Captured from: gh api /repos/dean0x/mdl/branches/nonexistent-xyz/protection
    // stderr output: "gh: Not Found (HTTP 404)"
    assert.equal(parseGhStderrHttpStatus('gh: Not Found (HTTP 404)'), 404,
      'must extract 404 from real gh stderr format');
  });

  test('extracts 403 from the real gh Forbidden format', () => {
    // Captured from: gh api on a branch with insufficient permissions
    // stderr output: "gh: Forbidden (HTTP 403)"
    assert.equal(parseGhStderrHttpStatus('gh: Forbidden (HTTP 403)'), 403,
      'must extract 403 from real gh stderr format');
  });

  test('returns null for a non-HTTP error (e.g. connection refused)', () => {
    // Connection errors have no "(HTTP NNN)" suffix — must not crash or return
    // a wrong code that triggers the 404/403 branch accidentally.
    assert.equal(parseGhStderrHttpStatus('connection refused'), null);
  });

  test('returns null for empty string', () => {
    assert.equal(parseGhStderrHttpStatus(''), null);
  });

  test('returns null for null/undefined (guard against caller passing undefined stderr)', () => {
    assert.equal(parseGhStderrHttpStatus(null), null);
    assert.equal(parseGhStderrHttpStatus(undefined), null);
  });

});

// ---------------------------------------------------------------------------
// Medium finding: defaultGhRunner populates httpStatus from stderr, not status.
// fetchRequiredContexts must branch on httpStatus (not the process exit code).
// Stubs mirror the parsed shape (applies ADR-009, avoids PF-013 dead branches).
// ---------------------------------------------------------------------------
describe('medium finding: fetchRequiredContexts branches on httpStatus, not process exit code', () => {

  test('httpStatus:404 → 404-specific message branch (not generic protection API error)', () => {
    const runner = (_args) => ({ __error: true, httpStatus: 404, stderr: 'gh: Not Found (HTTP 404)' });
    const result = fetchRequiredContexts('some-branch', null, runner);
    assert.ok(!result.ok);
    assert.equal(result.exitCode, 2);
    // The 404 branch must fire — not the generic fallthrough
    assert.ok(result.message.includes('no protection') || result.message.includes('404'),
      `must use the 404 branch; got: ${result.message}`);
    assert.ok(!result.message.includes('protection API error'),
      `must NOT fall through to generic error; got: ${result.message}`);
  });

  test('httpStatus:403 → 403-specific message branch (not generic protection API error)', () => {
    const runner = (_args) => ({ __error: true, httpStatus: 403, stderr: 'gh: Forbidden (HTTP 403)' });
    const result = fetchRequiredContexts('some-branch', null, runner);
    assert.ok(!result.ok);
    assert.equal(result.exitCode, 2);
    assert.ok(result.message.includes('403') || result.message.includes('permissions'),
      `must use the 403 branch; got: ${result.message}`);
    assert.ok(!result.message.includes('protection API error'),
      `must NOT fall through to generic error; got: ${result.message}`);
  });

  test('httpStatus:null (no HTTP code in stderr) → generic error branch', () => {
    // Simulates a non-API error (e.g. connection refused) where gh prints no HTTP code.
    // This is what the OLD runner returned for ALL errors (always status:1, never 404).
    // The fix: only the generic branch fires when httpStatus is null.
    const runner = (_args) => ({ __error: true, status: 1, httpStatus: null, stderr: 'connection refused' });
    const result = fetchRequiredContexts('some-branch', null, runner);
    assert.ok(!result.ok);
    assert.equal(result.exitCode, 2);
    assert.ok(result.message.includes('protection API error'),
      `non-HTTP error must fall through to generic branch; got: ${result.message}`);
  });

  test('httpStatus:undefined (old-shape stub) → generic error branch, not a throw', () => {
    // Backward-compat: a stub that only sets status (not httpStatus) must not
    // accidentally trigger the 404 or 403 branch via undefined === 404 → false.
    const runner = (_args) => ({ __error: true, status: 404, stderr: 'Not Found' });
    const result = fetchRequiredContexts('some-branch', null, runner);
    assert.ok(!result.ok);
    assert.equal(result.exitCode, 2);
    // httpStatus is undefined → neither 404 nor 403 branch fires → generic
    assert.ok(result.message.includes('protection API error'),
      `undefined httpStatus must not trigger the 404 branch; got: ${result.message}`);
  });

});

// ---------------------------------------------------------------------------
// Entry-point guard: the verifier must actually RUN wherever it is checked out
// ---------------------------------------------------------------------------
describe('the verifier runs from a spaced / symlinked path', () => {

  test('invoked with no arguments it exits 2 (usage), never a silent 0', () => {
    // A merge gate that no-ops and exits 0 is the worst possible failure mode:
    // the operator reads it as "verified" and merges. Copy the script to a path
    // with a space (mkdtemp is also symlinked on macOS) and confirm it runs.
    const dir = mkdtempSync(join(tmpdir(), 'mds verify space-'));
    try {
      assert.ok(dir.includes(' '), 'this test is meaningless unless the path has a space');
      const target = join(dir, 'verify-pr-checks.mjs');
      writeFileSync(target, readFileSync(join(ROOT, 'scripts/verify-pr-checks.mjs')));
      const r = spawnSync(process.execPath, [target], { encoding: 'utf8', timeout: 30000 });
      assert.equal(r.status, 2,
        `expected usage exit 2; got ${r.status} (0 means the script never ran). stdout: ${r.stdout}`);
      assert.ok(r.stderr.includes('Usage:'), `must print usage; got: ${r.stderr}`);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

});

// ---------------------------------------------------------------------------
// Tier B: non-required, non-expected check-runs
//
// Tier B uses a WHITELIST: only 'success' passes (security-13). All other
// conclusions (failure, cancelled, timed_out, action_required, stale,
// skipped, neutral, null, and any future GitHub conclusion) = FAIL.
// queued, in_progress (non-completed = indeterminate) → FAIL (avoids PF-017).
// null conclusion (completed but outcome unknown) → FAIL (reliability-10).
//
// Coverage requirement: explicit tests for every shape; success (completed)
// is the positive control that proves the describe block is not failing unconditionally.
// ---------------------------------------------------------------------------
describe('Tier B: non-required non-expected check-run states', () => {

  // Helper: 6 required contexts pass + Source hygiene passes; inject one extra.
  function baseRunsWith(extra) {
    return [
      ...loadCheckRuns('checks-main-113f472.json'),
      { ...SOURCE_HYGIENE_PASS },
      extra,
    ];
  }

  test('success (completed) → PASS (whitelist positive control: suite is not unconditionally failing)', () => {
    // This is the ONLY conclusion that passes Tier B. Positive control for the whitelist.
    const runs = baseRunsWith({ name: 'Some background job', status: 'completed', conclusion: 'success' });
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: runs, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 0, 'Tier B: completed/success non-required run must not block PASS');
  });

  test('queued (non-completed) → FAIL (avoids PF-017: indeterminate ≠ success)', () => {
    const runs = baseRunsWith({ name: 'Some background job', status: 'queued', conclusion: null });
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: runs, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 1, 'Tier B: queued non-required run must exit 1');
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('queued'), `must quote observed status; got: ${allLines}`);
    assert.ok(allLines.includes('Some background job'), `must name the job; got: ${allLines}`);
  });

  test('in_progress (non-completed) → FAIL (avoids PF-017: indeterminate ≠ success)', () => {
    const runs = baseRunsWith({ name: 'Some background job', status: 'in_progress', conclusion: null });
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: runs, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 1, 'Tier B: in_progress non-required run must exit 1');
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('in_progress'), `must quote observed status; got: ${allLines}`);
  });

  test('null conclusion (completed+null, anomalous) → FAIL, not silently dropped (reliability-10)', () => {
    // GitHub should never pair completed status with null conclusion, but if it does,
    // fail closed — do not silently drop an anomalous run (reliability-10).
    const runs = baseRunsWith({ name: 'Anomalous job', status: 'completed', conclusion: null });
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: runs, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 1, 'Tier B: completed+null conclusion must not be silently dropped');
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('Anomalous job'), `must name the anomalous job; got: ${allLines}`);
    assert.ok(allLines.includes('null'), `must quote the null conclusion; got: ${allLines}`);
  });

  test('failure (completed) → FAIL', () => {
    const runs = baseRunsWith({ name: 'Some background job', status: 'completed', conclusion: 'failure' });
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: runs, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 1, 'Tier B: completed/failure non-required run must exit 1');
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('failure'), `must quote conclusion; got: ${allLines}`);
  });

  test('cancelled (completed) → FAIL', () => {
    const runs = baseRunsWith({ name: 'Some background job', status: 'completed', conclusion: 'cancelled' });
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: runs, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 1, 'Tier B: completed/cancelled non-required run must exit 1');
  });

  test('skipped (completed) → FAIL (security-13: whitelist — only success passes)', () => {
    // security-13: Tier B now uses a whitelist. skipped is no longer advisory; it fails closed.
    // A ci.yml job carrying an `if:` or `paths:` filter can report skipped — fail-open there
    // would be a security hole on any such future job. Whitelist prevents this.
    const runs = baseRunsWith({ name: 'Some background job', status: 'completed', conclusion: 'skipped' });
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: runs, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 1, 'Tier B: skipped must fail (whitelist, security-13)');
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('skipped'), `failure must name the conclusion; got: ${allLines}`);
    assert.ok(allLines.includes('Some background job'), `failure must name the job; got: ${allLines}`);
  });

  test('neutral (completed) → FAIL (security-13: whitelist — only success passes)', () => {
    // security-13: Same rationale as skipped — whitelist rather than blacklist.
    const runs = baseRunsWith({ name: 'Some background job', status: 'completed', conclusion: 'neutral' });
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: runs, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 1, 'Tier B: neutral must fail (whitelist, security-13)');
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('neutral'), `failure must name the conclusion; got: ${allLines}`);
  });

  // Parameterized over all remaining FAIL conclusions to prevent regression:
  test('timed_out (completed) → FAIL', () => {
    const runs = baseRunsWith({ name: 'Some background job', status: 'completed', conclusion: 'timed_out' });
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: runs, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 1, 'Tier B: completed/timed_out non-required run must exit 1');
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('timed_out'), `must quote conclusion; got: ${allLines}`);
    assert.ok(allLines.includes('Some background job'), `must name the job; got: ${allLines}`);
  });

  test('action_required (completed) → FAIL', () => {
    const runs = baseRunsWith({ name: 'Some background job', status: 'completed', conclusion: 'action_required' });
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: runs, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 1, 'Tier B: completed/action_required non-required run must exit 1');
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('action_required'), `must quote conclusion; got: ${allLines}`);
  });

  test('stale (completed) → FAIL', () => {
    const runs = baseRunsWith({ name: 'Some background job', status: 'completed', conclusion: 'stale' });
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: runs, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 1, 'Tier B: completed/stale non-required run must exit 1');
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('stale'), `must quote conclusion; got: ${allLines}`);
  });

});

// ---------------------------------------------------------------------------
// Duplicate check-run names must not mask a failure
// ---------------------------------------------------------------------------
describe('duplicate check-run names are all evaluated', () => {

  test('a failing run is not masked by a later success under the same name', () => {
    const ctx = REQUIRED[0];
    const runs = [
      ...loadCheckRuns('checks-main-113f472.json').filter(cr => cr.name !== ctx),
      { name: ctx, status: 'completed', conclusion: 'failure' },
      { name: ctx, status: 'completed', conclusion: 'success' },
      SOURCE_HYGIENE_PASS,
    ];
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: runs, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 1,
      'a failing required check-run must fail even when a later run shares its name');
    assert.ok(result.lines.join('\n').includes('failure'), 'must quote the observed conclusion');
  });

  test('loop index is printed correctly when multiple runs share a name', () => {
    // Review finding: the message hardcoded "(1 of N)" for every run.
    // After fix: each failing run shows its own 1-based index.
    const ctx = REQUIRED[0];
    const runs = [
      ...loadCheckRuns('checks-main-113f472.json').filter(cr => cr.name !== ctx),
      { name: ctx, status: 'completed', conclusion: 'failure' },
      { name: ctx, status: 'completed', conclusion: 'failure' },
      { name: ctx, status: 'completed', conclusion: 'failure' },
      SOURCE_HYGIENE_PASS,
    ];
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: runs, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 1);
    const allLines = result.lines.join('\n');
    // All three runs share the name; each must show its own position.
    assert.ok(allLines.includes('1 of 3'), `first failing run must show "1 of 3"; got: ${allLines}`);
    assert.ok(allLines.includes('2 of 3'), `second failing run must show "2 of 3"; got: ${allLines}`);
    assert.ok(allLines.includes('3 of 3'), `third failing run must show "3 of 3"; got: ${allLines}`);
  });

});

// ---------------------------------------------------------------------------
// Vacuity guard on the pure function itself
// ---------------------------------------------------------------------------
describe('empty required set is indeterminate, never a pass', () => {

  test('evaluateChecks with zero required contexts → exit 2', () => {
    const result = evaluateChecks({
      requiredContexts: [],
      checkRuns: [{ name: 'anything', status: 'completed', conclusion: 'success' }],
      statuses: [],
      headSha: HEAD_113F472,
    });
    assert.equal(result.exitCode, 2, 'zero required contexts must be indeterminate (exit 2)');
    assert.equal(result.pass, false);
    assert.ok(!result.mergeCommand, 'must not emit a merge command it cannot justify');
  });

});

// ---------------------------------------------------------------------------
// AC-13 (documentation): D-PR2a union of check-runs and statuses
// ---------------------------------------------------------------------------
describe('D-PR2a: required context satisfied by commit status', () => {
  test('required context present only in statuses (not check-runs) → PASS', () => {
    // Build check-runs with one required context removed from check-runs,
    // but that context is present in commit statuses as success.
    const allRuns = loadCheckRuns('checks-main-113f472.json');
    const msrvName = 'MSRV (Rust 1.88)';
    const withoutMsrv = [...allRuns.filter(cr => cr.name !== msrvName), SOURCE_HYGIENE_PASS];

    // Simulate MSRV being satisfied via commit status instead
    const statuses = [{ context: msrvName, state: 'success' }];

    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns: withoutMsrv,
      statuses,
      headSha: HEAD_113F472,
    });
    assert.equal(result.exitCode, 0,
      'required context satisfied via commit status must pass (D-PR2a)');
  });
});

// ---------------------------------------------------------------------------
// Tier C: pending statuses are reported as advisory, not silently dropped
// ---------------------------------------------------------------------------
describe('Tier C: pending non-required status is reported as advisory', () => {

  test('state=pending non-required status emits an advisory line (not silently ignored)', () => {
    // A pending non-required status is the same indeterminate condition as a
    // non-completed Tier B run — both are "not yet resolved". Tier B now FAILs
    // on queued/in_progress (same delta). Tier C is advisory-only, but the
    // operator must still be able to see it, not have it vanish silently.
    const runs = [...loadCheckRuns('checks-main-113f472.json'), SOURCE_HYGIENE_PASS];
    const statuses = [{ context: 'security/snyk (dean0x)', state: 'pending' }];
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns: runs,
      statuses,
      headSha: HEAD_113F472,
    });
    // PASS overall (non-required, advisory only)
    assert.equal(result.exitCode, 0, `pending non-required status must not cause FAIL; lines: ${result.lines.join('\n')}`);
    const allLines = result.lines.join('\n');
    // The advisory line must be present so the operator sees the pending status
    assert.ok(
      allLines.includes('advisory (Tier C)') && allLines.includes('pending'),
      `must emit an advisory line for pending non-required status; got:\n${allLines}`,
    );
  });

  test('state=error non-required status still emits advisory (regression guard)', () => {
    // Guard against the Tier C rewrite accidentally dropping non-pending errors.
    const runs = [...loadCheckRuns('checks-main-113f472.json'), SOURCE_HYGIENE_PASS];
    const statuses = [{ context: 'security/snyk (dean0x)', state: 'error' }];
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns: runs,
      statuses,
      headSha: HEAD_113F472,
    });
    assert.equal(result.exitCode, 0, 'non-required error status must not cause FAIL');
    const allLines = result.lines.join('\n');
    assert.ok(
      allLines.includes('advisory (Tier C)') && allLines.includes('error'),
      `must emit an advisory line for error non-required status; got:\n${allLines}`,
    );
  });

});

// ---------------------------------------------------------------------------
// reliability-02: EXPECTED_CONTEXTS absence detection for all 4 non-required jobs
//
// The 113f472 fixture includes all 6 Python matrix runs, examples/ gitignore
// coverage, and Python — wheel install smoke. These tests verify that removing
// any of the three newly-added expected contexts causes FAIL. Each test runs
// with the 6 required contexts passing and Source hygiene passing, but the
// target context absent.
// ---------------------------------------------------------------------------
describe('reliability-02: EXPECTED_CONTEXTS covers all 4 non-required CI jobs', () => {

  // Base: only required runs + Source hygiene.
  function requiredPlusHygiene() {
    const allRuns = loadCheckRuns('checks-main-113f472.json');
    // Keep only runs that are required (in REQUIRED set) or Source hygiene.
    return allRuns.filter(cr => REQUIRED.includes(cr.name) || cr.name === 'Source hygiene')
      .concat([{ ...SOURCE_HYGIENE_PASS }]);
  }

  test('Python — build & test: all matrix runs absent → FAIL (reliability-02)', () => {
    // Only required runs + Source hygiene; all Python matrix runs absent.
    // Before fix: no EXPECTED_CONTEXTS entry → Tier B has nothing to iterate → PASS.
    // After fix: Tier A+ detects absence via prefix match → FAIL.
    const checkRuns = requiredPlusHygiene();
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 1, 'Python — build & test absent must FAIL (reliability-02)');
    const allLines = result.lines.join('\n');
    assert.ok(
      allLines.includes('Python') && (allLines.includes('not found') || allLines.includes('never ran')),
      `failure must mention Python absence; got:\n${allLines}`,
    );
  });

  test('examples/ gitignore coverage: absent → FAIL (reliability-02)', () => {
    const checkRuns = loadCheckRuns('checks-main-113f472.json')
      .filter(cr => cr.name !== 'examples/ gitignore coverage')
      .concat([{ ...SOURCE_HYGIENE_PASS }]);
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 1, 'examples/ gitignore coverage absent must FAIL (reliability-02)');
    const allLines = result.lines.join('\n');
    assert.ok(
      allLines.includes('examples/ gitignore coverage'),
      `failure must name the absent job; got:\n${allLines}`,
    );
  });

  test('Python — wheel install smoke: absent → FAIL (reliability-02)', () => {
    const checkRuns = loadCheckRuns('checks-main-113f472.json')
      .filter(cr => cr.name !== 'Python — wheel install smoke')
      .concat([{ ...SOURCE_HYGIENE_PASS }]);
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 1, 'Python — wheel install smoke absent must FAIL (reliability-02)');
    const allLines = result.lines.join('\n');
    assert.ok(
      allLines.includes('Python — wheel install smoke'),
      `failure must name the absent job; got:\n${allLines}`,
    );
  });

  test('Python — build & test: one matrix variant failed → FAIL (prefix match catches it)', () => {
    const checkRuns = [
      ...loadCheckRuns('checks-main-113f472.json').map(cr => {
        // Corrupt one Python matrix run
        if (cr.name === 'Python — build & test (ubuntu-latest, 3.11)') {
          return { ...cr, conclusion: 'failure' };
        }
        return cr;
      }),
      { ...SOURCE_HYGIENE_PASS },
    ];
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 1, 'a failing Python matrix run must FAIL (Tier A+)');
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('failure'), `must quote the conclusion; got:\n${allLines}`);
  });

  test('all 4 expected contexts present+success → PASS (full fixture, no double-count)', () => {
    // The full 113f472 fixture includes all 6 Python matrix runs, examples/ gitignore,
    // and Python — wheel install smoke. With Source hygiene injected, all 4 EXPECTED_CONTEXTS
    // are present and passing → exit 0.
    const checkRuns = [...loadCheckRuns('checks-main-113f472.json'), { ...SOURCE_HYGIENE_PASS }];
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 0, 'all 4 expected contexts present and passing must PASS');
  });

  test('EXPECTED_CONTEXTS entries each match a job name: in .github/workflows/ci.yml (full set)', () => {
    // Existing coverage: 'Source hygiene'. This test verifies all 4 EXPECTED_CONTEXTS entries.
    const ciYml = readFileSync(join(ROOT, '.github/workflows/ci.yml'), 'utf8');
    const jobNames = ciYml
      .split('\n')
      .filter(line => /^    name: /.test(line))
      .map(line => line.replace(/^    name:\s+/, '').trim());
    assert.equal(EXPECTED_CONTEXTS.length, 4, 'EXPECTED_CONTEXTS must have 4 entries (reliability-02)');
    for (const ctx of EXPECTED_CONTEXTS) {
      assert.ok(
        jobNames.includes(ctx),
        `EXPECTED_CONTEXTS entry "${ctx}" must be a job name: in .github/workflows/ci.yml; ` +
        `found job names: ${jobNames.join(', ')}`,
      );
    }
  });

});

// ---------------------------------------------------------------------------
// complexity-08: argument parsing and merge command PR number
// ---------------------------------------------------------------------------
describe('complexity-08: argument parsing and merge command', () => {

  test('--required-from value is not consumed as the PR number', () => {
    // Before fix: argv.find(/^\d+$/) picked up the --required-from value '123'
    // when '456' was the intended PR number. After fix: --required-from and its
    // value are excluded from the PR number search.
    const calls = [];
    const runner = (args) => {
      const url = args[args.length - 1];
      calls.push(url);
      if (url.includes('/pulls/456')) return { head: { sha: HEAD_113F472 }, base: { ref: 'main' } };
      if (url.includes('/pulls/')) return { __error: true, httpStatus: 404, stderr: 'Not Found' };
      if (url.includes('/protection')) return PROTECTION_OK;
      if (url.includes('/check-runs')) return CHECKS_OK_WITH_HYGIENE;
      if (url.includes('/status')) return { statuses: [], total_count: 0 };
      if (url.includes('/actions/runs')) return RUNS_NONE;
      return { __error: true, httpStatus: 404, stderr: 'no route' };
    };
    // '123' is the branch name for --required-from; '456' is the PR number.
    assert.equal(main(['--required-from', '123', '456'], runner, OK_GH_VERSION), 0,
      'should succeed verifying PR 456 with --required-from 123');
    const prCall = calls.find(u => u.includes('/pulls/'));
    assert.ok(prCall && prCall.includes('/pulls/456'),
      `PR API call must use /pulls/456; got: ${prCall}`);
  });

  test('unknown flags are rejected with exit 2 (not silently ignored)', () => {
    // Before fix: unknown flags were silently dropped; '1' was found by argv.find.
    // After fix: any unrecognized flag causes exit 2 before API calls.
    const calls = [];
    const runner = (args) => { calls.push(args[args.length - 1]); return PR_OK; };
    assert.equal(main(['1', '--verbose'], runner, OK_GH_VERSION), 2,
      'unknown flags must cause exit 2');
    assert.equal(calls.length, 0, 'no API calls must be made when args are rejected');
  });

  test('merge command includes the explicit PR number (D-PR5 / complexity-08)', () => {
    // Before fix: emitted 'gh pr merge --squash --admin --match-head-commit <sha>'
    // with no PR number — relying on gh resolving from current branch. Ambiguous.
    // After fix: PR number is included so the command is unambiguous.
    const runs = [...loadCheckRuns('checks-main-113f472.json'), SOURCE_HYGIENE_PASS];
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns: runs,
      statuses: [],
      headSha: HEAD_113F472,
      prNumber: 314,
    });
    assert.equal(result.exitCode, 0);
    assert.ok(result.mergeCommand, 'PASS must produce a mergeCommand');
    assert.ok(result.mergeCommand.includes('314'),
      `merge command must include PR number 314; got: ${result.mergeCommand}`);
    // Ensure it's a positional argument, not a flag: 'gh pr merge 314 --squash ...'
    assert.ok(/gh pr merge 314\b/.test(result.mergeCommand),
      `PR number must be positional in the merge command; got: ${result.mergeCommand}`);
  });

  test('merge command from live path (main()) includes the PR number', () => {
    // Drives main() end-to-end and verifies the printed merge command contains the PR number.
    const lines = [];
    const runner = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', PROTECTION_OK],
      ['/check-runs', CHECKS_OK_WITH_HYGIENE],
      ['/status', { statuses: [], total_count: 0 }],
      ['/actions/runs', RUNS_NONE],
    ]);
    // Intercept console.log to capture the merge command line.
    const origLog = console.log;
    console.log = (...args) => lines.push(args.join(' '));
    try {
      const exitCode = main(['1'], runner, OK_GH_VERSION);
      assert.equal(exitCode, 0);
    } finally {
      console.log = origLog;
    }
    const mergeLine = lines.find(l => l.includes('gh pr merge'));
    assert.ok(mergeLine, `output must include a merge command line; got:\n${lines.join('\n')}`);
    assert.ok(mergeLine.includes('1'),
      `merge command must include PR number 1; got: ${mergeLine}`);
  });

});

// ---------------------------------------------------------------------------
// security-11: URL encoding of network-derived values
// ---------------------------------------------------------------------------
describe('security-11: URL encoding of branch and SHA values', () => {

  test('branch name with special characters is URL-encoded in protection API call', () => {
    // Branch falls back to baseBranch from the PR API response — network-derived.
    // Special characters in a branch name must be encoded before URL construction.
    const specialBranch = 'feat+my branch';
    const calls = [];
    const runner = (args) => {
      calls.push(args[args.length - 1]);
      return { __error: true, httpStatus: 404, stderr: 'not found' };
    };
    fetchRequiredContexts(specialBranch, null, runner);
    const protectionCall = calls.find(u => u.includes('protection'));
    assert.ok(protectionCall, 'must have made a protection API call');
    const encoded = encodeURIComponent(specialBranch);
    assert.ok(
      protectionCall.includes(encoded),
      `branch name must be URL-encoded; expected ${encoded} in: ${protectionCall}`,
    );
    assert.ok(
      !protectionCall.includes('+my branch'),
      `unencoded branch name must not appear in URL; got: ${protectionCall}`,
    );
  });

  test('--required-from branch name is URL-encoded in protection API call', () => {
    // requiredFrom comes from argv (operator input); still validate encoding.
    const specialBranch = 'release+2026';
    const calls = [];
    const runner = (args) => {
      calls.push(args[args.length - 1]);
      return { __error: true, httpStatus: 404, stderr: 'not found' };
    };
    fetchRequiredContexts('main', specialBranch, runner);
    const protectionCall = calls.find(u => u.includes('protection'));
    assert.ok(protectionCall, 'must have made a protection API call');
    assert.ok(
      protectionCall.includes(encodeURIComponent(specialBranch)),
      `--required-from branch must be URL-encoded; got: ${protectionCall}`,
    );
  });

});

// ---------------------------------------------------------------------------
// architecture-08: fetchCheckRuns — direct unit tests for the bounded loop
// and total_count guard (D-PR4a). These tests drive fetchCheckRuns directly
// without going through main(), so the bound is pinned independently of the
// PR-fetch and protection-fetch steps.
//
// MAX_PAGES is 20 (verify-pr-checks.mjs line ~96). The call-count assertion
// in the first test pins this: raising MAX_PAGES or removing the cap causes
// that test to fail immediately.
// ---------------------------------------------------------------------------
describe('architecture-08: fetchCheckRuns — bounded loop and total_count guard', () => {

  test('MAX_PAGES=20 bound: exactly 20 page requests are made, then returns exit 2', () => {
    // Runner always returns a full page with total_count far larger than perPage,
    // so the loop never terminates naturally. The hard cap must fire after 20 pages.
    let calls = 0;
    const fullPage = {
      total_count: 100_000,
      check_runs: Array.from({ length: 100 }, (_, i) => ({
        name: `job-${i}`, status: 'completed', conclusion: 'success',
      })),
    };
    const runner = (_args) => { calls++; return fullPage; };
    const result = fetchCheckRuns('abc123sha', runner);
    assert.ok(!result.ok, 'must return ok:false when pagination exceeds MAX_PAGES');
    assert.equal(result.exitCode, 2, 'pagination cap must produce exit 2 (D-PR4a)');
    assert.ok(
      result.message.includes('pagination exceeded') || result.message.includes('20 pages'),
      `message must describe the page cap; got: ${result.message}`,
    );
    // Pinning the call count to 20 ensures this test fails if MAX_PAGES is raised or removed.
    assert.equal(calls, 20,
      'must make exactly 20 page requests before bailing (pinned to MAX_PAGES=20; update if the constant changes)');
  });

  test('total_count guard: collected < declared total_count → exit 2, not a partial verdict', () => {
    // Single page returns 5 runs but total_count declares 50. The loop breaks
    // (5 < perPage=100), but collected count does not equal total_count.
    // fetchCheckRuns must refuse to return a partial result (D-PR4a).
    const runner = (_args) => ({
      total_count: 50,
      check_runs: Array.from({ length: 5 }, (_, i) => ({
        name: `job-${i}`, status: 'completed', conclusion: 'success',
      })),
    });
    const result = fetchCheckRuns('abc123sha', runner);
    assert.ok(!result.ok, 'partial total_count must return ok:false');
    assert.equal(result.exitCode, 2, 'partial total_count must produce exit 2 (D-PR4a)');
    assert.ok(
      result.message.includes('total_count') || result.message.includes('partial page set'),
      `message must mention the partial count; got: ${result.message}`,
    );
  });

  test('API error on first page → exit 2 (indeterminate), not a partial verdict', () => {
    const runner = (_args) => ({ __error: true, status: 1, httpStatus: 500, stderr: 'internal server error' });
    const result = fetchCheckRuns('abc123sha', runner);
    assert.ok(!result.ok, 'API error must return ok:false');
    assert.equal(result.exitCode, 2, 'API error must produce exit 2');
    assert.ok(
      result.message.includes('check-runs API error'),
      `message must describe the error; got: ${result.message}`,
    );
  });

  test('single complete page (runs.length < perPage, total_count matches) → ok:true', () => {
    const expected = [
      { name: 'job-a', status: 'completed', conclusion: 'success' },
      { name: 'job-b', status: 'completed', conclusion: 'failure' },
    ];
    const runner = (_args) => ({ total_count: 2, check_runs: expected });
    const result = fetchCheckRuns('abc123sha', runner);
    assert.ok(result.ok, `must return ok:true for a complete single-page result; got: ${JSON.stringify(result)}`);
    assert.deepEqual(result.checkRuns, expected, 'must return all check-runs unchanged');
  });

});

// ---------------------------------------------------------------------------
// TIER_B_EXPECTED_SKIPPED: release publish jobs may be skipped on a PR-branch
// dry-run (guarded by startsWith(github.ref,'refs/tags/v') or the testpypi
// dispatch input). Five names: 'Publish to crates.io', 'Publish to npm',
// 'Publish to PyPI', 'GitHub Release', 'Publish to TestPyPI (rehearsal)';
// only when 'skipped', pass Tier B. Any other conclusion or any other name
// still fails. (ADR-013 amendment 2026-09-06)
// ---------------------------------------------------------------------------
describe('TIER_B_EXPECTED_SKIPPED: release dry-run skipped publish jobs', () => {

  // Helper: build a full passing run set (all required + expected contexts)
  // then inject extra check-runs from the caller.
  function basePassingRunsWith(extras) {
    return [
      ...loadCheckRuns('checks-main-113f472.json'),
      { ...SOURCE_HYGIENE_PASS },
      ...extras,
    ];
  }

  test('D-PR5a: all five publish names skipped in release suite → PASS; each allowed line names suite id (D-PR8)', () => {
    // The RELEASING.md dry-run dispatched on a PR branch sees the five publish
    // jobs as skipped (their refs/tags/v or inputs. guard fires). The verifier must exit 0
    // so the operator can proceed to tag. D-PR8: each run must be in a release.yml suite.
    const skippedPublishRuns = [...TIER_B_EXPECTED_SKIPPED].map(name => withSuite({
      name,
      status: 'completed',
      conclusion: 'skipped',
    }));
    const runs = basePassingRunsWith(skippedPublishRuns);
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns: runs,
      statuses: [],
      headSha: HEAD_113F472,
      prNumber: 338,
      releaseSuiteIds: RELEASE_SUITES,
    });
    assert.equal(result.exitCode, 0,
      `five publish names skipped in release suite must not block PASS; lines:\n${result.lines.join('\n')}`);
    assert.ok(result.pass, 'must return pass=true');
    assert.ok(result.mergeCommand, 'PASS must produce a merge command');
    // D-PR8: each allowed line must say 'allowed' and name the suite id
    for (const name of TIER_B_EXPECTED_SKIPPED) {
      const line = result.lines.find(l => l.includes(name));
      assert.ok(line, `output must include a line for skipped job "${name}"; got:\n${result.lines.join('\n')}`);
      assert.ok(line.includes('allowed'),
        `output for "${name}" must say "allowed"; got: ${line}`);
      assert.ok(line.includes(String(RELEASE_SUITE)),
        `output for "${name}" must name suite id ${RELEASE_SUITE}; got: ${line}`);
    }
  });

  test('D-PR5b: "Publish to npm" with conclusion=cancelled → FAIL (only skipped is allowed)', () => {
    // A cancelled publish run is not the refs/tags/v guard — it is a real
    // failure that must block the merge. Only conclusion=skipped is allowed.
    const runs = basePassingRunsWith([
      withSuite({ name: 'Publish to npm', status: 'completed', conclusion: 'cancelled' }),
    ]);
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns: runs,
      statuses: [],
      headSha: HEAD_113F472,
      releaseSuiteIds: RELEASE_SUITES,
    });
    assert.equal(result.exitCode, 1,
      'cancelled Publish to npm must exit 1 (only skipped is allowed in TIER_B_EXPECTED_SKIPPED)');
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('Publish to npm'), `must name the failing job; got:\n${allLines}`);
    assert.ok(allLines.includes('cancelled'), `must quote the conclusion; got:\n${allLines}`);
  });

  test('D-PR5c: Tier B allowance never satisfies a Tier A required context', () => {
    // The TIER_B_EXPECTED_SKIPPED allowance lives in the Tier B loop only. If a
    // mutation leaked it into the Tier A loop, a required "Publish to npm" with
    // conclusion=skipped would pass when it must not (avoids PF-017). Making
    // "Publish to npm" required here pins that boundary.
    const runs = basePassingRunsWith([
      withSuite({ name: 'Publish to npm', status: 'completed', conclusion: 'skipped' }),
    ]);
    const result = evaluateChecks({
      requiredContexts: [...REQUIRED, 'Publish to npm'],
      checkRuns: runs,
      statuses: [],
      headSha: HEAD_113F472,
      releaseSuiteIds: RELEASE_SUITES,
    });
    assert.equal(result.exitCode, 1,
      'a required context with conclusion=skipped must fail Tier A even if it is in ' +
      `TIER_B_EXPECTED_SKIPPED; got:\n${result.lines.join('\n')}`);
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('Publish to npm'),
      `must name the failing Tier A context; got:\n${allLines}`);
    assert.ok(allLines.includes('avoids PF-017'),
      `must cite avoids PF-017 in the Tier A failure message; got:\n${allLines}`);
  });

  test('D-PR5d: an unrelated Tier B name with conclusion=skipped → FAIL (whitelist is exact)', () => {
    // The allowance is exactly the five publish job names. Any other job name
    // that reports skipped must still fail Tier B (security-13: whitelist).
    const runs = basePassingRunsWith([
      withSuite({ name: 'Some other job', status: 'completed', conclusion: 'skipped' }),
    ]);
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns: runs,
      statuses: [],
      headSha: HEAD_113F472,
      releaseSuiteIds: RELEASE_SUITES,
    });
    assert.equal(result.exitCode, 1,
      'skipped run under an unlisted name must fail (whitelist is exact, security-13)');
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('Some other job'), `must name the job; got:\n${allLines}`);
    assert.ok(allLines.includes('skipped'), `must quote the conclusion; got:\n${allLines}`);
  });

  test('D-PR5e: "Publish to npm" in_progress → FAIL (not-yet-completed guard precedes allowance, PF-017)', () => {
    // The not-yet-completed guard (status !== 'completed') in Tier B fires before
    // the TIER_B_EXPECTED_SKIPPED allowance. An in_progress publish run must
    // still block the merge — widening the allowance to accept status!='completed'
    // for the five names would silently break PF-017 (avoids that mutation).
    const runs = basePassingRunsWith([
      withSuite({ name: 'Publish to npm', status: 'in_progress', conclusion: null }),
    ]);
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns: runs,
      statuses: [],
      headSha: HEAD_113F472,
      releaseSuiteIds: RELEASE_SUITES,
    });
    assert.equal(result.exitCode, 1,
      'in_progress publish run must exit 1 (not-yet-completed guard, avoids PF-017)');
    const allLines = result.lines.join('\n');
    assert.ok(
      allLines.includes('not yet completed') || allLines.includes('in_progress'),
      `failure must mention not-yet-completed or in_progress; got:\n${allLines}`,
    );
  });

  test('D-PR5f: "Publish to TestPyPI (rehearsal)" with conclusion=cancelled → FAIL (only skipped is allowed)', () => {
    // ADR-013 amendment: this job is added to TIER_B_EXPECTED_SKIPPED so that
    // a skipped run (dispatch-input-guarded) does not block the verifier.
    // But cancelled is NOT skipped — it is a real anomaly and must fail closed.
    const runs = basePassingRunsWith([
      withSuite({ name: 'Publish to TestPyPI (rehearsal)', status: 'completed', conclusion: 'cancelled' }),
    ]);
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns: runs,
      statuses: [],
      headSha: HEAD_113F472,
      releaseSuiteIds: RELEASE_SUITES,
    });
    assert.equal(result.exitCode, 1,
      'cancelled Publish to TestPyPI must exit 1 (only skipped is allowed in TIER_B_EXPECTED_SKIPPED)');
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('Publish to TestPyPI'), `must name the failing job; got:\n${allLines}`);
    assert.ok(allLines.includes('cancelled'), `must quote the conclusion; got:\n${allLines}`);
  });

});

// ---------------------------------------------------------------------------
// D-PR7: Release-surface presence check
//
// When a PR touches .github/workflows/release.yml, .github/actions/**,
// crates/mds-napi/**, crates/mds-python/**, or scripts/verify-napi-names.mjs,
// the verifier REQUIRES completed+success runs for each RELEASE_SURFACE_CONTEXTS
// job: "Version gate", "Stage + verify platform packages",
// "Rehearse PyPI publish (no upload)".
// ---------------------------------------------------------------------------
describe('D-PR7: release-surface presence check', () => {

  // Helper: all required + expected passes; inject extras.
  function passingRunsWith(extras) {
    return [
      ...loadCheckRuns('checks-main-113f472.json'),
      { ...SOURCE_HYGIENE_PASS },
      ...extras,
    ];
  }

  // Three release-surface jobs, all success, each attributed to RELEASE_SUITE (D-PR8).
  function releaseSuccessRuns() {
    return RELEASE_SURFACE_CONTEXTS.map(name => withSuite({
      name,
      status: 'completed',
      conclusion: 'success',
    }));
  }

  test('D-PR7a: changedFiles touches release surface + all RELEASE_SURFACE_CONTEXTS present/success → exit 0 and "release surface touched" line', () => {
    const checkRuns = passingRunsWith(releaseSuccessRuns());
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns,
      statuses: [],
      headSha: HEAD_113F472,
      changedFiles: ['.github/workflows/release.yml'],
      releaseSuiteIds: RELEASE_SUITES,
    });
    assert.equal(result.exitCode, 0,
      `touched release surface + all contexts success must exit 0; lines:\n${result.lines.join('\n')}`);
    const allLines = result.lines.join('\n');
    assert.ok(
      allLines.includes('release surface touched'),
      `output must include "release surface touched"; got:\n${allLines}`,
    );
  });

  test('D-PR7b: touched + "Version gate" absent → exit 1 naming it (#342)', () => {
    const withoutVersionGate = releaseSuccessRuns().filter(r => r.name !== 'Version gate');
    const checkRuns = passingRunsWith(withoutVersionGate);
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns,
      statuses: [],
      headSha: HEAD_113F472,
      changedFiles: ['.github/workflows/release.yml'],
      releaseSuiteIds: RELEASE_SUITES,
    });
    assert.equal(result.exitCode, 1,
      `absent "Version gate" with release surface touched must exit 1; got:\n${result.lines.join('\n')}`);
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('Version gate'), `must name the absent job; got:\n${allLines}`);
  });

  test('D-PR7c: touched + "Rehearse PyPI publish (no upload)" completed+skipped → exit 1', () => {
    const withSkippedRehearse = releaseSuccessRuns().map(r =>
      r.name === 'Rehearse PyPI publish (no upload)'
        ? { ...r, conclusion: 'skipped' }
        : r,
    );
    const checkRuns = passingRunsWith(withSkippedRehearse);
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns,
      statuses: [],
      headSha: HEAD_113F472,
      changedFiles: ['.github/workflows/release.yml'],
      releaseSuiteIds: RELEASE_SUITES,
    });
    assert.equal(result.exitCode, 1,
      'release surface requires success, not skipped; must exit 1');
    const allLines = result.lines.join('\n');
    assert.ok(
      allLines.includes('Rehearse PyPI publish (no upload)'),
      `must name the non-success context; got:\n${allLines}`,
    );
  });

  test('D-PR7d: touched + one present run in_progress → exit 1', () => {
    const withInProgress = releaseSuccessRuns().map(r =>
      r.name === 'Stage + verify platform packages'
        ? withSuite({ name: r.name, status: 'in_progress', conclusion: null })
        : r,
    );
    const checkRuns = passingRunsWith(withInProgress);
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns,
      statuses: [],
      headSha: HEAD_113F472,
      changedFiles: ['crates/mds-napi/src/lib.rs'],
      releaseSuiteIds: RELEASE_SUITES,
    });
    assert.equal(result.exitCode, 1, 'in_progress release job with touched surface must exit 1');
  });

  test('D-PR7e: changedFiles = ["crates/mds-core/src/lib.rs"], no release runs → exit 0 and "not touched" line', () => {
    const checkRuns = passingRunsWith([]);
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns,
      statuses: [],
      headSha: HEAD_113F472,
      changedFiles: ['crates/mds-core/src/lib.rs'],
      releaseSuiteIds: RELEASE_SUITES,
    });
    assert.equal(result.exitCode, 0,
      'non-release file must not require release check-runs; must exit 0');
    const allLines = result.lines.join('\n');
    assert.ok(
      allLines.includes('release surface not touched'),
      `must include "release surface not touched"; got:\n${allLines}`,
    );
  });

  test('D-PR7f: matchesReleaseSurface positives and negatives', () => {
    // Positives (must return true)
    assert.ok(matchesReleaseSurface('.github/actions/setup-wasm/action.yml'),
      '.github/actions/** pattern must match .github/actions/setup-wasm/action.yml');
    assert.ok(matchesReleaseSurface('crates/mds-napi/src/lib.rs'),
      'crates/mds-napi/** must match crates/mds-napi/src/lib.rs');
    assert.ok(matchesReleaseSurface('scripts/verify-napi-names.mjs'),
      'exact match must work for scripts/verify-napi-names.mjs');
    assert.ok(matchesReleaseSurface('.github/workflows/release.yml'),
      'exact match must work for .github/workflows/release.yml');
    assert.ok(matchesReleaseSurface('crates/mds-python/src/lib.rs'),
      'crates/mds-python/** must match crates/mds-python/src/lib.rs');

    // Negatives (must return false)
    assert.ok(!matchesReleaseSurface('.github/workflows/ci.yml'),
      '.github/workflows/ci.yml must NOT match (only release.yml is listed)');
    assert.ok(!matchesReleaseSurface('scripts/verify-versions.mjs'),
      'scripts/verify-versions.mjs must NOT match (only verify-napi-names.mjs is listed)');
    assert.ok(!matchesReleaseSurface('crates/mds-core/src/lib.rs'),
      'crates/mds-core/** must NOT match (not in RELEASE_SURFACE)');
  });

  test('D-PR7g: changedFiles undefined → exit 0 with skip notice (caller-path only, never an API failure)', () => {
    // evaluateChecks is an exported pure function; a caller holding only a SHA
    // cannot enumerate changed files, so `undefined` skips the check with a
    // notice. main() NEVER reaches this path: fetchChangedFiles fails closed on
    // an API error (see D-PR7h) rather than degrading to undefined, because a
    // transient 500 must not turn a release-surface PR into one that needs no
    // release check-runs (fail-open in a merge gate).
    const checkRuns = passingRunsWith([]);
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns,
      statuses: [],
      headSha: HEAD_113F472,
      changedFiles: undefined,
    });
    assert.equal(result.exitCode, 0, 'undefined changedFiles must not cause failure');
    const allLines = result.lines.join('\n');
    assert.ok(
      allLines.includes('release-surface presence check skipped') ||
      allLines.includes('changedFiles not provided'),
      `must include a skip notice; got:\n${allLines}`,
    );
  });

  // -------------------------------------------------------------------------
  // D-PR7h: the live path must FAIL CLOSED when the files endpoint errors.
  //
  // This is the fail-open that would otherwise sit under D-PR7: a 500 from
  // /pulls/N/files would let a PR that touches release.yml pass the verifier
  // without a single release check-run. Indeterminate is exit 2 — the same
  // contract fetchCheckRuns, fetchStatuses and fetchRequiredContexts hold.
  // -------------------------------------------------------------------------
  test('D-PR7h: PR-files API error → exit 2 (fail closed, never a silent skip)', () => {
    // Route the files URL to an error BEFORE the generic /pulls/ route, which
    // would otherwise answer it with PR metadata.
    const runner = stubRunner([
      ['/files', { __error: true, httpStatus: 500, stderr: 'server error' }],
      ['/pulls/', PR_OK],
      ['/protection', PROTECTION_OK],
      ['/check-runs', CHECKS_OK_WITH_HYGIENE],
      ['/status', { statuses: [], total_count: 0 }],
    ]);
    assert.equal(
      main(['1'], runner, OK_GH_VERSION), 2,
      'a files-endpoint error is indeterminate: it must exit 2, not skip the ' +
      'release-surface check and report PASS',
    );
  });

  test('D-PR7h: changed_files count mismatch → exit 2 (partial file list)', () => {
    // The PR declares 3 changed files but the endpoint returns 1. Evaluating
    // the release-surface question on a partial list could miss the one file
    // that touches the surface (non-vacuity, avoids PF-013).
    const runner = stubRunner([
      ['/files', [{ filename: 'README.md' }]],
      ['/pulls/', { ...PR_OK, changed_files: 3 }],
      ['/protection', PROTECTION_OK],
      ['/check-runs', CHECKS_OK_WITH_HYGIENE],
      ['/status', { statuses: [], total_count: 0 }],
    ]);
    assert.equal(main(['1'], runner, OK_GH_VERSION), 2,
      'a partial changed-files list must exit 2');
  });

  test('D-PR7h: live path requires the release runs when the files endpoint reports a surface file', () => {
    // End-to-end through main(): the files endpoint (not a hand-built
    // changedFiles array) drives the presence check. Without the release
    // check-runs this PR must FAIL — proving fetchChangedFiles is wired into
    // evaluateChecks and that D-PR7a-f are not testing a disconnected function.
    const runner = stubRunner([
      ['/files', [{ filename: 'crates/mds-napi/src/lib.rs' }]],
      ['/pulls/', { ...PR_OK, changed_files: 1 }],
      ['/protection', PROTECTION_OK],
      ['/check-runs', CHECKS_OK_WITH_HYGIENE],
      ['/status', { statuses: [], total_count: 0 }],
      ['/actions/runs', RUNS_NONE],
    ]);
    assert.equal(main(['1'], runner, OK_GH_VERSION), 1,
      'a release-surface file with no release check-runs must exit 1');
  });

  test('D-PR7h: live path passes when the release runs are present for a surface file', () => {
    // The complement of the case above: same PR, plus the three release runs.
    // Without this pair, exit 1 could be coming from anywhere.
    const withRelease = {
      ...CHECKS_OK_WITH_HYGIENE,
      check_runs: [
        ...CHECKS_OK_WITH_HYGIENE.check_runs,
        ...RELEASE_SURFACE_CONTEXTS.map(name => withSuite({
          name, status: 'completed', conclusion: 'success',
        })),
      ],
      total_count: CHECKS_OK_WITH_HYGIENE.total_count + RELEASE_SURFACE_CONTEXTS.length,
    };
    const runner = stubRunner([
      ['/files', [{ filename: 'crates/mds-napi/src/lib.rs' }]],
      ['/pulls/', { ...PR_OK, changed_files: 1 }],
      ['/protection', PROTECTION_OK],
      ['/check-runs', withRelease],
      ['/status', { statuses: [], total_count: 0 }],
      ['/actions/runs', RUNS_RELEASE],
    ]);
    assert.equal(main(['1'], runner, OK_GH_VERSION), 0,
      'a release-surface file WITH all release check-runs must exit 0');
  });

  // -------------------------------------------------------------------------
  // D-PR7j: duplicate names resolve ALL-MUST-PASS, not newest-wins.
  //
  // Same contract as Tier A (see the checksByName comment in the production
  // file): `filter=latest` de-duplicates within one check-suite, but two suites
  // can publish the same name, so keeping only the last entry would let a green
  // re-run mask a red sibling — a fail-open in a merge gate.
  // -------------------------------------------------------------------------
  test('D-PR7j: a failed and a succeeded run sharing a release-surface name → exit 1 (all must pass)', () => {
    const checkRuns = passingRunsWith([
      ...releaseSuccessRuns(),
      // A second run under a name that already has a success above, in the SAME release suite.
      withSuite({ name: 'Version gate', status: 'completed', conclusion: 'failure' }),
    ]);
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns,
      statuses: [],
      headSha: HEAD_113F472,
      changedFiles: ['.github/workflows/release.yml'],
      releaseSuiteIds: RELEASE_SUITES,
    });
    assert.equal(result.exitCode, 1,
      'a later success must not mask an earlier failure under the same name; must exit 1');
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('Version gate') && allLines.includes('failure'),
      `must name the failing run and its conclusion; got:\n${allLines}`);
  });

  // -------------------------------------------------------------------------
  // D-PR7i: matchesReleaseSurface prefix and exact-match boundaries.
  // A `dir/**` pattern must not match a sibling whose name merely starts with
  // the directory name, and an exact pattern must not prefix-match.
  // -------------------------------------------------------------------------
  test('D-PR7i: "/**" requires a trailing slash and exact patterns do not prefix-match', () => {
    assert.ok(!matchesReleaseSurface('.github/actions-foo/x.yml'),
      '.github/actions/** must not match the sibling directory .github/actions-foo/');
    assert.ok(!matchesReleaseSurface('crates/mds-napi-extra/src/lib.rs'),
      'crates/mds-napi/** must not match crates/mds-napi-extra/');
    assert.ok(!matchesReleaseSurface('scripts/verify-napi-names.mjs.bak'),
      'an exact pattern must not prefix-match scripts/verify-napi-names.mjs.bak');
    assert.ok(!matchesReleaseSurface('.github/workflows/release.yml.orig'),
      'an exact pattern must not prefix-match .github/workflows/release.yml.orig');
    // The directory itself, with no file under it, is not a changed file path
    // GitHub ever reports — but the prefix rule must still be strict about it.
    assert.ok(!matchesReleaseSurface('.github/actions'),
      '.github/actions/** must not match the bare directory name');
  });

});

// ---------------------------------------------------------------------------
// D-PR8: Suite-keyed skipped-publish allowance — new tests
// ---------------------------------------------------------------------------
describe('D-PR8: suite-keyed TIER_B_EXPECTED_SKIPPED allowance', () => {

  function basePassingRunsWith(extras) {
    return [
      ...loadCheckRuns('checks-main-113f472.json'),
      { ...SOURCE_HYGIENE_PASS },
      ...extras,
    ];
  }

  test('D-PR5g: allow-listed name skipped in NON-release suite (CI_SUITE_113F472) → exit 1; message names both suite ids (D-PR8)', () => {
    const runs = basePassingRunsWith([
      { name: 'Publish to npm', status: 'completed', conclusion: 'skipped', check_suite: { id: CI_SUITE_113F472 } },
    ]);
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns: runs,
      statuses: [],
      headSha: HEAD_113F472,
      releaseSuiteIds: RELEASE_SUITES,
    });
    assert.equal(result.exitCode, 1,
      'allow-listed name in non-release suite must fail (D-PR8)');
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('Publish to npm'), `must name the job; got:\n${allLines}`);
    assert.ok(
      allLines.includes(String(CI_SUITE_113F472)),
      `failure must name the check-run's suite id (${CI_SUITE_113F472}); got:\n${allLines}`,
    );
    assert.ok(
      allLines.includes(String(RELEASE_SUITE)) || allLines.includes('none'),
      `failure must mention the release suite list; got:\n${allLines}`,
    );
    // Positive control: same check-run but treat CI_SUITE as a release suite → PASS
    const result2 = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns: runs,
      statuses: [],
      headSha: HEAD_113F472,
      releaseSuiteIds: new Set([CI_SUITE_113F472]),
    });
    assert.equal(result2.exitCode, 0,
      'control: when the run\'s suite IS in releaseSuiteIds, must pass');
  });

  test('D-PR5g2: allow-listed name skipped but check_suite field absent → exit 1; "check_suite.id missing" (D-PR8)', () => {
    const runs = basePassingRunsWith([
      { name: 'Publish to npm', status: 'completed', conclusion: 'skipped' }, // no check_suite
    ]);
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns: runs,
      statuses: [],
      headSha: HEAD_113F472,
      releaseSuiteIds: RELEASE_SUITES,
    });
    assert.equal(result.exitCode, 1, 'missing check_suite must fail (D-PR8)');
    const allLines = result.lines.join('\n');
    assert.ok(
      allLines.includes('check_suite.id missing'),
      `failure must say "check_suite.id missing"; got:\n${allLines}`,
    );
    // Positive control: add check_suite in release suite → PASS
    const runs2 = basePassingRunsWith([
      withSuite({ name: 'Publish to npm', status: 'completed', conclusion: 'skipped' }),
    ]);
    const result2 = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns: runs2,
      statuses: [],
      headSha: HEAD_113F472,
      releaseSuiteIds: RELEASE_SUITES,
    });
    assert.equal(result2.exitCode, 0, 'control: check_suite present in release suite must pass');
  });

  test('D-PR5h: releaseSuiteIds undefined → all five skipped allow-listed names FAIL with DISABLED in message (D-PR8, fails closed)', () => {
    const skippedPublishRuns = [...TIER_B_EXPECTED_SKIPPED].map(name => ({
      name, status: 'completed', conclusion: 'skipped',
    }));
    const runs = basePassingRunsWith(skippedPublishRuns);
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns: runs,
      statuses: [],
      headSha: HEAD_113F472,
      // releaseSuiteIds: undefined — not provided
    });
    assert.equal(result.exitCode, 1,
      'undefined releaseSuiteIds must fail the five skipped allow-listed names');
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('DISABLED'),
      `failure must say DISABLED when releaseSuiteIds is undefined; got:\n${allLines}`);
    // Each of the five names must appear in a failure
    for (const name of TIER_B_EXPECTED_SKIPPED) {
      assert.ok(
        allLines.includes(name),
        `failure must name "${name}"; got:\n${allLines}`,
      );
    }
    // Positive control: with releaseSuiteIds supplied the same five skips (with suite) are allowed → PASS
    const skippedPublishRuns2 = [...TIER_B_EXPECTED_SKIPPED].map(name =>
      withSuite({ name, status: 'completed', conclusion: 'skipped' }),
    );
    const runs2 = basePassingRunsWith(skippedPublishRuns2);
    const result2 = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns: runs2,
      statuses: [],
      headSha: HEAD_113F472,
      releaseSuiteIds: RELEASE_SUITES,
    });
    assert.equal(result2.exitCode, 0,
      'control: with releaseSuiteIds supplied, five skips in release suite are allowed → PASS');
  });

  test('D-PR5h2: releaseSuiteIds undefined but no skipped allow-listed names and untouched surface → PASS; prints "not provided" line', () => {
    const runs = basePassingRunsWith([]);
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns: runs,
      statuses: [],
      headSha: HEAD_113F472,
      changedFiles: ['README.md'],
      // releaseSuiteIds: undefined — not provided
    });
    assert.equal(result.exitCode, 0,
      'undefined releaseSuiteIds with no skipped names and untouched surface must pass');
    const allLines = result.lines.join('\n');
    assert.ok(
      allLines.includes('not provided'),
      `output must print the "not provided" state; got:\n${allLines}`,
    );
    // Positive control: adding one skipped allow-listed run without releaseSuiteIds → FAIL with DISABLED
    const runs2 = basePassingRunsWith([
      { name: 'Publish to npm', status: 'completed', conclusion: 'skipped' },
    ]);
    const result2 = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns: runs2,
      statuses: [],
      headSha: HEAD_113F472,
      changedFiles: ['README.md'],
      // releaseSuiteIds: undefined — not provided
    });
    assert.equal(result2.exitCode, 1,
      'control: skipped allow-listed run with undefined releaseSuiteIds → FAIL with DISABLED');
    assert.ok(result2.lines.join('\n').includes('DISABLED'),
      'control must say DISABLED');
  });

  test('D-PR5i: live path, runs-API error → exit 2 (D-PR8, fail closed)', () => {
    const runner = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', PROTECTION_OK],
      ['/check-runs', CHECKS_OK_WITH_HYGIENE],
      ['/status', { statuses: [], total_count: 0 }],
      ['/actions/runs', { __error: true, httpStatus: 500, stderr: 'server error' }],
    ]);
    assert.equal(main(['1'], runner, OK_GH_VERSION), 2,
      'runs API error must exit 2 (D-PR8, fail closed)');
    // Positive control: valid runs response → not exit 2
    const runner2 = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', PROTECTION_OK],
      ['/check-runs', CHECKS_OK_WITH_HYGIENE],
      ['/status', { statuses: [], total_count: 0 }],
      ['/actions/runs', RUNS_RELEASE],
    ]);
    assert.notEqual(main(['1'], runner2, OK_GH_VERSION), 2,
      'control: valid runs response must not exit 2');
  });

});

// ---------------------------------------------------------------------------
// D-PR5i2: fetchWorkflowRuns unit tests (bounded loop, total_count guard,
// field projection). Mirrors architecture-08 for fetchCheckRuns.
// ---------------------------------------------------------------------------
describe('D-PR5i2: fetchWorkflowRuns — bounded loop, total_count guard, projection', () => {

  test('page cap (≤ 5 requests) then ok:false/exitCode 2 (D-PR8)', () => {
    let calls = 0;
    const fullPage = {
      total_count: 100_000,
      workflow_runs: Array.from({ length: 100 }, (_, i) => ({
        id: i, path: '.github/workflows/ci.yml', event: 'pull_request',
        check_suite_id: i + 1, conclusion: 'success',
      })),
    };
    const runner = (_args) => { calls++; return fullPage; };
    const result = fetchWorkflowRuns('abc123sha', runner);
    assert.ok(!result.ok, 'must return ok:false when pagination exceeds MAX_RUNS_PAGES');
    assert.equal(result.exitCode, 2, 'pagination cap must produce exit 2 (D-PR8)');
    assert.ok(calls <= 5, `must not exceed 5 page requests; made ${calls} (pinned to MAX_RUNS_PAGES=5)`);
    assert.ok(calls >= 1, 'must have made at least one request');
    // Positive control: 5 pages whose total_count is satisfied → ok:true
    let calls2 = 0;
    const fullPage2 = {
      total_count: 500,
      workflow_runs: Array.from({ length: 100 }, (_, i) => ({
        id: i, path: '.github/workflows/ci.yml', event: 'pull_request',
        check_suite_id: i + 1, conclusion: 'success',
      })),
    };
    const runner2 = (_args) => { calls2++; return fullPage2; };
    const result2 = fetchWorkflowRuns('abc123sha', runner2);
    assert.ok(result2.ok, `control: 5 pages with satisfied total_count must return ok:true; got: ${JSON.stringify(result2)}`);
    assert.equal(calls2, 5, `control: must have made exactly 5 requests; made ${calls2}`);
  });

  test('total_count mismatch → ok:false/exitCode 2 (D-PR8)', () => {
    const runner = (_args) => ({
      total_count: 50,
      workflow_runs: [{ id: 1, path: '.github/workflows/release.yml', event: 'pull_request', check_suite_id: 123, conclusion: 'success' }],
    });
    const result = fetchWorkflowRuns('abc123sha', runner);
    assert.ok(!result.ok, 'total_count mismatch must return ok:false');
    assert.equal(result.exitCode, 2, 'total_count mismatch must produce exit 2 (D-PR8)');
    assert.ok(
      result.message.includes('total_count') || result.message.includes('D-PR8'),
      `message must mention total_count or D-PR8; got: ${result.message}`,
    );
    // Positive control: matching total_count → ok:true
    const runner2 = (_args) => ({
      total_count: 1,
      workflow_runs: [{ id: 1, path: '.github/workflows/release.yml', event: 'pull_request', check_suite_id: 123, conclusion: 'success' }],
    });
    const result2 = fetchWorkflowRuns('abc123sha', runner2);
    assert.ok(result2.ok, `control: matching total_count must return ok:true; got: ${JSON.stringify(result2)}`);
  });

  test('single complete page → ok:true with ONLY the projected fields', () => {
    const rawRun = {
      id: 123,
      path: '.github/workflows/release.yml',
      event: 'pull_request',
      check_suite_id: RELEASE_SUITE,
      conclusion: 'success',
      // Extra fields that must NOT appear in the projected output:
      name: 'Release', html_url: 'https://...', actor: { login: 'user' },
    };
    const runner = (_args) => ({ total_count: 1, workflow_runs: [rawRun] });
    const result = fetchWorkflowRuns('abc123sha', runner);
    assert.ok(result.ok, `must return ok:true for a complete single-page result; got: ${JSON.stringify(result)}`);
    assert.equal(result.runs.length, 1, 'must return one run');
    const projected = result.runs[0];
    assert.equal(projected.id, 123, 'id must be projected');
    assert.equal(projected.path, '.github/workflows/release.yml', 'path must be projected');
    assert.equal(projected.event, 'pull_request', 'event must be projected');
    assert.equal(projected.check_suite_id, RELEASE_SUITE, 'check_suite_id must be projected');
    assert.equal(projected.conclusion, 'success', 'conclusion must be projected');
    assert.ok(!('name' in projected), 'extra field "name" must be excluded');
    assert.ok(!('html_url' in projected), 'extra field "html_url" must be excluded');
    assert.ok(!('actor' in projected), 'extra field "actor" must be excluded');
  });

  test('API error → ok:false/exitCode 2 (D-PR8)', () => {
    const runner = (_args) => ({ __error: true, httpStatus: 500, stderr: 'server error' });
    const result = fetchWorkflowRuns('abc123sha', runner);
    assert.ok(!result.ok, 'API error must return ok:false');
    assert.equal(result.exitCode, 2, 'API error must produce exit 2');
  });

});

// ---------------------------------------------------------------------------
// D-PR5j: live path with RUNS_RELEASE → 6 API calls, exit 0; control with
// ci.yml path → exit 1 (D-PR8)
// ---------------------------------------------------------------------------
describe('D-PR5j: live path with workflow runs routing (D-PR8)', () => {

  test('D-PR5j: RUNS_RELEASE → exit 0 and 6 URLs; control: run path = ci.yml → releaseSuiteIds empty → exit 1', () => {
    // Add a skipped publish run so that the "no release suite → fail" control fires.
    const withSkipped = {
      ...CHECKS_OK_WITH_HYGIENE,
      check_runs: [
        ...CHECKS_OK_WITH_HYGIENE.check_runs,
        { name: 'Publish to npm', status: 'completed', conclusion: 'skipped', check_suite: { id: RELEASE_SUITE } },
      ],
      total_count: CHECKS_OK_WITH_HYGIENE.total_count + 1,
    };
    const calls = [];
    const runner = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', PROTECTION_OK],
      ['/check-runs', withSkipped],
      ['/status', { statuses: [], total_count: 0 }],
      ['/actions/runs', RUNS_RELEASE],
    ], calls);
    assert.equal(main(['1'], runner, OK_GH_VERSION), 0,
      'RUNS_RELEASE must allow the skipped publish run and exit 0');
    assert.equal(calls.length, 6, `expected 6 API calls (pr, protection, checks, status, files, runs); got ${calls.length}`);
    const runsCall = calls.find(u => u.includes('/actions/runs'));
    assert.ok(runsCall, 'must have made a /actions/runs call');
    assert.ok(runsCall.includes('head_sha='), 'runs call must include head_sha=');
    assert.ok(runsCall.includes('per_page=100'), 'runs call must include per_page=100');

    // Control: same check-runs but run path = ci.yml → empty releaseSuiteIds → skipped run fails
    const CI_RUNS = {
      total_count: 1,
      workflow_runs: [{ id: 99, path: '.github/workflows/ci.yml', event: 'pull_request', check_suite_id: CI_SUITE_113F472, conclusion: 'success' }],
    };
    const runner2 = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', PROTECTION_OK],
      ['/check-runs', withSkipped],
      ['/status', { statuses: [], total_count: 0 }],
      ['/actions/runs', CI_RUNS],
    ]);
    assert.equal(main(['1'], runner2, OK_GH_VERSION), 1,
      'control: ci.yml path → releaseSuiteIds empty → skipped publish run fails → exit 1');
  });

});

// ---------------------------------------------------------------------------
// D-PR5k: two release suites on one head — each run attributed on its own
// ---------------------------------------------------------------------------
describe('D-PR5k: multiple release suites on one head (dispatch + pull_request)', () => {

  test('D-PR5k: two release suites → each skip attributed to its own suite; PASS; control: failure in second suite → exit 1', () => {
    const SUITE_A = RELEASE_SUITE;
    const SUITE_B = 92246852351; // PR #365 dispatch-only release suite (event-agnostic)
    const twoSuites = new Set([SUITE_A, SUITE_B]);

    function basePassingRunsWith(extras) {
      return [
        ...loadCheckRuns('checks-main-113f472.json'),
        { ...SOURCE_HYGIENE_PASS },
        ...extras,
      ];
    }

    // Four names in SUITE_A, TestPyPI in SUITE_B
    const runs = basePassingRunsWith([
      ...['Publish to crates.io', 'Publish to npm', 'Publish to PyPI', 'GitHub Release'].map(name =>
        withSuite({ name, status: 'completed', conclusion: 'skipped' }, SUITE_A),
      ),
      withSuite({ name: 'Publish to TestPyPI (rehearsal)', status: 'completed', conclusion: 'skipped' }, SUITE_B),
    ]);
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns: runs,
      statuses: [],
      headSha: HEAD_113F472,
      releaseSuiteIds: twoSuites,
    });
    assert.equal(result.exitCode, 0,
      'both release suites must allow their skipped runs → pass');

    // Control: flip TestPyPI in SUITE_B to failure → exit 1 (all-must-pass, D-PR7j)
    const runs2 = basePassingRunsWith([
      ...['Publish to crates.io', 'Publish to npm', 'Publish to PyPI', 'GitHub Release'].map(name =>
        withSuite({ name, status: 'completed', conclusion: 'skipped' }, SUITE_A),
      ),
      withSuite({ name: 'Publish to TestPyPI (rehearsal)', status: 'completed', conclusion: 'failure' }, SUITE_B),
    ]);
    const result2 = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns: runs2,
      statuses: [],
      headSha: HEAD_113F472,
      releaseSuiteIds: twoSuites,
    });
    assert.equal(result2.exitCode, 1,
      'control: failure in second release suite must exit 1 (all-must-pass)');
    const allLines2 = result2.lines.join('\n');
    assert.ok(allLines2.includes('Publish to TestPyPI'), 'must name the failing job');
  });

});

// ---------------------------------------------------------------------------
// D-PR7k, D-PR7l: release-surface attribution via releaseSuiteIds (D-PR8)
// ---------------------------------------------------------------------------
describe('D-PR7k, D-PR7l: D-PR7 attribution keyed on release suite identity', () => {

  function passingRunsWith(extras) {
    return [
      ...loadCheckRuns('checks-main-113f472.json'),
      { ...SOURCE_HYGIENE_PASS },
      ...extras,
    ];
  }

  test('D-PR7k: contexts in ci.yml suite → exit 1; message cites "absent from every .github/workflows/release.yml" and N same-name runs ignored', () => {
    const releaseRunsInCiSuite = RELEASE_SURFACE_CONTEXTS.map(name => ({
      name, status: 'completed', conclusion: 'success', check_suite: { id: CI_SUITE_113F472 },
    }));
    const checkRuns = passingRunsWith(releaseRunsInCiSuite);
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns,
      statuses: [],
      headSha: HEAD_113F472,
      changedFiles: ['.github/workflows/release.yml'],
      releaseSuiteIds: RELEASE_SUITES,
    });
    assert.equal(result.exitCode, 1, 'contexts only in ci.yml suite must fail (D-PR8)');
    const allLines = result.lines.join('\n');
    assert.ok(
      allLines.includes('absent from every .github/workflows/release.yml check-suite on this head'),
      `must say "absent from every .github/workflows/release.yml check-suite on this head"; got:\n${allLines}`,
    );
    assert.ok(
      allLines.includes('same-name run(s) in other suites ignored'),
      `must mention "same-name run(s) in other suites ignored"; got:\n${allLines}`,
    );
    assert.ok(
      allLines.includes(`release suites: ${RELEASE_SUITE}`),
      `must list release suite id ${RELEASE_SUITE}; got:\n${allLines}`,
    );
    // Positive control: same runs but CI_SUITE_113F472 IS a release suite → PASS
    const result2 = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns,
      statuses: [],
      headSha: HEAD_113F472,
      changedFiles: ['.github/workflows/release.yml'],
      releaseSuiteIds: new Set([CI_SUITE_113F472]),
    });
    assert.equal(result2.exitCode, 0,
      'control: when ci.yml suite is treated as a release suite, must pass');
  });

  test('D-PR7l: releaseSuiteIds undefined + surface touched → 3 D-PR7 failures ("not provided")', () => {
    const releaseRuns = RELEASE_SURFACE_CONTEXTS.map(name => ({
      name, status: 'completed', conclusion: 'success',
    }));
    const checkRuns = passingRunsWith(releaseRuns);
    const result = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns,
      statuses: [],
      headSha: HEAD_113F472,
      changedFiles: ['.github/workflows/release.yml'],
      // releaseSuiteIds: undefined — not provided
    });
    assert.equal(result.exitCode, 1,
      'undefined releaseSuiteIds + surface touched must fail (D-PR8, fails closed)');
    const allLines = result.lines.join('\n');
    assert.ok(
      allLines.includes('not provided'),
      `must say "not provided" in D-PR7 failure; got:\n${allLines}`,
    );
    // All 3 RELEASE_SURFACE_CONTEXTS should fail
    for (const ctx of RELEASE_SURFACE_CONTEXTS) {
      assert.ok(allLines.includes(ctx), `must name context "${ctx}"; got:\n${allLines}`);
    }
    // Positive control: supplying RELEASE_SUITES with attributed successful runs → 0 D-PR7 failures
    const releaseRuns2 = RELEASE_SURFACE_CONTEXTS.map(name => ({
      name, status: 'completed', conclusion: 'success', check_suite: { id: RELEASE_SUITE },
    }));
    const checkRuns2 = passingRunsWith(releaseRuns2);
    const result2 = evaluateChecks({
      requiredContexts: REQUIRED,
      checkRuns: checkRuns2,
      statuses: [],
      headSha: HEAD_113F472,
      changedFiles: ['.github/workflows/release.yml'],
      releaseSuiteIds: RELEASE_SUITES,
    });
    assert.equal(result2.exitCode, 0,
      'control: RELEASE_SUITES with properly attributed runs → 0 D-PR7 failures → PASS');
  });

});

// ---------------------------------------------------------------------------
// releaseSuiteIdsFrom unit tests (D-PR8)
// ---------------------------------------------------------------------------
describe('releaseSuiteIdsFrom: builds Set<number> of check_suite_ids for release.yml runs', () => {

  test('returns Set<number> with release.yml suite ids only; non-release paths excluded; non-integer ids excluded', () => {
    const runs = [
      { id: 1, path: '.github/workflows/release.yml', event: 'pull_request', check_suite_id: 123, conclusion: 'success' },
      { id: 2, path: '.github/workflows/ci.yml', event: 'pull_request', check_suite_id: 456, conclusion: 'success' },
      { id: 3, path: '.github/workflows/release.yml', event: 'workflow_dispatch', check_suite_id: 789, conclusion: 'success' },
      { id: 4, path: '.github/workflows/release.yml', event: 'push', check_suite_id: 'not-a-number', conclusion: 'success' },
    ];
    const result = releaseSuiteIdsFrom(runs);
    assert.ok(result instanceof Set, 'must return a Set');
    assert.ok(result.has(123), 'must include pull_request release suite 123');
    assert.ok(result.has(789), 'must include workflow_dispatch release suite 789 (any event counts)');
    assert.ok(!result.has(456), 'must exclude ci.yml suite 456');
    assert.ok(!result.has('not-a-number'), 'must exclude non-integer ids');
    assert.equal(result.size, 2, 'must contain exactly 2 entries');
  });

  test('empty runs array → empty Set (no release suites on this head)', () => {
    const result = releaseSuiteIdsFrom([]);
    assert.ok(result instanceof Set, 'must return a Set');
    assert.equal(result.size, 0, 'empty runs must yield empty Set');
  });

  test('drift guard: RELEASE_SURFACE includes RELEASE_WORKFLOW_PATH (ADR-013 amendment)', () => {
    // releaseSuiteIdsFrom uses RELEASE_WORKFLOW_PATH to filter runs.
    // RELEASE_SURFACE must include that path so the release-surface presence check
    // and the suite attribution use the same path string (ADR-013).
    assert.ok(
      RELEASE_SURFACE.includes(RELEASE_WORKFLOW_PATH),
      `RELEASE_WORKFLOW_PATH "${RELEASE_WORKFLOW_PATH}" must be in RELEASE_SURFACE: ${RELEASE_SURFACE.join(', ')}`,
    );
  });

});

// ---------------------------------------------------------------------------
// Current fixtures (2026-09): live-shaped evaluation with the PR #366 fixture
// ---------------------------------------------------------------------------
describe('current fixtures (2026-09): live-shaped evaluation', () => {

  const CHECKS_PR366 = JSON.parse(readFileSync(join(FIXTURES, 'checks-pr366-e02bcf2.json'), 'utf8'));
  const RUNS_PR366 = JSON.parse(readFileSync(join(FIXTURES, 'runs-pr366-e02bcf2.json'), 'utf8'));
  const PROTECTION_2026_09 = JSON.parse(readFileSync(join(FIXTURES, 'protection-main-2026-09.json'), 'utf8'));

  const REQUIRED_2026_09 = PROTECTION_2026_09.required_status_checks.contexts;
  const CHECK_RUNS_PR366 = CHECKS_PR366.check_runs;
  const PR366_HEAD = 'e02bcf280dc50bb8df032744aa2a2520c02865ee';
  // B1 files changed in PR #366:
  const PR366_FILES = [
    { filename: '.github/workflows/release.yml' },
    { filename: 'CHANGELOG.md' },
    { filename: 'RELEASING.md' },
    { filename: 'scripts/__test__/release-auth-probe.spec.mjs' },
    { filename: 'scripts/__test__/verify-pr-checks.spec.mjs' },
    { filename: 'scripts/verify-pr-checks.mjs' },
  ];
  const CI_SUITE_PR366 = 92290758550; // ci.yml suite for PR #366

  // Build releaseSuiteIds from the runs fixture
  function pr366ReleaseSuiteIds() {
    return releaseSuiteIdsFrom(RUNS_PR366.workflow_runs.map(r => ({
      id: r.id, path: r.path, event: r.event, check_suite_id: r.check_suite_id, conclusion: r.conclusion,
    })));
  }

  test('fixture shapes: 15 required contexts; 44 check-runs across 4 suites; app.slug ∈ {github-actions, github-advanced-security}; 3 workflow runs with exactly 1 release.yml run (check_suite_id = RELEASE_SUITE)', () => {
    assert.equal(REQUIRED_2026_09.length, 15,
      'protection-main-2026-09 must have 15 required contexts');
    assert.equal(CHECK_RUNS_PR366.length, 44,
      'checks-pr366-e02bcf2 must have 44 check-runs');
    const suiteIds = new Set(CHECK_RUNS_PR366.map(cr => cr.check_suite.id));
    assert.equal(suiteIds.size, 4, 'must have 4 distinct check-suites');
    const appSlugs = new Set(CHECK_RUNS_PR366.map(cr => cr.app.slug));
    for (const slug of appSlugs) {
      assert.ok(
        ['github-actions', 'github-advanced-security'].includes(slug),
        `unexpected app.slug "${slug}" — only github-actions and github-advanced-security expected`,
      );
    }
    assert.equal(RUNS_PR366.total_count, 3, 'runs fixture must declare total_count=3');
    const releaseRuns = RUNS_PR366.workflow_runs.filter(r => r.path === RELEASE_WORKFLOW_PATH);
    assert.equal(releaseRuns.length, 1, 'exactly one release.yml workflow run expected');
    assert.equal(releaseRuns[0].check_suite_id, RELEASE_SUITE,
      `release run must have check_suite_id ${RELEASE_SUITE}`);
  });

  test('CF-1: pure evaluateChecks with PR #366 fixture → PASS; "release surface touched"; five allowed (D-PR8) lines with suite id; control: mutate Version gate → exit 1', () => {
    const releaseSuiteIds = pr366ReleaseSuiteIds();

    const result = evaluateChecks({
      requiredContexts: REQUIRED_2026_09,
      checkRuns: CHECK_RUNS_PR366,
      statuses: [],
      headSha: PR366_HEAD,
      changedFiles: ['.github/workflows/release.yml'],
      releaseSuiteIds,
    });
    assert.equal(result.exitCode, 0, `must PASS; lines:\n${result.lines.join('\n')}`);
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('release surface touched'), 'must say "release surface touched"');

    // Five allowed lines (D-PR8): each must say "allowed" and name the release suite id
    for (const name of TIER_B_EXPECTED_SKIPPED) {
      const line = result.lines.find(l => l.includes(name));
      assert.ok(line, `output must include a line for "${name}"; got:\n${allLines}`);
      assert.ok(line.includes('allowed'), `"${name}" allowed line must say "allowed"; got: ${line}`);
      assert.ok(
        line.includes(String(RELEASE_SUITE)),
        `"${name}" allowed line must name suite id ${RELEASE_SUITE}; got: ${line}`,
      );
    }

    // Control: mutate "Version gate" to failure → exit 1
    const withFailedGate = CHECK_RUNS_PR366.map(cr =>
      (cr.name === 'Version gate' && cr.check_suite.id === RELEASE_SUITE)
        ? { ...cr, conclusion: 'failure' }
        : cr,
    );
    const result2 = evaluateChecks({
      requiredContexts: REQUIRED_2026_09,
      checkRuns: withFailedGate,
      statuses: [],
      headSha: PR366_HEAD,
      changedFiles: ['.github/workflows/release.yml'],
      releaseSuiteIds,
    });
    assert.equal(result2.exitCode, 1, 'control: failed Version gate must exit 1');
    assert.ok(result2.lines.join('\n').includes('Version gate'), 'must name Version gate in failure');
  });

  test('CF-2: "Publish to npm" re-suited to ci.yml suite → exit 1 (D-PR8: skipped allowance requires release suite)', () => {
    const releaseSuiteIds = pr366ReleaseSuiteIds();
    // Move "Publish to npm" from release suite to ci.yml suite
    const resuitedChecks = CHECK_RUNS_PR366.map(cr =>
      (cr.name === 'Publish to npm' && cr.check_suite.id === RELEASE_SUITE)
        ? { ...cr, check_suite: { id: CI_SUITE_PR366 } }
        : cr,
    );
    const result = evaluateChecks({
      requiredContexts: REQUIRED_2026_09,
      checkRuns: resuitedChecks,
      statuses: [],
      headSha: PR366_HEAD,
      changedFiles: ['.github/workflows/release.yml'],
      releaseSuiteIds,
    });
    assert.equal(result.exitCode, 1,
      '"Publish to npm" re-suited to ci.yml must fail the skipped-allowance check (D-PR8)');
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('Publish to npm'), 'must name the failing job');
    assert.ok(
      allLines.includes(String(CI_SUITE_PR366)) || allLines.includes(String(RELEASE_SUITE)),
      'must mention a suite id in the failure',
    );
  });

  test('CF-3: live main() over fixtures (stubbed runner) → exit 0; 6 API calls; merge line contains "366"; control: run path = ci.yml → exit 1', () => {
    const PR366_DATA = { head: { sha: PR366_HEAD }, base: { ref: 'main' }, changed_files: 6 };
    const calls = [];
    const runner = stubRunner([
      ['/files', PR366_FILES],
      ['/pulls/', PR366_DATA],
      ['/protection', PROTECTION_2026_09],
      ['/check-runs', CHECKS_PR366],
      ['/status', { statuses: [], total_count: 0 }],
      ['/actions/runs', RUNS_PR366],
    ], calls);

    const lines = [];
    const origLog = console.log;
    console.log = (...args) => lines.push(args.join(' '));
    let exitCode;
    try {
      exitCode = main(['366'], runner, OK_GH_VERSION);
    } finally {
      console.log = origLog;
    }
    assert.equal(exitCode, 0, `must exit 0; output:\n${lines.join('\n')}`);
    assert.equal(calls.length, 6, `expected 6 API calls; got ${calls.length}: ${calls.join(', ')}`);
    const mergeLine = lines.find(l => l.includes('gh pr merge'));
    assert.ok(mergeLine, `output must include a merge command; got:\n${lines.join('\n')}`);
    assert.ok(mergeLine.includes('366'), `merge command must contain PR number 366; got: ${mergeLine}`);

    // Control: rewrite the release run's path to ci.yml → releaseSuiteIds empty → 5+3 failures → exit 1
    const RUNS_NO_RELEASE_PATH = {
      ...RUNS_PR366,
      workflow_runs: RUNS_PR366.workflow_runs.map(r =>
        r.path === RELEASE_WORKFLOW_PATH ? { ...r, path: '.github/workflows/ci.yml' } : r,
      ),
    };
    const runner2 = stubRunner([
      ['/files', PR366_FILES],
      ['/pulls/', PR366_DATA],
      ['/protection', PROTECTION_2026_09],
      ['/check-runs', CHECKS_PR366],
      ['/status', { statuses: [], total_count: 0 }],
      ['/actions/runs', RUNS_NO_RELEASE_PATH],
    ]);
    assert.equal(main(['366'], runner2, OK_GH_VERSION), 1,
      'control: no release.yml run in the runs fixture → exit 1');
  });

  test('CF-4: release run removed from runs fixture → Tier B (5 skipped) + D-PR7 (3 contexts) all FAIL', () => {
    const RUNS_STRIPPED = {
      total_count: 2,
      workflow_runs: RUNS_PR366.workflow_runs.filter(r => r.path !== RELEASE_WORKFLOW_PATH),
    };
    const releaseSuiteIds = releaseSuiteIdsFrom(RUNS_STRIPPED.workflow_runs.map(r => ({
      id: r.id, path: r.path, event: r.event, check_suite_id: r.check_suite_id, conclusion: r.conclusion,
    })));
    const result = evaluateChecks({
      requiredContexts: REQUIRED_2026_09,
      checkRuns: CHECK_RUNS_PR366,
      statuses: [],
      headSha: PR366_HEAD,
      changedFiles: ['.github/workflows/release.yml'],
      releaseSuiteIds,
    });
    assert.equal(result.exitCode, 1, 'no release suite → must fail');
    // 5 Tier B failures + 3 D-PR7 failures = 8 content failures + 1 summary line = 9 ✖ lines
    const failureLines = result.lines.filter(l => l.startsWith('✖'));
    assert.ok(
      failureLines.length >= 9,
      `expected ≥9 failure lines (5 Tier B + 3 D-PR7 + summary); got ${failureLines.length}:\n${result.lines.join('\n')}`,
    );
  });

  test('CF-5: with 2026-09 protection, EXPECTED_CONTEXTS are now Tier A — no double-report; Tier A catches planted Source hygiene failure', () => {
    const releaseSuiteIds = pr366ReleaseSuiteIds();

    // PASS: all 15 required contexts + surface contexts → no double-report
    const result = evaluateChecks({
      requiredContexts: REQUIRED_2026_09,
      checkRuns: CHECK_RUNS_PR366,
      statuses: [],
      headSha: PR366_HEAD,
      changedFiles: ['.github/workflows/release.yml'],
      releaseSuiteIds,
    });
    assert.equal(result.exitCode, 0, `must PASS; lines:\n${result.lines.join('\n')}`);
    // Source hygiene is now Tier A (in the 2026-09 required set); must NOT appear in Tier A+
    const tierAplusLines = result.lines.filter(l => l.includes('Tier A+') && l.includes('Source hygiene'));
    assert.equal(tierAplusLines.length, 0,
      'Source hygiene must not appear in Tier A+ — it is now in Tier A (2026-09 protection)');

    // Tier A catches planted Source hygiene failure
    const withFailedHygiene = CHECK_RUNS_PR366.map(cr =>
      cr.name === 'Source hygiene' ? { ...cr, conclusion: 'failure' } : cr,
    );
    const result2 = evaluateChecks({
      requiredContexts: REQUIRED_2026_09,
      checkRuns: withFailedHygiene,
      statuses: [],
      headSha: PR366_HEAD,
      changedFiles: ['.github/workflows/release.yml'],
      releaseSuiteIds,
    });
    assert.equal(result2.exitCode, 1, 'planted Source hygiene failure must fail');
    const allLines2 = result2.lines.join('\n');
    assert.ok(allLines2.includes('Source hygiene'), 'must name Source hygiene');
    assert.ok(allLines2.includes('Tier A'), 'must report as Tier A failure (not Tier A+)');
  });

});

// Code of Conduct tests (AC-1, AC-2) live in code-of-conduct.spec.mjs —
// split in commit 2e9482f to follow the one-spec-per-module convention.
