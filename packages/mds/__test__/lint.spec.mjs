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
import { lint, lintFile, lintVirtual, isMdsError, init } from '../dist/node.js';
import { assertResultShape } from '../dist/backend/contract.js';

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
  // PF-007 governs this: each surface asserts its OWN expected value — asserting
  // "<stdin>" here would lock in the wrong value for the binding side.
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
      const offsets = f.diagnostics
        .filter((d) => d.span !== null && d.span !== undefined)
        .map((d) => d.span.offset);
      for (let i = 1; i < offsets.length; i++) {
        assert.ok(
          offsets[i] >= offsets[i - 1],
          `AC-P1-24: diagnostics must be in non-decreasing offset order; ` +
            `got offsets[${i - 1}]=${offsets[i - 1]} > offsets[${i}]=${offsets[i]}`,
        );
      }
    }
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
});
