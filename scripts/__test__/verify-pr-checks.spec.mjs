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
  EXPECTED_CONTEXTS,
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
assert.equal(REQUIRED.length, 6, 'fixture must have 6 required contexts');

const HEAD_113F472 = '113f472684d6ee7e398d54c1aadc22b2ad747ae1';
const HEAD_F168944 = 'f168944'; // PR #239
const HEAD_E9DACE1 = 'e9dace1'; // PR #240

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

  test('EXPECTED_CONTEXTS is [Source hygiene]', () => {
    assert.deepEqual(EXPECTED_CONTEXTS, ['Source hygiene'],
      'EXPECTED_CONTEXTS must list exactly "Source hygiene"');
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

  test('PASS → exit 0 with --match-head-commit <sha> in output', () => {
    const runs = [...loadCheckRuns('checks-main-113f472.json'), SOURCE_HYGIENE_PASS];
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: runs, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 0);
    assert.ok(result.mergeCommand, 'PASS must produce a mergeCommand');
    // D-PR5: merge command must include --match-head-commit <headSha>
    assert.ok(result.mergeCommand.includes('--match-head-commit'), 'merge command must include --match-head-commit');
    assert.ok(result.mergeCommand.includes(HEAD_113F472), 'merge command must include the verified SHA');
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

  test('AC-30: the live path issues at most page-bound + 3 API calls', () => {
    const calls = [];
    const runner = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', PROTECTION_OK],
      ['/check-runs', CHECKS_OK_WITH_HYGIENE],
      ['/status', { statuses: [], total_count: 0 }],
    ], calls);
    assert.equal(main(['1'], runner, OK_GH_VERSION), 0);
    assert.equal(calls.length, 4, `expected 4 API calls (pr, protection, checks, status); got ${calls.length}`);
    const checkCall = calls.find(u => u.includes('/check-runs'));
    assert.ok(checkCall.includes('filter=latest'), 'filter=latest must be pinned explicitly (D-PR4a)');
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
    ]);
    assert.equal(main(['1'], runner, OK_GH_VERSION), 0,
      'missing total_count must not cause a false exit 2');
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
// Tier B FAILs on:  failure, cancelled, timed_out, action_required, stale
//                   queued, in_progress (non-completed = indeterminate → FAIL)
// Tier B advises on: skipped, neutral (reported but not fatal)
//
// Coverage requirement (from review finding): explicit tests for each category
// so `grep -c "Tier B"` on this file is non-zero for every shape, and the
// positive control (advisory case → PASS) confirms the suite is not
// unconditionally failing.
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

  test('skipped (completed) → advisory only, PASS (Tier B does not fatal-fail skipped)', () => {
    // skipped/neutral are ADVISORY in Tier B — the run completed, just not with work.
    // This positive control proves the describe block is not unconditionally failing.
    const runs = baseRunsWith({ name: 'Some background job', status: 'completed', conclusion: 'skipped' });
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: runs, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 0, 'Tier B: skipped is advisory — must not block PASS');
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('skipped'), `advisory line must name the conclusion; got: ${allLines}`);
  });

  test('neutral (completed) → advisory only, PASS (Tier B does not fatal-fail neutral)', () => {
    const runs = baseRunsWith({ name: 'Some background job', status: 'completed', conclusion: 'neutral' });
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: runs, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 0, 'Tier B: neutral is advisory — must not block PASS');
    const allLines = result.lines.join('\n');
    assert.ok(allLines.includes('neutral'), `advisory line must name the conclusion; got: ${allLines}`);
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

// Code of Conduct tests (AC-1, AC-2) live in code-of-conduct.spec.mjs —
// split in commit 2e9482f to follow the one-spec-per-module convention.
