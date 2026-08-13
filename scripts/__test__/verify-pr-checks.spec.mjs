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
import { readFileSync, statSync, mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

import { evaluateChecks, main, fetchRequiredContexts } from '../verify-pr-checks.mjs';

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

// ---------------------------------------------------------------------------
// AC-22: Historical fixtures reproduce correctly
// ---------------------------------------------------------------------------
describe('AC-21 AC-22: historical fixture evaluation', () => {

  test('113f472 (main baseline, 18 check-runs, all success) → PASS (exit 0)', () => {
    const checkRuns = loadCheckRuns('checks-main-113f472.json');
    assert.equal(checkRuns.length, 18, 'fixture must have 18 check-runs');
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

  test('17 of 18 check-runs (MSRV deleted) → FAIL naming MSRV', () => {
    // Synthesize by removing the MSRV check-run from the 113f472 fixture.
    // This is the case `gh pr checks --required` exits 0 on (all present checks are green)
    // but the tool catches: a required context is absent.
    const allRuns = loadCheckRuns('checks-main-113f472.json');
    const msrvName = 'MSRV (Rust 1.88)';
    const withoutMsrv = allRuns.filter(cr => cr.name !== msrvName);
    assert.equal(withoutMsrv.length, 17, 'should have 17 runs after removing MSRV');

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

  // Build a passing baseline from the 113f472 fixture, then mutate one required check
  function buildPassingRuns() {
    return loadCheckRuns('checks-main-113f472.json').map(cr => ({ ...cr }));
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

  test('control: all-success baseline still exits 0 (suite is not failing unconditionally)', () => {
    const runs = buildPassingRuns();
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: runs, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 0, 'all-success baseline must pass');
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
    const runs = loadCheckRuns('checks-main-113f472.json');
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
    const runs = loadCheckRuns('checks-main-113f472.json');
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
      checkRuns: loadCheckRuns('checks-main-113f472.json'),
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
 * `{ __error: true, status }` shape mirroring defaultGhRunner's failure return.
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
    return { __error: true, status: 404, stderr: `no stub route for ${url}` };
  };
}

const PR_OK = { head: { sha: HEAD_113F472 }, base: { ref: 'main' } };
const PROTECTION_OK = JSON.parse(readFileSync(join(FIXTURES, 'protection-main.json'), 'utf8'));
const CHECKS_OK = JSON.parse(readFileSync(join(FIXTURES, 'checks-main-113f472.json'), 'utf8'));

describe('AC-26 AC-28 AC-29: live path exit codes (injected runner)', () => {

  test('happy path → exit 0', () => {
    const runner = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', PROTECTION_OK],
      ['/check-runs', CHECKS_OK],
      ['/status', { statuses: [] }],
    ]);
    assert.equal(main(['1'], runner, OK_GH_VERSION), 0);
  });

  test('AC-29: unprotected base (404 on protection) → exit 2, never 0', () => {
    const runner = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', { __error: true, status: 404, stderr: 'Not Found' }],
      ['/check-runs', CHECKS_OK],
      ['/status', { statuses: [] }],
    ]);
    assert.equal(main(['1'], runner, OK_GH_VERSION), 2);
  });

  test('AC-29: --required-from branch also unprotected → exit 2', () => {
    const runner = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', { __error: true, status: 404, stderr: 'Not Found' }],
    ]);
    assert.equal(main(['1', '--required-from', 'nope'], runner, OK_GH_VERSION), 2);
  });

  test('AC-26: protection unreadable (403) → exit 2', () => {
    const runner = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', { __error: true, status: 403, stderr: 'Forbidden' }],
    ]);
    assert.equal(main(['1'], runner, OK_GH_VERSION), 2);
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
      ['/check-runs', CHECKS_OK],
      ['/status', { statuses: [] }],
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
      ['/check-runs', CHECKS_OK],
      ['/status', { statuses: [] }],
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
      ['/status', { statuses: [] }],
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
      ['/status', { statuses: [] }],
    ]);
    assert.equal(main(['1'], runner, OK_GH_VERSION), 2);
  });

  test('AC-30: the live path issues at most page-bound + 3 API calls', () => {
    const calls = [];
    const runner = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', PROTECTION_OK],
      ['/check-runs', CHECKS_OK],
      ['/status', { statuses: [] }],
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
      ['/check-runs', { __error: true, status: 500, stderr: 'server error' }],
    ]);
    assert.equal(main(['1'], runner, OK_GH_VERSION), 2);
  });

  test('a real FAIL still exits 1, so exit 2 has not swallowed the FAIL path', () => {
    const runner = stubRunner([
      ['/pulls/', PR_OK],
      ['/protection', PROTECTION_OK],
      ['/check-runs', { total_count: 0, check_runs: [] }],
      ['/status', { statuses: [] }],
    ]);
    assert.equal(main(['1'], runner, OK_GH_VERSION), 1);
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
// Duplicate check-run names must not mask a failure
// ---------------------------------------------------------------------------
describe('duplicate check-run names are all evaluated', () => {

  test('a failing run is not masked by a later success under the same name', () => {
    const ctx = REQUIRED[0];
    const runs = [
      ...loadCheckRuns('checks-main-113f472.json').filter(cr => cr.name !== ctx),
      { name: ctx, status: 'completed', conclusion: 'failure' },
      { name: ctx, status: 'completed', conclusion: 'success' },
    ];
    const result = evaluateChecks({ requiredContexts: REQUIRED, checkRuns: runs, statuses: [], headSha: HEAD_113F472 });
    assert.equal(result.exitCode, 1,
      'a failing required check-run must fail even when a later run shares its name');
    assert.ok(result.lines.join('\n').includes('failure'), 'must quote the observed conclusion');
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
    const withoutMsrv = allRuns.filter(cr => cr.name !== msrvName);

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
// Code of Conduct fixture verification (AC-1, AC-2)
// ---------------------------------------------------------------------------
describe('AC-1 AC-2: Code of Conduct verification', () => {
  // D-COC2: the fixture is the recorded UPSTREAM text, and the sha256 below is
  // the whole point of recording it — without a pinned digest, "CODE_OF_CONDUCT.md
  // differs from the fixture in exactly one line" can be satisfied by editing the
  // fixture. `hash.length === 64` is true of every sha256 ever computed and
  // asserts nothing (applies ADR-009, avoids PF-013).
  //
  // Provenance, re-verified at review time:
  //   https://raw.githubusercontent.com/EthicalSource/contributor_covenant/
  //     release/content/version/2/1/code_of_conduct.md
  //   The upstream file carries a TOML front-matter block (+++ ... +++) that is
  //   site metadata, not part of the document. With it stripped, the body is
  //   byte-identical to this fixture: 5478 bytes,
  //   sha256 369bf7301883368fc19203bd0f1233fed2b83f0378ad19c4d0708bf61925339b.
  //   AC-1 recorded a different capture (977d781349351fd7c1f076e4c7dc7de2a05b40e12c773542c3815dd4ce7f37ba,
  //   5480 bytes) that does not reproduce against upstream today; the constants
  //   below reflect the measured value, not the plan's capture.
  const FIXTURE_SHA256 = '369bf7301883368fc19203bd0f1233fed2b83f0378ad19c4d0708bf61925339b';
  const FIXTURE_BYTES = 5478;

  test('fixture matches its recorded sha256 and byte count exactly', () => {
    const fixturePath = join(FIXTURES, 'contributor-covenant-2.1.md');
    const buf = readFileSync(fixturePath);
    const hash = createHash('sha256').update(buf).digest('hex');
    const size = statSync(fixturePath).size;
    assert.equal(size, FIXTURE_BYTES, `fixture must be exactly ${FIXTURE_BYTES} bytes; got ${size}`);
    assert.equal(hash, FIXTURE_SHA256,
      'fixture no longer matches the recorded upstream digest — the vendored Contributor ' +
      'Covenant text was modified; restore it rather than updating this constant');
  });

  test('CODE_OF_CONDUCT.md differs from fixture in exactly one line (contact substitution)', () => {
    const cocPath = join(ROOT, 'CODE_OF_CONDUCT.md');
    const fixturePath = join(FIXTURES, 'contributor-covenant-2.1.md');
    const coc = readFileSync(cocPath, 'utf8');
    const fixture = readFileSync(fixturePath, 'utf8');

    const cocLines = coc.split('\n');
    const fixtureLines = fixture.split('\n');

    // Find differing lines
    const maxLen = Math.max(cocLines.length, fixtureLines.length);
    const diffs = [];
    for (let i = 0; i < maxLen; i++) {
      if (cocLines[i] !== fixtureLines[i]) {
        diffs.push({ lineNo: i + 1, coc: cocLines[i], fixture: fixtureLines[i] });
      }
    }

    assert.equal(diffs.length, 1,
      `CODE_OF_CONDUCT.md must differ from fixture in exactly 1 line; got ${diffs.length} diff(s): ` +
      JSON.stringify(diffs));
    assert.ok(diffs[0].coc.includes('deanshrn@gmail.com'),
      `the differing line must contain 'deanshrn@gmail.com'; got: ${diffs[0].coc}`);
    assert.ok(
      (diffs[0].fixture ?? '').includes('[INSERT CONTACT METHOD]'),
      `fixture's differing line must contain '[INSERT CONTACT METHOD]'; got: ${diffs[0].fixture}`
    );
  });

  test('CODE_OF_CONDUCT.md does not contain [INSERT CONTACT METHOD]', () => {
    const coc = readFileSync(join(ROOT, 'CODE_OF_CONDUCT.md'), 'utf8');
    assert.ok(!coc.includes('[INSERT CONTACT METHOD]'),
      'CODE_OF_CONDUCT.md must not contain [INSERT CONTACT METHOD]');
    assert.ok(coc.includes('deanshrn@gmail.com'),
      'CODE_OF_CONDUCT.md must contain the contact email');
  });
});
