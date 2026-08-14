/**
 * lint(), lintFile(), lintVirtual() tests for @mdscript/mds universal package.
 * Tests: U-L1 through U-LV5
 *
 * All tests run against whichever backend init() selects (native if available,
 * WASM fallback). Shape assertions use assertResultShape for the canonical lint
 * result contract. Byte-identical cross-surface parity is covered in the
 * parity goldens (Step 12 — crates/mds-python/tests/test_parity.py and
 * crates/mds-napi/__test__/index.spec.mjs).
 */
import { test, describe, before } from 'node:test';
import assert from 'node:assert/strict';
import { SIMPLE_MDS, FIXTURES } from './helpers.mjs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { existsSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { lint, lintFile, lintVirtual, isMdsError, init, LINT_RULE_NAMES } from '../dist/node.js';
import { assertResultShape } from '../dist/backend/contract.js';
import { initWasmNode, createWasmBackend } from '../dist/backend/wasm.js';

// ---------------------------------------------------------------------------
// CLI helper for AC-P1-24 cross-surface differential (U-L11).
// Resolution order: MDS_CLI_BIN env var > freshest release/debug build > null.
// ---------------------------------------------------------------------------

/** Absolute path to the repository root (three levels above this test file). */
const REPO_ROOT = path.resolve(fileURLToPath(new URL('.', import.meta.url)), '../../..');

/** Return the absolute path to the `mds` CLI binary, or null if none can be found. */
function findMdsCli() {
  const envBin = process.env.MDS_CLI_BIN;
  if (envBin) {
    if (!existsSync(envBin)) throw new Error(`MDS_CLI_BIN=${envBin} does not exist`);
    return envBin;
  }
  const exe = process.platform === 'win32' ? 'mds.exe' : 'mds';
  const candidates = ['release', 'debug']
    .map((profile) => path.join(REPO_ROOT, 'target', profile, exe))
    .filter(existsSync);
  if (!candidates.length) return null;
  return candidates.reduce((a, b) => (statSync(a).mtimeMs >= statSync(b).mtimeMs ? a : b));
}

// Fixture with unused frontmatter variable (triggers unused-variable finding).
const LINT_WARN_MDS = path.join(FIXTURES, 'lint_warn.mds');

// Source strings for virtual lint tests.
const CLEAN_SOURCE = 'Hello World!\n';
const UNUSED_SOURCE = '---\nunused_key: value\n---\nHello!\n';

// ---------------------------------------------------------------------------
// lint() — source string
// ---------------------------------------------------------------------------

describe('lint', () => {
  before(() => init());

  test('U-L1: lint clean source returns version:1, empty files, truncated:false', () => {
    const result = lint(CLEAN_SOURCE);
    assert.equal(result.version, 1, 'version must be 1');
    assert.ok(Array.isArray(result.files), 'files must be an array');
    assert.equal(result.files.length, 0, 'clean source should have no file entries');
    assert.equal(result.truncated, false, 'truncated must be false');
  });

  test('U-L2: lint result passes assertResultShape', () => {
    const result = lint(CLEAN_SOURCE);
    assert.doesNotThrow(() => assertResultShape(result, 'lint'));
  });

  test('U-L3: lint detects unused frontmatter variable', () => {
    const result = lint(UNUSED_SOURCE);
    assert.equal(result.version, 1);
    const allDiags = result.files.flatMap((f) => f.diagnostics);
    const hasUnused = allDiags.some((d) => d.rule === 'unused-variable');
    assert.ok(hasUnused, `expected unused-variable finding; got: ${JSON.stringify(allDiags)}`);
  });

  test('U-L4: lint rules option silences a rule', () => {
    const result = lint(UNUSED_SOURCE, { rules: { 'unused-variable': 'off' } });
    const allDiags = result.files.flatMap((f) => f.diagnostics);
    const hasUnused = allDiags.some((d) => d.rule === 'unused-variable');
    assert.ok(!hasUnused, `unused-variable should be silenced; got: ${JSON.stringify(allDiags)}`);
  });

  test('U-L5: lint accepts null options', () => {
    const result = lint(CLEAN_SOURCE, undefined);
    assert.equal(result.version, 1);
  });

  test('U-L6: lint invalid source throws MdsError', () => {
    assert.throws(
      () => lint('Hello {{undefined_var}}!\n'),
      (err) => {
        assert.ok(isMdsError(err), `expected MdsError, got: ${err}`);
        assert.ok(err.code.startsWith('mds::'), `expected mds:: code, got: ${err.code}`);
        return true;
      },
    );
  });

  test('U-L7: lint unknown severity in rules throws MdsError', () => {
    assert.throws(
      () => lint(CLEAN_SOURCE, { rules: { 'unused-variable': 'verbose' } }),
      (err) => {
        assert.ok(isMdsError(err), `expected MdsError, got: ${err}`);
        assert.equal(err.code, 'mds::invalid_options', `got: ${err.code}`);
        return true;
      },
    );
  });

  test('U-L8: lint diagnostic has required fields', () => {
    const result = lint(UNUSED_SOURCE);
    const allDiags = result.files.flatMap((f) => f.diagnostics);
    assert.ok(allDiags.length > 0, 'expected at least one diagnostic');
    const d = allDiags[0];
    assert.ok(typeof d.rule === 'string', 'diagnostic.rule must be a string');
    assert.ok(typeof d.severity === 'string', 'diagnostic.severity must be a string');
    assert.ok(typeof d.message === 'string', 'diagnostic.message must be a string');
    assert.ok(typeof d.fixable === 'boolean', 'diagnostic.fixable must be a boolean');
  });

  // AC-P1-24 / AC-P1-06: the binding surface (napi/WASM via universal package)
  // must use "input.mds" as the file key for string-source lint, NOT "<stdin>".
  // This is a per-surface assertion that pins the binding side's expected value;
  // the CLI uses "<stdin>" (AC-P1-01). The cross-surface differential that compares
  // diagnostics[] between binding and CLI surfaces is in U-L11 (avoids PF-007).
  test('U-L9: AC-P1-24 — binding surface file key is input.mds (not <stdin>)', () => {
    const result = lint(UNUSED_SOURCE);
    assert.ok(result.files.length > 0, 'AC-P1-24: lint(UNUSED_SOURCE) must produce at least one file entry');
    for (const f of result.files) {
      assert.equal(
        f.file,
        'input.mds',
        `AC-P1-24: binding surface file key must be 'input.mds' (not '<stdin>'); got '${f.file}'`,
      );
    }
  });

  // AC-P1-24 / AC-P1-08 / AC-P1-09: diagnostics within each file group must
  // be in non-decreasing byte-offset order.  The sort is established in core
  // at LintResultBuilder::build (AD-202-1) — all surfaces inherit it.
  // Fixture: source whose rule-execution order is the REVERSE of offset order
  // (legacy-interpolation at low offset, duplicate-export at high offset;
  // run_rules dispatches duplicate_export before legacy_interpolation).
  test('U-L10: AC-P1-24 — diagnostics within a file are in non-decreasing offset order', () => {
    // {name} triggers legacy-interpolation at offset ~19 (line 2);
    // duplicate @export greet triggers duplicate-export at a high offset.
    // Without the sort these appear in reverse offset order.
    const source =
      '@define greet(name):\n  Hello {name}!\n@end\n\n@export greet\n@export greet\n';
    const result = lint(source);
    assert.ok(result.files.length > 0, 'AC-P1-24: fixture must produce at least one file entry');
    for (const f of result.files) {
      const allDiags = f.diagnostics;
      const offsets = allDiags
        .filter((d) => d.span !== null && d.span !== undefined)
        .map((d) => d.span.offset);
      // Non-vacuity guard: a loop over 0 or 1 elements executes no iterations and
      // proves nothing; equal offsets make the ordering assertion trivially true (PF-013).
      // Mirrors the Rust counterpart which asserts offsets.len() >= 2 && offsets[0] !=
      // offsets[last].
      assert.ok(
        offsets.length >= 2 && offsets[0] !== offsets[offsets.length - 1],
        `AC-P1-24: fixture needs at least 2 spanned diagnostics at DISTINCT offsets for ` +
          `a meaningful ordering assertion; got offsets ${JSON.stringify(offsets)} in file "${f.file}"`,
      );
      for (let i = 1; i < offsets.length; i++) {
        assert.ok(
          offsets[i] >= offsets[i - 1],
          `AC-P1-24: diagnostics must be in non-decreasing offset order; ` +
            `got offsets[${i - 1}]=${offsets[i - 1]} > offsets[${i}]=${offsets[i]}`,
        );
      }
      // Rule-position pinning: both rules must have fired, and legacy-interpolation must
      // appear BEFORE duplicate-export in the output array (proves the AD-202-1 sort
      // reordered against run_rules dispatch order, which dispatches duplicate-export
      // 5th and legacy-interpolation 10th — without the sort, duplicate-export would
      // appear first). Removing the sort in LintResultBuilder::build makes this fail.
      const rules = allDiags.map((d) => d.rule);
      const legacyIdx = rules.indexOf('legacy-interpolation');
      const dupIdx = rules.indexOf('duplicate-export');
      assert.ok(
        legacyIdx !== -1,
        `AC-P1-24: fixture must produce a legacy-interpolation diagnostic; ` +
          `got rules: ${JSON.stringify(rules)} in file "${f.file}"`,
      );
      assert.ok(
        dupIdx !== -1,
        `AC-P1-24: fixture must produce a duplicate-export diagnostic; ` +
          `got rules: ${JSON.stringify(rules)} in file "${f.file}"`,
      );
      assert.ok(
        legacyIdx < dupIdx,
        `AC-P1-24: emitted order must follow byte offset, not rule-dispatch order ` +
          `(duplicate-export dispatched 5th but fires at a later offset than ` +
          `legacy-interpolation dispatched 10th); got indices legacy=${legacyIdx} ` +
          `dup=${dupIdx} in file "${f.file}"`,
      );
    }
  });

  // AC-P1-24 cross-surface differential: the napi/WASM binding surface and the CLI
  // must produce byte-identical diagnostics[] when the file key is excluded.
  //
  // The file key intentionally differs ("input.mds" on binding vs "<stdin>" on CLI
  // stdin) — this is by design (AC-P1-01 / AC-P1-06). Comparing surfaces to each
  // other with the file key stripped is the only comparison that can catch a
  // divergence in diagnostic content, span offsets, or ordering (avoids PF-007,
  // which states per-surface goldens cannot prove cross-surface byte parity).
  //
  // The same fixture as U-L10 is reused: legacy-interpolation fires at a lower
  // offset than duplicate-export, but run_rules dispatches duplicate-export first,
  // so the offset sort must have run for the arrays to match.
  //
  // In CI the CLI is required; locally, if no binary is found, the test warns and
  // exits early (avoids a hard-fail when building only the JS side).
  test('U-L11: AC-P1-24 cross-surface — napi/WASM and CLI diagnostics[] are byte-identical (file key excluded)', () => {
    const mdsCli = findMdsCli();
    if (mdsCli == null) {
      if (process.env.CI) {
        throw new Error(
          'U-L11: mds CLI binary is required in CI for AC-P1-24 cross-surface parity. ' +
            'Set MDS_CLI_BIN or run `cargo build -p mds-cli`.',
        );
      }
      // Local dev without a built CLI: warn and skip (not a hard failure).
      console.warn(
        'U-L11: skipping CLI surface — mds binary not found ' +
          '(run `cargo build -p mds-cli` to enable this check)',
      );
      return;
    }

    // Fixture: {name} triggers legacy-interpolation at a low byte offset; duplicate
    // @export greet triggers duplicate-export at a higher offset. rule-execution order
    // is the REVERSE of offset order — correct sort changes the observed array order.
    const source =
      '@define greet(name):\n  Hello {name}!\n@end\n\n@export greet\n@export greet\n';

    // -- Binding surface (napi or WASM, whichever init() selected) --
    const bindingResult = lint(source);
    assert.ok(
      bindingResult.files.length > 0,
      'U-L11: fixture must produce at least one file entry from the binding surface',
    );

    // -- CLI surface: stdin lint in JSON mode; file key will be "<stdin>" --
    const cliProc = spawnSync(mdsCli, ['lint', '-', '--format', 'json'], {
      input: source,
      encoding: 'utf-8',
    });
    // exit 0 = clean, exit 1 = warn-only findings, exit 2 = at least one error-severity
    // finding.  The fixture's duplicate-export rule fires at "error" severity, so exit 2
    // is the expected normal outcome (not a CLI failure).
    assert.ok(
      cliProc.status === 0 || cliProc.status === 1 || cliProc.status === 2,
      `U-L11: CLI lint exited unexpected status ${cliProc.status}: ${cliProc.stderr}`,
    );
    const cliResult = JSON.parse(cliProc.stdout);
    assert.ok(
      cliResult.files.length > 0,
      'U-L11: fixture must produce at least one file entry from the CLI surface',
    );

    // Normalize: strip the file key from each files[] entry so the comparison is
    // over diagnostics[], not the deliberately-different source identity.
    function normalizeFiles(result) {
      return result.files.map(({ file: _file, ...rest }) => rest);
    }
    const bindingNorm = normalizeFiles(bindingResult);
    const cliNorm = normalizeFiles(cliResult);

    // Non-vacuity guard: two empty arrays compare equal and prove nothing (PF-013).
    const totalDiags = bindingNorm.reduce((n, f) => n + (f.diagnostics?.length ?? 0), 0);
    assert.ok(
      totalDiags >= 2,
      `U-L11: AC-P1-24 needs at least two diagnostics for a meaningful cross-surface ` +
        `differential; got ${totalDiags}`,
    );

    // Primary assertion: diagnostics[], spans, rule, severity, fixable are identical
    // across the two surfaces (the cross-surface differential, avoids PF-007).
    assert.deepEqual(
      bindingNorm,
      cliNorm,
      'U-L11: AC-P1-24: napi/WASM and CLI diagnostics[] must be identical when ' +
        `file key is excluded.\n  binding: ${JSON.stringify(bindingNorm)}\n` +
        `  CLI:     ${JSON.stringify(cliNorm)}`,
    );

    // Separately assert the per-surface file key values (AC-P1-24 / AC-P1-01 / AC-P1-06).
    assert.equal(
      bindingResult.files[0].file,
      'input.mds',
      'U-L11: binding surface string-source file key must be "input.mds"',
    );
    assert.equal(
      cliResult.files[0].file,
      '<stdin>',
      'U-L11: CLI stdin file key must be "<stdin>"',
    );
  });

  // AC-P1-24 cross-surface differential (WASM surface): init() always selects
  // native where the napi addon resolves, so U-L11 exercises napi only and the
  // WASM branch of its "napi or WASM" comment is dead (PF-007). This test forces
  // the WASM backend directly via initWasmNode() + createWasmBackend(), making
  // the WASM surface the one under test.
  //
  // Three assertions are combined here:
  //   (a) getBackend() === 'wasm' -- shape check on the backend descriptor string.
  //       NOTE: this assertion cannot detect a silent backend switch because
  //       createWasmBackend() hardcodes 'wasm'; it is NOT a PF-007 guard.
  //   (b) files[].file === 'input.mds' -- AC-P1-28(c): WASM string-source lint
  //       MUST keep the shared constant as its virtual-FS key.
  //   (c) WASM diagnostics[] equal CLI diagnostics[] when the file key is
  //       excluded -- the actual cross-surface parity differential (PF-007).
  //
  // In CI the WASM module is built before npm test; the assertion is hard.
  // Locally, if the WASM module is not yet built, the test warns and skips.
  // The CLI differential leg (c) additionally skips if no CLI binary is found.
  test('U-L12: AC-P1-24 cross-surface -- WASM backend lint matches CLI diagnostics[] (file key excluded, AC-P1-28(c))', async () => {
    // -- Force WASM backend directly, bypassing init()'s native preference --
    let wasmBackend;
    try {
      const wasmMod = await initWasmNode();
      wasmBackend = createWasmBackend(wasmMod);
    } catch (initErr) {
      if (process.env.CI) {
        throw new Error(
          'U-L12: WASM backend is required in CI for AC-P1-24 WASM surface coverage. ' +
            'Build it with: wasm-pack build crates/mds-wasm --target nodejs --out-dir pkg. ' +
            `Caused by: ${initErr.message}`,
        );
      }
      // Local dev without a built WASM module: warn and skip (not a hard failure).
      console.warn(
        'U-L12: skipping WASM surface -- mds-wasm module not found ' +
          '(run `wasm-pack build crates/mds-wasm --target nodejs --out-dir pkg` to enable)',
      );
      return;
    }

    // (a) Shape check: the backend descriptor must be the literal string 'wasm'.
    // This verifies the API contract of createWasmBackend(), not surface routing --
    // the test constructs the backend directly so no code path can return a different
    // backend here (cannot detect a silent backend switch; not a PF-007 guard).
    assert.equal(
      wasmBackend.getBackend(),
      'wasm',
      'U-L12: wasmBackend.getBackend() must return "wasm"',
    );

    // Same fixture as U-L10/U-L11: {name} triggers legacy-interpolation at a
    // low byte offset; duplicate @export greet triggers duplicate-export at a
    // higher offset. sort_diagnostics in LintResultBuilder::build is called by
    // both the WASM surface and the CLI, so both emit diagnostics in offset order
    // regardless of run_rules dispatch order. The rule-position pinning assertion
    // below verifies the sort actually reordered from dispatch order.
    const source =
      '@define greet(name):\n  Hello {name}!\n@end\n\n@export greet\n@export greet\n';

    const wasmResult = wasmBackend.lint(source);
    assert.ok(
      wasmResult.files.length > 0,
      'U-L12: fixture must produce at least one file entry from the WASM surface',
    );

    // (b) AC-P1-28(c): WASM string-source lint MUST emit files[].file == 'input.mds'.
    // STRING_SOURCE_MAP_LABEL is the shared constant that both the WASM virtual-FS
    // default filename and the core lint entry point use -- it must not be remapped
    // to '<stdin>' on the WASM surface (the remap belongs at the CLI output boundary).
    for (const f of wasmResult.files) {
      assert.equal(
        f.file,
        'input.mds',
        `U-L12: AC-P1-28(c): WASM string-source file key must be 'input.mds'; got '${f.file}'`,
      );
    }

    // (c) Cross-surface differential against the CLI (if a binary is available).
    const mdsCli = findMdsCli();
    if (mdsCli == null) {
      if (process.env.CI) {
        throw new Error(
          'U-L12: mds CLI binary is required in CI for the WASM vs CLI differential. ' +
            'Set MDS_CLI_BIN or run `cargo build -p mds-cli`.',
        );
      }
      console.warn(
        'U-L12: WASM vs CLI differential skipped -- mds CLI binary not found ' +
          '(run `cargo build -p mds-cli` to enable the full cross-surface check)',
      );
      return;
    }

    const cliProc = spawnSync(mdsCli, ['lint', '-', '--format', 'json'], {
      input: source,
      encoding: 'utf-8',
    });
    // exit 0 = clean, exit 1 = warn-only, exit 2 = at least one error-severity finding.
    assert.ok(
      cliProc.status === 0 || cliProc.status === 1 || cliProc.status === 2,
      `U-L12: CLI lint exited unexpected status ${cliProc.status}: ${cliProc.stderr}`,
    );
    const cliResult = JSON.parse(cliProc.stdout);
    assert.ok(
      cliResult.files.length > 0,
      'U-L12: fixture must produce at least one file entry from the CLI surface',
    );

    // Normalize: strip the file key (deliberately differs per surface) so the
    // comparison covers diagnostics[], spans, rule, severity, fixable only.
    function normalizeFiles(result) {
      return result.files.map(({ file: _file, ...rest }) => rest);
    }
    const wasmNorm = normalizeFiles(wasmResult);
    const cliNorm = normalizeFiles(cliResult);

    // Non-vacuity guard: two empty arrays compare equal and prove nothing (PF-013).
    const totalDiags = wasmNorm.reduce((n, f) => n + (f.diagnostics?.length ?? 0), 0);
    assert.ok(
      totalDiags >= 2,
      `U-L12: AC-P1-24 needs at least two diagnostics for a meaningful cross-surface ` +
        `differential; got ${totalDiags}`,
    );

    // Rule-position pinning: both rules must have fired, and legacy-interpolation
    // must appear BEFORE duplicate-export (proves the AD-202-1 sort reordered
    // against run_rules dispatch order). Delete the sort in LintResultBuilder::build
    // and this fails.
    const rules = wasmNorm.flatMap((f) => (f.diagnostics ?? []).map((d) => d.rule));
    const legacyIdx = rules.indexOf('legacy-interpolation');
    const dupIdx = rules.indexOf('duplicate-export');
    assert.ok(
      legacyIdx !== -1,
      `U-L12: fixture must produce a legacy-interpolation diagnostic; got rules: ${JSON.stringify(rules)}`,
    );
    assert.ok(
      dupIdx !== -1,
      `U-L12: fixture must produce a duplicate-export diagnostic; got rules: ${JSON.stringify(rules)}`,
    );
    assert.ok(
      legacyIdx < dupIdx,
      `U-L12: legacy-interpolation (lower offset) must appear before duplicate-export ` +
        `(higher offset); got indices legacy=${legacyIdx} dup=${dupIdx}`,
    );

    // Primary parity assertion: WASM and CLI diagnostics[] are identical when
    // the file key is excluded (the cross-surface differential, avoids PF-007).
    assert.deepStrictEqual(
      wasmNorm,
      cliNorm,
      'U-L12: AC-P1-24: WASM and CLI diagnostics[] must be identical when file key is excluded.\n' +
        `  WASM: ${JSON.stringify(wasmNorm)}\n` +
        `  CLI:  ${JSON.stringify(cliNorm)}`,
    );
  });
});

// ---------------------------------------------------------------------------
// lintFile() — file path
// ---------------------------------------------------------------------------

describe('lintFile', () => {
  before(() => init());

  test('U-LF1: lintFile clean file returns valid shape', async () => {
    const result = await lintFile(SIMPLE_MDS);
    assert.equal(result.version, 1);
    assert.ok(Array.isArray(result.files));
    assert.equal(result.truncated, false);
    assertResultShape(result, 'lint');
  });

  test('U-LF2: lintFile with findings returns file entries', async () => {
    const result = await lintFile(LINT_WARN_MDS);
    assert.equal(result.version, 1);
    const allDiags = result.files.flatMap((f) => f.diagnostics);
    const hasUnused = allDiags.some((d) => d.rule === 'unused-variable');
    assert.ok(hasUnused, `expected unused-variable in ${LINT_WARN_MDS}; got: ${JSON.stringify(allDiags)}`);
  });

  test('U-LF3: lintFile rules option silences rule', async () => {
    const result = await lintFile(LINT_WARN_MDS, { rules: { 'unused-variable': 'off' } });
    const allDiags = result.files.flatMap((f) => f.diagnostics);
    const hasUnused = allDiags.some((d) => d.rule === 'unused-variable');
    assert.ok(!hasUnused, 'unused-variable should be silenced');
  });

  test('U-LF4: lintFile nonexistent file rejects', async () => {
    await assert.rejects(
      () => lintFile('/nonexistent/path/no_such.mds'),
      (err) => {
        assert.ok(err instanceof Error);
        return true;
      },
    );
  });
});

// ---------------------------------------------------------------------------
// lintVirtual() — virtual filesystem
// ---------------------------------------------------------------------------

describe('lintVirtual', () => {
  before(() => init());

  test('U-LV1: lintVirtual clean module returns version:1, empty files', () => {
    const result = lintVirtual({ 'main.mds': CLEAN_SOURCE }, 'main.mds');
    assert.equal(result.version, 1);
    assert.ok(Array.isArray(result.files));
    assert.equal(result.files.length, 0);
    assert.equal(result.truncated, false);
    assertResultShape(result, 'lint');
  });

  test('U-LV2: lintVirtual detects findings in virtual module', () => {
    const result = lintVirtual({ 'main.mds': UNUSED_SOURCE }, 'main.mds');
    const allDiags = result.files.flatMap((f) => f.diagnostics);
    const hasUnused = allDiags.some((d) => d.rule === 'unused-variable');
    assert.ok(hasUnused, `expected unused-variable; got: ${JSON.stringify(allDiags)}`);
  });

  test('U-LV3: lintVirtual rules option silences rule', () => {
    const result = lintVirtual(
      { 'main.mds': UNUSED_SOURCE },
      'main.mds',
      { rules: { 'unused-variable': 'off' } },
    );
    const allDiags = result.files.flatMap((f) => f.diagnostics);
    const hasUnused = allDiags.some((d) => d.rule === 'unused-variable');
    assert.ok(!hasUnused, 'unused-variable should be silenced');
  });

  test('U-LV4: lintVirtual entry not in modules throws', () => {
    assert.throws(
      () => lintVirtual({ 'main.mds': CLEAN_SOURCE }, 'other.mds'),
      (err) => {
        assert.ok(err instanceof Error);
        return true;
      },
    );
  });

  test('U-LV5: lintVirtual result shape is canonical lint JSON', () => {
    const result = lintVirtual({ 'main.mds': UNUSED_SOURCE }, 'main.mds');
    // The file key in the JSON must match the entry key.
    assert.ok(result.files.length > 0, 'expected at least one file entry');
    assert.equal(result.files[0].file, 'main.mds', 'file key must match entry');
  });

  test('U-LV6: cross-surface parity: clean source produces identical shape from lint/lintVirtual/lintFile', async () => {
    // Clean sources produce {version:1, files:[], truncated:false} on all surfaces.
    // Key order may differ between surfaces (serde_json BTreeMap vs JS insertion
    // order) so compare with deepEqual on parsed objects.
    const expected = { version: 1, files: [], truncated: false };
    const fromLint = lint(CLEAN_SOURCE);
    const fromLintVirtual = lintVirtual({ 'main.mds': CLEAN_SOURCE }, 'main.mds');
    const fromLintFile = await lintFile(SIMPLE_MDS);
    assert.deepEqual(fromLint, expected, 'lint clean must match expected shape');
    assert.deepEqual(fromLintVirtual, expected, 'lintVirtual clean must match expected shape');
    assert.deepEqual(fromLintFile, expected, 'lintFile clean must match expected shape');
  });
});

// ---------------------------------------------------------------------------
// Lint canonical JSON goldens (Step 12 — AC-API-06)
// ---------------------------------------------------------------------------
//
// Goldens are derived from the Rust core serializer and checked in. Comparing
// JS-serialized lintVirtual() output against these strings catches key-order
// drift or shape changes across releases. Non-circular: goldens are NOT
// regenerated from the universal package itself.
//
// Keys in BTreeMap alphabetical order: {"files":[...],"truncated":false,"version":1}

describe('lint canonical JSON goldens', () => {
  before(() => init());

  test('U-LG1: lintVirtual clean source matches canonical golden', () => {
    const CLEAN_GOLDEN = '{"files":[],"truncated":false,"version":1}';
    const result = lintVirtual({ 'main.mds': CLEAN_SOURCE }, 'main.mds');
    assert.equal(
      JSON.stringify(result),
      CLEAN_GOLDEN,
      `lintVirtual clean golden mismatch: got ${JSON.stringify(result)}`,
    );
  });

  test('U-LG2: lintVirtual unused-variable source matches canonical golden', () => {
    const UNUSED_GOLDEN =
      '{"files":[{"diagnostics":[{"fix_edits":null,"fixable":false,"help":"Remove the frontmatter key or reference it in the template body.",' +
      '"message":"Variable \'unused_key\' is defined in frontmatter but never referenced in the body.",' +
      '"rule":"unused-variable","severity":"warn","span":{"length":10,"offset":4}}],"file":"main.mds"}],"truncated":false,"version":1}';
    const result = lintVirtual({ 'main.mds': UNUSED_SOURCE }, 'main.mds');
    assert.equal(
      JSON.stringify(result),
      UNUSED_GOLDEN,
      `lintVirtual unused-variable golden mismatch: got ${JSON.stringify(result)}`,
    );
  });

  test('U-LG3: lintVirtual silenced rule produces same clean golden', () => {
    const CLEAN_GOLDEN = '{"files":[],"truncated":false,"version":1}';
    const result = lintVirtual(
      { 'main.mds': UNUSED_SOURCE },
      'main.mds',
      { rules: { 'unused-variable': 'off' } },
    );
    assert.equal(
      JSON.stringify(result),
      CLEAN_GOLDEN,
      `lintVirtual silenced golden mismatch: got ${JSON.stringify(result)}`,
    );
  });

  // spec.md normative claim: in alphabetical key order `lint_warnings` sorts between
  // `files` and `truncated`. This test pins that position in the actual JSON output.
  // (ADR-009 / PF-013: verifying key ORDER requires a result that actually contains the
  // key; clean-source goldens above cannot exercise this.)
  test('U-LG4: lint_warnings key sorts between files and truncated in the JSON wire form', () => {
    const result = lintVirtual(
      { 'main.mds': CLEAN_SOURCE },
      'main.mds',
      { rules: { 'no-such-rule-xyzzy': 'warn' } },
    );
    assert.ok(
      Array.isArray(result.lint_warnings) && result.lint_warnings.length > 0,
      `U-LG4: lint_warnings must be a non-empty array; got ${JSON.stringify(result.lint_warnings)}`,
    );
    const json = JSON.stringify(result);
    const filesIdx = json.indexOf('"files"');
    const warnIdx = json.indexOf('"lint_warnings"');
    const truncIdx = json.indexOf('"truncated"');
    assert.ok(
      filesIdx !== -1 && warnIdx !== -1 && truncIdx !== -1,
      `U-LG4: expected files, lint_warnings, and truncated keys in serialized result; got: ${json}`,
    );
    assert.ok(
      filesIdx < warnIdx && warnIdx < truncIdx,
      `U-LG4: lint_warnings must sort between files and truncated (alphabetical BTreeMap order); ` +
        `filesIdx=${filesIdx} warnIdx=${warnIdx} truncIdx=${truncIdx} in: ${json}`,
    );
  });
});

// ---------------------------------------------------------------------------
// AC-224-1 / AC-224-8 (D8): unknown lint rule names on the universal surface.
//
// The universal package is the fifth surface AC-224-1 enumerates, and PF-007 says
// a per-surface assertion elsewhere proves nothing here: napi's spec proves napi,
// web.rs proves WASM. These tests assert the contract as `@mdscript/mds` exposes
// it, on the default backend AND on a directly-constructed WASM backend, so the
// claim is not silently limited to whichever backend init() happens to pick.
//
// Contract: an unknown rule name never throws. It surfaces in
// `result.lint_warnings`, and the field is ABSENT when every name is recognised.
// ---------------------------------------------------------------------------

describe('unknown lint rule names (AC-224-1 / D8)', () => {
  before(() => init());

  /** Assert one lint result carries a warning naming `ruleName`. */
  function assertWarnsAbout(result, ruleName, label) {
    assert.equal(result.version, 1, `${label}: version must be 1`);
    assert.ok(
      Array.isArray(result.lint_warnings),
      `${label}: lint_warnings must be an array; got ${JSON.stringify(result.lint_warnings)}`,
    );
    assert.ok(result.lint_warnings.length > 0, `${label}: lint_warnings must be non-empty`);
    const joined = result.lint_warnings.join(' ');
    assert.ok(
      joined.includes(ruleName),
      `${label}: lint_warnings must name ${ruleName}; got: ${joined}`,
    );
    assert.ok(
      joined.includes('recognised rules are'),
      `${label}: lint_warnings must list the recognised rules; got: ${joined}`,
    );
  }

  test('U-L-WARN-1: lint() warns and continues on an unknown rule name', () => {
    const result = lint(UNUSED_SOURCE, { rules: { 'no-such-rule-xyzzy': 'warn' } });
    assertWarnsAbout(result, 'no-such-rule-xyzzy', 'U-L-WARN-1');
    // Lint CONTINUES: the real finding is still reported, unchanged.
    assert.doesNotThrow(() => assertResultShape(result, 'lint'));
    const rules = result.files.flatMap((f) => f.diagnostics).map((d) => d.rule);
    assert.ok(
      rules.includes('unused-variable'),
      `U-L-WARN-1: linting must continue and still report findings; got: ${JSON.stringify(rules)}`,
    );
  });

  test('U-L-WARN-2: lintVirtual() warns on an unknown rule name', () => {
    const result = lintVirtual(
      { 'main.mds': CLEAN_SOURCE },
      'main.mds',
      { rules: { 'no-such-rule-xyzzy': 'error' } },
    );
    assertWarnsAbout(result, 'no-such-rule-xyzzy', 'U-L-WARN-2');
  });

  test('U-L-WARN-3: lintFile() warns on an unknown rule name', async () => {
    const result = await lintFile(LINT_WARN_MDS, { rules: { 'no-such-rule-xyzzy': 'warn' } });
    assertWarnsAbout(result, 'no-such-rule-xyzzy', 'U-L-WARN-3');
  });

  // Paired negative arm (PF-013 / ADR-009): the positive arms above only prove the
  // field can appear. Without this, an implementation that always populated
  // lint_warnings would pass every assertion above.
  test('U-L-WARN-4: every recognised rule name is accepted with no lint_warnings', () => {
    assert.equal(LINT_RULE_NAMES.length, 10, 'LINT_RULE_NAMES must have exactly 10 entries');
    for (const name of LINT_RULE_NAMES) {
      const result = lint(CLEAN_SOURCE, { rules: { [name]: 'off' } });
      assert.equal(
        result.lint_warnings,
        undefined,
        `U-L-WARN-4: ${name} is a recognised rule — lint_warnings must be absent; ` +
          `got: ${JSON.stringify(result.lint_warnings)}`,
      );
    }
    // Empty and absent rules maps behave identically to a recognised-only map.
    assert.equal(lint(CLEAN_SOURCE, { rules: {} }).lint_warnings, undefined);
    assert.equal(lint(CLEAN_SOURCE).lint_warnings, undefined);
  });

  test('U-L-WARN-5: an unknown rule name never throws (contrast: unknown severity does)', () => {
    assert.doesNotThrow(
      () => lint(CLEAN_SOURCE, { rules: { 'no-such-rule-xyzzy': 'warn' } }),
      'U-L-WARN-5: an unknown rule NAME must not throw',
    );
    // The asymmetry is deliberate and must stay: unknown severity VALUES still throw.
    assert.throws(
      () => lint(CLEAN_SOURCE, { rules: { 'unused-variable': 'verbose' } }),
      (err) => {
        assert.ok(isMdsError(err), `expected MdsError, got: ${err}`);
        assert.equal(err.code, 'mds::invalid_options', `got code: ${err.code}`);
        return true;
      },
      'U-L-WARN-5: an unknown severity VALUE must still throw mds::invalid_options',
    );
  });

  // PF-007: init() prefers native wherever the napi addon resolves, so every test
  // above may have exercised only one backend. Force the WASM backend directly so
  // the universal contract is proven on both, mirroring U-L12's approach.
  test('U-L-WARN-6: the same contract holds on a directly-constructed WASM backend', async () => {
    let wasmBackend;
    try {
      const wasmMod = await initWasmNode();
      wasmBackend = createWasmBackend(wasmMod);
    } catch (initErr) {
      if (process.env.CI) {
        throw new Error(
          'U-L-WARN-6: WASM backend is required in CI for AC-224-1 universal/WASM coverage. ' +
            `Caused by: ${initErr.message}`,
        );
      }
      console.warn('U-L-WARN-6: skipping WASM surface -- mds-wasm module not built');
      return;
    }
    const warned = wasmBackend.lint(CLEAN_SOURCE, { rules: { 'no-such-rule-xyzzy': 'warn' } });
    assertWarnsAbout(warned, 'no-such-rule-xyzzy', 'U-L-WARN-6');
    const clean = wasmBackend.lint(CLEAN_SOURCE, { rules: { 'unused-variable': 'off' } });
    assert.equal(
      clean.lint_warnings,
      undefined,
      `U-L-WARN-6: recognised rules must produce no lint_warnings; got: ${JSON.stringify(clean.lint_warnings)}`,
    );
  });
});
