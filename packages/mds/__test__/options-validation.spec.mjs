/**
 * Options-validation tests — assertKnownKeys wrapper-level enforcement, plus
 * basePath forwarding and rejection across both backends.
 * Tests: U-OV-1 through U-OV-34.
 *
 * Verifies that the universal @mdscript/mds wrapper rejects unknown option keys
 * with code === 'mds::invalid_options' before dispatching to any backend.
 * Unknown-key validation runs inside the wrapper (before backend dispatch) and is
 * therefore backend-agnostic: the same rejection fires on native and WASM paths.
 *
 * basePath behaviour is NOT backend-agnostic (OD-1): the native backend honors it
 * on the string surface, while the WASM backend rejects it (no filesystem access).
 * Tests touching basePath are therefore either native-gated via requireNativeAddon()
 * or written backend-aware via assertWrapperAcceptsBasePath().
 *
 * U-OV-14 performs a byte-identical message parity check across all seven
 * methods against the native napi backend. That test hard-fails when the
 * native addon is absent — a silently-passing skip is how parity regressions
 * survive undetected (avoids PF-007).
 */
import { test, describe, before } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import {
  compile,
  check,
  compileFile,
  checkFile,
  lint,
  lintFile,
  lintVirtual,
  isMdsError,
  init,
  getBackend,
} from '../dist/node.js';
import { assertKnownKeys, METHOD_KEYS, forwardOpts } from '../dist/util/options.js';
import * as os from 'node:os';
import * as fs from 'node:fs';
import * as path from 'node:path';

const require = createRequire(import.meta.url);

describe('options-validation', () => {
  before(() => init());

  // ── compile: typo'd key ────────────────────────────────────────────────────

  test('U-OV-1: compile rejects unknown key "sourceMaps"', () => {
    assert.throws(
      () => compile('Hello\n', { sourceMaps: true }),
      (err) => {
        assert.ok(isMdsError(err), `expected isMdsError, got: ${err}`);
        assert.equal(err.code, 'mds::invalid_options');
        assert.ok(err.message.includes('"sourceMaps"'), `key name in message: ${err.message}`);
        assert.ok(err.message.includes('recognised keys are:'), `format check: ${err.message}`);
        return true;
      },
    );
  });

  test('U-OV-2: compile rejects unknown key "varsJson"', () => {
    assert.throws(
      () => compile('Hello\n', { varsJson: '{}' }),
      (err) => {
        assert.ok(isMdsError(err));
        assert.equal(err.code, 'mds::invalid_options');
        return true;
      },
    );
  });

  test('U-OV-3: compile rejects snake_case alias "source_map"', () => {
    assert.throws(
      () => compile('Hello\n', { source_map: true }),
      (err) => {
        assert.ok(isMdsError(err));
        assert.equal(err.code, 'mds::invalid_options');
        return true;
      },
    );
  });

  // ── check: typo'd key ─────────────────────────────────────────────────────

  test('U-OV-4: check rejects unknown key "base_path" (snake_case)', () => {
    assert.throws(
      () => check('Hello\n', { base_path: '.' }),
      (err) => {
        assert.ok(isMdsError(err));
        assert.equal(err.code, 'mds::invalid_options');
        assert.ok(err.message.includes('"base_path"'), `key name in message: ${err.message}`);
        return true;
      },
    );
  });

  test('U-OV-5: check rejects sourceMap (not valid for check)', () => {
    assert.throws(
      () => check('Hello\n', { sourceMap: true }),
      (err) => {
        assert.ok(isMdsError(err));
        assert.equal(err.code, 'mds::invalid_options');
        return true;
      },
    );
  });

  // ── lint: typo'd key ──────────────────────────────────────────────────────

  test('U-OV-6: lint rejects unknown key "base_path" (snake_case)', () => {
    assert.throws(
      () => lint('Hello\n', { base_path: '.' }),
      (err) => {
        assert.ok(isMdsError(err));
        assert.equal(err.code, 'mds::invalid_options');
        assert.ok(err.message.includes('"base_path"'), `key name: ${err.message}`);
        return true;
      },
    );
  });

  // ── lintVirtual: basePath not allowed there ────────────────────────────────

  test('U-OV-7: lintVirtual rejects basePath (not in LintFileOptions)', () => {
    const modules = { 'a.mds': 'Hello\n' };
    assert.throws(
      () => lintVirtual(modules, 'a.mds', { basePath: '.' }),
      (err) => {
        assert.ok(isMdsError(err));
        assert.equal(err.code, 'mds::invalid_options');
        assert.ok(err.message.includes('"basePath"'), `key name: ${err.message}`);
        return true;
      },
    );
  });

  // ── valid options pass through ─────────────────────────────────────────────

  /**
   * Assert that the WRAPPER does not reject `basePath` as an unknown option key.
   *
   * OD-1 made a bare `doesNotThrow` backend-dependent: the native backend honors
   * `basePath` (no throw), while the WASM backend rejects it because it has no
   * filesystem. Both outcomes prove the property these tests exist for — that
   * `assertKnownKeys` no longer intercepts `basePath` on the string surface (#180).
   * Asserting backend-aware keeps the test meaningful on a machine that falls back
   * to WASM (node.ts:225) instead of failing with a misleading
   * "wrapper reconciliation" message that misattributes correct WASM behaviour to
   * a wrapper regression.
   *
   * The WASM branch is not a free pass: it asserts the error is the WASM-backend
   * rejection and explicitly NOT the generic `unknown option key` form, which is
   * the exact regression being guarded (avoids PF-013 — a bare "it threw" would
   * accept the very failure this test must catch).
   */
  function assertWrapperAcceptsBasePath(fn, label) {
    let caught;
    try {
      fn();
    } catch (err) {
      caught = err;
    }
    if (caught === undefined) {
      assert.equal(
        getBackend(), 'native',
        `${label}: only the native backend may accept basePath without throwing; backend=${getBackend()}`,
      );
      return;
    }
    assert.equal(
      getBackend(), 'wasm',
      `${label}: basePath must not throw on the native backend — wrapper regression? err: ${caught.message}`,
    );
    assert.ok(isMdsError(caught), `${label}: expected isMdsError, got: ${caught}`);
    assert.equal(caught.code, 'mds::invalid_options', `${label}: wrong error code`);
    assert.ok(
      !caught.message.startsWith('unknown option key'),
      `${label}: wrapper must not reject basePath as an unknown key (#180 regression): ${caught.message}`,
    );
    assert.ok(
      caught.message.includes('WASM'),
      `${label}: expected the WASM-backend basePath rejection, got: ${caught.message}`,
    );
  }

  test('U-OV-8: compile accepts all valid keys without error', () => {
    assert.doesNotThrow(() => compile('Hello\n', { vars: {}, sourceMap: false, sourcesContent: false }));
    // basePath is now also accepted by the wrapper (reconciled with napi
    // parse_compile_opts — issue #180). Backend-aware: see the helper above.
    assertWrapperAcceptsBasePath(() => compile('Hello\n', { basePath: '.' }), 'U-OV-8 compile');
  });

  test('U-OV-9: check accepts vars and basePath without error', () => {
    assert.doesNotThrow(() => check('Hello\n', { vars: {} }));
    // basePath is now also accepted by the wrapper (reconciled with napi
    // parse_check_opts — issue #180). Backend-aware: see the helper above.
    assertWrapperAcceptsBasePath(() => check('Hello\n', { basePath: '.' }), 'U-OV-9 check');
  });

  test('U-OV-10: no options does not throw', () => {
    assert.doesNotThrow(() => compile('Hello\n'));
    assert.doesNotThrow(() => compile('Hello\n', undefined));
    assert.doesNotThrow(() => check('Hello\n'));
    assert.doesNotThrow(() => check('Hello\n', undefined));
  });

  // ── multiple unknown keys ──────────────────────────────────────────────────

  test('U-OV-11: compile rejects multiple unknown keys, lists them all', () => {
    assert.throws(
      () => compile('Hello\n', { sourceMaps: true, varsJson: '{}' }),
      (err) => {
        assert.ok(isMdsError(err));
        assert.equal(err.code, 'mds::invalid_options');
        // Plural form: "unknown option keys:"
        assert.ok(err.message.startsWith('unknown option keys:'), `plural form: ${err.message}`);
        assert.ok(err.message.includes('"sourceMaps"') && err.message.includes('"varsJson"'));
        return true;
      },
    );
  });

  // ── async file-ops throw unknown-key errors synchronously ────────────────
  //
  // checkFile and lintFile are NOT async functions: assertKnownKeys() fires
  // before any I/O and throws synchronously.  The tests are intentionally
  // non-async and use assert.throws (not assert.rejects) so that a regression
  // to async would be caught: Node.js assert.rejects() with a validator
  // function does NOT intercept synchronous throws in v22, so the error would
  // escape the validator and the test would fail with "testCodeFailure" rather
  // than "Missing expected exception", masking the regression.

  test('U-OV-12: checkFile throws synchronously for unknown key (sourceMap not valid for check)', () => {
    // assertKnownKeys fires before any I/O, so no real file is needed.
    assert.throws(
      () => checkFile('/any.mds', { sourceMap: true }),
      (err) => {
        assert.ok(isMdsError(err), `expected isMdsError, got: ${err}`);
        assert.equal(err.code, 'mds::invalid_options');
        return true;
      },
    );
  });

  test('U-OV-13: lintFile throws synchronously for unknown key "basePath"', () => {
    // assertKnownKeys fires before any I/O, so no real file is needed.
    assert.throws(
      () => lintFile('/any.mds', { basePath: '.' }),
      (err) => {
        assert.ok(isMdsError(err), `expected isMdsError, got: ${err}`);
        assert.equal(err.code, 'mds::invalid_options');
        return true;
      },
    );
  });

  // ── message-parity: wrapper format matches backend format (all 7 methods) ──

  test('U-OV-14: wrapper error message is byte-identical to napi for all seven methods (avoids PF-007)', async () => {
    // Hard-fail if native addon not available — a silently-passing skip is exactly
    // how parity regressions survive undetected (PF-007: per-surface goldens each
    // lock in their own value, defeating cross-surface parity).
    const addon = require('@mdscript/mds-napi');

    // A key not recognised by any method: triggers the standard
    // "unknown option key" format from both wrapper and napi.
    // 'sourceMaps' is a common typo for 'sourceMap'; not in any method's list.
    const BAD_OPT = { sourceMaps: true };

    // Virtual modules for lintVirtual: modules must be valid before options are checked.
    const VIRTUAL_MODS = { 'a.mds': '' };
    const VIRTUAL_ENTRY = 'a.mds';

    // Call fn(), awaiting if it returns a Promise. Returns the error message if fn
    // throws (sync or async), or an empty string if it does not throw.
    async function captureMsg(fn) {
      try {
        const result = fn();
        if (result != null && typeof result === 'object' && typeof result.then === 'function') {
          await result;
        }
      } catch (e) {
        return e instanceof Error ? e.message : String(e);
      }
      return '';
    }

    const cases = [
      {
        name: 'compile',
        wrapperFn: () => compile('', BAD_OPT),
        addonFn:   () => addon.compile('', BAD_OPT),
      },
      {
        name: 'check',
        wrapperFn: () => check('', BAD_OPT),
        addonFn:   () => addon.check('', BAD_OPT),
      },
      {
        // Options are validated before file I/O in both the wrapper and napi.
        name: 'compileFile',
        wrapperFn: () => compileFile('/nonexistent.mds', BAD_OPT),
        addonFn:   () => addon.compileFile('/nonexistent.mds', BAD_OPT),
      },
      {
        name: 'checkFile',
        wrapperFn: () => checkFile('/nonexistent.mds', BAD_OPT),
        addonFn:   () => addon.checkFile('/nonexistent.mds', BAD_OPT),
      },
      {
        name: 'lint',
        wrapperFn: () => lint('', BAD_OPT),
        addonFn:   () => addon.lint('', BAD_OPT),
      },
      {
        // Options are validated before file I/O in napi.
        name: 'lintFile',
        wrapperFn: () => lintFile('/nonexistent.mds', BAD_OPT),
        addonFn:   () => addon.lintFile('/nonexistent.mds', BAD_OPT),
      },
      {
        name: 'lintVirtual',
        wrapperFn: () => lintVirtual(VIRTUAL_MODS, VIRTUAL_ENTRY, BAD_OPT),
        addonFn:   () => addon.lintVirtual(VIRTUAL_MODS, VIRTUAL_ENTRY, BAD_OPT),
      },
    ];

    for (const { name, wrapperFn, addonFn } of cases) {
      const wrapperMsg = await captureMsg(wrapperFn);
      const addonMsg   = await captureMsg(addonFn);

      assert.ok(wrapperMsg.length > 0, `wrapper should have thrown for ${name}`);
      assert.ok(addonMsg.length > 0,   `napi backend should have thrown for ${name}`);
      assert.strictEqual(
        wrapperMsg,
        addonMsg,
        `byte-identical messages required for ${name} — wrapper: "${wrapperMsg}" | napi: "${addonMsg}"`,
      );
    }
  });

  // ── basePath reconciliation: compile and check (issue #72 / user decision) ─

  test('U-OV-15: compile now accepts basePath (reconciled with napi parse_compile_opts — issue #180)', () => {
    // napi's parse_compile_opts has always accepted basePath; the wrapper was wrong to
    // reject it. After the fix, the wrapper no longer intercepts it.
    // Backend-aware per OD-1 — see assertWrapperAcceptsBasePath above.
    assertWrapperAcceptsBasePath(() => compile('Hello\n', { basePath: '.' }), 'U-OV-15');
  });

  test('U-OV-16: check now accepts basePath (reconciled with napi parse_check_opts — issue #180)', () => {
    // napi's parse_check_opts has always accepted basePath; the wrapper was wrong to
    // reject it. After the fix, the wrapper no longer intercepts it.
    // Backend-aware per OD-1 — see assertWrapperAcceptsBasePath above.
    assertWrapperAcceptsBasePath(() => check('Hello\n', { basePath: '.' }), 'U-OV-16');
  });

  // ── basePath passthrough on file methods: purposeful rejection (issue #74) ──
  //
  // U-OV-25 / U-OV-26 replace the now-vacuous U-OV-17 / U-OV-18. The old tests
  // asserted only that the message did NOT start with "unknown option key" — they
  // passed because compileFile/checkFile with basePath succeeded silently (the bug
  // was that basePath was accepted and then dropped). After the fix, the call MUST
  // throw with code 'mds::invalid_options' and a purpose-built message.

  test('U-OV-25: compileFile throws for basePath with a purposeful error (AC-P3-06, AC-P3-07)', () => {
    // basePath guard fires synchronously before any I/O — no real file needed
    // (per U-OV-32 / the note at line 168–177: assert.rejects does NOT intercept
    // synchronous throws in Node v22 and the error escapes with failureType
    // 'testCodeFailure', masking the regression).
    assert.throws(
      () => compileFile('/any.mds', { basePath: '.' }),
      (err) => {
        assert.ok(isMdsError(err), `expected isMdsError, got: ${err}`);
        assert.equal(err.code, 'mds::invalid_options');
        // Must name basePath and state the base is derived from the file path.
        assert.ok(err.message.includes('basePath'), `basePath in message: ${err.message}`);
        assert.ok(err.message.includes('derived from the file path'), `remedy in message: ${err.message}`);
        // Must NOT be the generic unknown-key message (AC-P3-07).
        assert.ok(
          !err.message.startsWith('unknown option key'),
          `must not be generic rejection: "${err.message}"`,
        );
        return true;
      },
    );
  });

  test('U-OV-26: checkFile throws for basePath with a purposeful error (AC-P3-06, AC-P3-07)', () => {
    // Same synchronous-throw contract as U-OV-25 — use assert.throws (not assert.rejects).
    assert.throws(
      () => checkFile('/any.mds', { basePath: '.' }),
      (err) => {
        assert.ok(isMdsError(err), `expected isMdsError, got: ${err}`);
        assert.equal(err.code, 'mds::invalid_options');
        assert.ok(err.message.includes('basePath'), `basePath in message: ${err.message}`);
        assert.ok(err.message.includes('derived from the file path'), `remedy in message: ${err.message}`);
        assert.ok(
          !err.message.startsWith('unknown option key'),
          `must not be generic rejection: "${err.message}"`,
        );
        return true;
      },
    );
  });

  // ── basePath honored on native backend (AC-P3-01 / AC-P3-02 / AC-P3-03) ─────

  // Hard-fail sentinel for tests that require the native addon — a silently-passing
  // skip is exactly how behavioral regressions survive undetected (avoids PF-013).
  function requireNativeAddon() {
    return require('@mdscript/mds-napi');
  }

  // fileURLToPath, NOT URL.pathname: on Windows `new URL('.', import.meta.url).pathname`
  // yields "/C:/..." (leading slash, drive letter) and percent-encodes spaces, so
  // path.join produces a path that cannot be resolved. windows-latest is in the JS CI
  // matrix (ci.yml `js` job), and every other spec in this package already uses
  // fileURLToPath — matching that pattern here (avoids the PF-003 Windows-path class).
  const TEST_DIR = fileURLToPath(new URL('.', import.meta.url));
  const PKG_DIR = fileURLToPath(new URL('..', import.meta.url));
  const FIXTURES = path.join(TEST_DIR, 'fixtures');
  const IMPORT_SRC = '@import { greet } from "./import_provider.mds"\n\n{{greet("World")}}\n';

  test('U-OV-21: native compile honors basePath for import resolution (AC-P3-01)', () => {
    requireNativeAddon(); // hard-fail without addon
    const result = compile(IMPORT_SRC, { basePath: FIXTURES });
    assert.equal(result.kind, 'markdown');
    assert.ok(
      result.output.includes('Hello World!'),
      `expected "Hello World!" in output, got: "${result.output}"`,
    );
    assert.ok(
      result.dependencies.some((d) => d.endsWith('import_provider.mds')),
      `expected import_provider.mds in dependencies: ${result.dependencies.join(', ')}`,
    );
  });

  test('U-OV-22: value-sensitive positive control — wrong basePath throws, right basePath succeeds (AC-P3-02)', () => {
    requireNativeAddon(); // hard-fail without addon
    // A fresh empty directory has no import_provider.mds; the import cannot resolve.
    const empty = fs.mkdtempSync(path.join(os.tmpdir(), 'mds-bp-'));
    try {
      assert.throws(
        () => compile(IMPORT_SRC, { basePath: empty }),
        (err) => {
          assert.ok(isMdsError(err), `expected isMdsError, got: ${err}`);
          return true;
        },
        'compiling with a wrong basePath must throw — proves the VALUE is honored',
      );
    } finally {
      fs.rmSync(empty, { recursive: true, force: true });
    }
    // The correct basePath must succeed (paired with U-OV-21 above).
    const good = compile(IMPORT_SRC, { basePath: FIXTURES });
    assert.equal(good.kind, 'markdown');
  });

  test('U-OV-23: native check honors basePath (AC-P3-03)', () => {
    requireNativeAddon(); // hard-fail without addon
    const empty = fs.mkdtempSync(path.join(os.tmpdir(), 'mds-bp-'));
    try {
      // Correct basePath: check must succeed and return warnings array.
      const result = check(IMPORT_SRC, { basePath: FIXTURES });
      assert.ok(Array.isArray(result.warnings), 'check result must have warnings array');
      // check results carry no dependencies field (napi F-K11).
      assert.equal(
        Object.prototype.hasOwnProperty.call(result, 'dependencies'),
        false,
        'check result must not have dependencies field',
      );
      // Wrong basePath must throw.
      assert.throws(
        () => check(IMPORT_SRC, { basePath: empty }),
        (err) => {
          assert.ok(isMdsError(err), `expected isMdsError, got: ${err}`);
          return true;
        },
        'check with wrong basePath must throw',
      );
    } finally {
      fs.rmSync(empty, { recursive: true, force: true });
    }
  });

  // ── forwarding drift guard: spy addon (AC-P3-04 / AC-P3-05) ──────────────

  test('U-OV-24: all 7 methods forward exactly the accepted keys to the backend (AC-P3-04, AC-P3-05)', async () => {
    // No requireNativeAddon() here: the backend under test is a spy object injected
    // directly into createNativeBackend(), making this test fully environment-independent.
    // Adding requireNativeAddon() would couple the highest-value drift-guard test to
    // native-addon availability, which is the opposite of what a forwarding-parity
    // test should require.
    const { createNativeBackend } = await import('../dist/backend/native.js');

    // Minimal valid result shapes for each assertResultShape kind.
    const compileResult = { kind: 'markdown', output: '', warnings: [], dependencies: [] };
    const checkResult = { warnings: [] };
    const lintResult = { version: 1, files: [], truncated: false };

    // Build a spy addon: each method records the last options argument received.
    let lastOpts;
    function makeResult(shape) { return shape; }
    const spyAddon = {
      compile:     (_src, opts) => { lastOpts = opts; return makeResult(compileResult); },
      check:       (_src, opts) => { lastOpts = opts; return makeResult(checkResult); },
      compileFile: (_p, opts)   => { lastOpts = opts; return Promise.resolve(makeResult(compileResult)); },
      checkFile:   (_p, opts)   => { lastOpts = opts; return Promise.resolve(makeResult(checkResult)); },
      lint:        (_src, opts) => { lastOpts = opts; return makeResult(lintResult); },
      lintFile:    (_p, opts)   => { lastOpts = opts; return Promise.resolve(makeResult(lintResult)); },
      lintVirtual: (_m, _e, opts) => { lastOpts = opts; return makeResult(lintResult); },
    };

    const be = createNativeBackend(spyAddon);

    // A distinguishable test value for each possible option key. Covers every
    // key currently appearing in any method's METHOD_KEYS entry.
    //
    // MAINTENANCE NOTE: if a new option key is added to any option interface,
    // add a non-null entry here — without it, fullOpts() would supply `undefined`
    // for the new key, and forwardOpts would correctly omit it (its != null check
    // skips absent keys), making the deepStrictEqual pass vacuously for that key.
    const KEY_VALUES = {
      basePath: '/some/base/path',
      vars: { k: 1 },
      sourceMap: true,
      sourcesContent: true,
      rules: { 'unused-variable': 'warn' },
    };

    // Build an options object with ALL keys that METHOD_KEYS[method] accepts,
    // each with a distinguishable value. This is the AC-P3-05 positive control:
    // if a key is added to METHOD_KEYS (accepted at validation) but not forwarded
    // by forwardOpts (e.g. accidentally excluded), expected still includes that key
    // so deepStrictEqual catches the drift — reproducing the #180 bug class at the
    // point of introduction rather than at runtime in production (avoids PF-013:
    // a hand-typed expected cannot detect this drift).
    function fullOpts(methodName) {
      const keys = METHOD_KEYS[methodName] ?? [];
      const obj = {};
      for (const k of keys) {
        // Hard-fail if KEY_VALUES is missing an entry for this key. Without this guard,
        // fullOpts would set obj[k] = undefined; forwardOpts would skip it (its != null
        // check); deepStrictEqual would compare {k: undefined} against an object without
        // the key and fail with a confusing message. This assertion surfaces the problem
        // at KEY_VALUES lookup time with a clear diagnosis (avoids PF-013).
        assert.ok(
          Object.prototype.hasOwnProperty.call(KEY_VALUES, k) && KEY_VALUES[k] !== undefined,
          `KEY_VALUES["${k}"] must be a non-null value — add it or the forwarding test for "${methodName}" passes vacuously for that key`,
        );
        obj[k] = KEY_VALUES[k];
      }
      return obj;
    }

    // Independent oracle: verify the lint surface against a hand-typed literal that
    // does NOT pass through fullOpts() or derive from METHOD_KEYS/KEY_VALUES, so
    // that a systematic corruption of KEY_VALUES (all values undefined, or fullOpts
    // returning the wrong shape) is caught by something other than the self-referential
    // deepStrictEqual loop below (avoids PF-013: assertion that cannot fail for the
    // stated reason). Update this literal when METHOD_KEYS.lint or KEY_VALUES changes.
    assert.deepStrictEqual(
      fullOpts('lint'),
      { basePath: '/some/base/path', vars: { k: 1 }, rules: { 'unused-variable': 'warn' } },
      'lint independent oracle mismatch — update this literal if METHOD_KEYS.lint or KEY_VALUES changes',
    );

    const cases = [
      {
        name: 'compile',
        call: () => be.compile('', fullOpts('compile')),
        expected: fullOpts('compile'),
      },
      {
        name: 'check',
        call: () => be.check('', fullOpts('check')),
        expected: fullOpts('check'),
      },
      {
        name: 'compileFile',
        call: () => be.compileFile('/any.mds', fullOpts('compileFile')),
        expected: fullOpts('compileFile'),
      },
      {
        name: 'checkFile',
        call: () => be.checkFile('/any.mds', fullOpts('checkFile')),
        expected: fullOpts('checkFile'),
      },
      {
        name: 'lint',
        call: () => be.lint('', fullOpts('lint')),
        expected: fullOpts('lint'),
      },
      {
        name: 'lintFile',
        call: () => be.lintFile('/any.mds', fullOpts('lintFile')),
        expected: fullOpts('lintFile'),
      },
      {
        name: 'lintVirtual',
        call: () => be.lintVirtual({ 'a.mds': '' }, 'a.mds', fullOpts('lintVirtual')),
        expected: fullOpts('lintVirtual'),
      },
    ];

    for (const { name, call, expected } of cases) {
      lastOpts = undefined;
      await call();
      assert.deepStrictEqual(
        lastOpts,
        expected,
        `${name}: forwarded options must be deep-equal to the expected key subset`,
      );
    }
  });

  // ── cross-backend message equality for file basePath (AC-P3-06 / U-OV-27) ─

  test('U-OV-27: basePath rejection message is byte-identical to napi and WASM for all four file-surface methods (AC-P3-06, avoids PF-007)', async () => {
    const addon = requireNativeAddon(); // hard-fail without addon

    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'mds-bp-'));
    const file = path.join(tmp, 'ok.mds');
    fs.writeFileSync(file, 'Hello\n', 'utf8');

    async function captureMsg(fn) {
      try { await fn(); } catch (e) { return e instanceof Error ? e.message : String(e); }
      return '';
    }

    const { execFileSync } = await import('node:child_process');
    const VIRTUAL_MODS = { 'a.mds': '' };
    const VIRTUAL_ENTRY = 'a.mds';

    // Per-method cases: wrapperFn, addonFn, and wasmScript cover four surfaces.
    // lintFile and lintVirtual guards fire synchronously (before assertReady()),
    // so no init() is needed in the subprocess for any of these cases.
    const cases = [
      {
        name: 'compileFile',
        wrapperFn: () => compileFile(file, { basePath: '.' }),
        addonFn:   () => addon.compileFile(file, { basePath: '.' }),
        wasmScript: [
          `import { compileFile } from './dist/node.js';`,
          `try {`,
          `  compileFile(${JSON.stringify(file)}, { basePath: '.' });`,
          `  process.stdout.write('');`,
          `} catch (e) {`,
          `  process.stdout.write(e instanceof Error ? e.message : String(e));`,
          `}`,
        ].join('\n'),
      },
      {
        name: 'checkFile',
        wrapperFn: () => checkFile(file, { basePath: '.' }),
        addonFn:   () => addon.checkFile(file, { basePath: '.' }),
        wasmScript: [
          `import { checkFile } from './dist/node.js';`,
          `try {`,
          `  checkFile(${JSON.stringify(file)}, { basePath: '.' });`,
          `  process.stdout.write('');`,
          `} catch (e) {`,
          `  process.stdout.write(e instanceof Error ? e.message : String(e));`,
          `}`,
        ].join('\n'),
      },
      {
        name: 'lintFile',
        wrapperFn: () => lintFile(file, { basePath: '.' }),
        addonFn:   () => addon.lintFile(file, { basePath: '.' }),
        wasmScript: [
          `import { lintFile } from './dist/node.js';`,
          `try {`,
          `  lintFile(${JSON.stringify(file)}, { basePath: '.' });`,
          `  process.stdout.write('');`,
          `} catch (e) {`,
          `  process.stdout.write(e instanceof Error ? e.message : String(e));`,
          `}`,
        ].join('\n'),
      },
      {
        name: 'lintVirtual',
        wrapperFn: () => lintVirtual(VIRTUAL_MODS, VIRTUAL_ENTRY, { basePath: '.' }),
        addonFn:   () => addon.lintVirtual(VIRTUAL_MODS, VIRTUAL_ENTRY, { basePath: '.' }),
        wasmScript: [
          `import { lintVirtual } from './dist/node.js';`,
          `const m = ${JSON.stringify(VIRTUAL_MODS)};`,
          `try {`,
          `  lintVirtual(m, ${JSON.stringify(VIRTUAL_ENTRY)}, { basePath: '.' });`,
          `  process.stdout.write('');`,
          `} catch (e) {`,
          `  process.stdout.write(e instanceof Error ? e.message : String(e));`,
          `}`,
        ].join('\n'),
      },
    ];

    try {
      for (const { name, wrapperFn, addonFn, wasmScript } of cases) {
        // LEG 1 (wrapper vs napi — canonical drift guard): compare the wrapper's
        // purpose-built rejection against the RAW napi addon's message.
        // The wrapper fires getBasePathError() BEFORE assertReady()/backend dispatch,
        // so napi never receives this input through the public wrapper.
        // Without this leg, editing either string silently diverges the two surfaces.
        const wrapperMsg = await captureMsg(wrapperFn);
        const napiMsg    = await captureMsg(addonFn);

        assert.ok(wrapperMsg.length > 0, `wrapper ${name} must throw for basePath`);
        assert.ok(napiMsg.length > 0,    `napi ${name} must throw for basePath`);
        assert.strictEqual(
          wrapperMsg,
          napiMsg,
          `wrapper must match napi byte-for-byte for ${name} — wrapper: "${wrapperMsg}" | napi: "${napiMsg}"`,
        );

        // LEG 2 (WASM subprocess — regression barrier per AC-P3-06 / PF-004):
        // Run under MDS_BACKEND=wasm and assert (a) the throw still occurs and
        // (b) the message is byte-identical to the wrapper. The guard fires before
        // assertReady(), so MDS_BACKEND has no effect on this input today —
        // making the byte-equality assertion technically tautological with LEG 1.
        // But that tautology IS the regression barrier: if the guard is ever
        // refactored into the backend layer, a WASM-specific divergence is caught
        // here instead of silently reaching production (avoids PF-004).
        let wasmMsg = '';
        try {
          wasmMsg = execFileSync(process.execPath, ['--input-type=module'], {
            input: wasmScript,
            cwd: PKG_DIR,
            env: { ...process.env, MDS_BACKEND: 'wasm' },
            timeout: 15000,
            encoding: 'utf8',
          });
        } catch (e) {
          // execFileSync throws on non-zero exit. The subprocess catches all errors
          // and writes to stdout before exiting 0, so this branch handles unexpected
          // subprocess failures only — capture stdout if available.
          wasmMsg = (typeof e === 'object' && e !== null && typeof e.stdout === 'string') ? e.stdout : '';
        }
        assert.ok(wasmMsg.length > 0, `WASM subprocess ${name} must throw for basePath`);
        assert.strictEqual(
          wrapperMsg,
          wasmMsg,
          `WASM path must match wrapper byte-for-byte for ${name} — wrapper: "${wrapperMsg}" | wasm: "${wasmMsg}"`,
        );
      }
    } finally {
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  });

  // ── {basePath: null} wrapper-vs-napi parity (consistency-19 / U-OV-36) ──────

  test('U-OV-36: {basePath: null} is rejected with purpose-built error byte-identical to napi for all file-surface methods (consistency-19)', async () => {
    // Hard-fail without addon — a silently-passing skip is exactly how parity
    // regressions survive undetected (avoids PF-007).
    const addon = requireNativeAddon();

    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'mds-bp-null-'));
    const file = path.join(tmp, 'ok.mds');
    fs.writeFileSync(file, 'Hello\n', 'utf8');

    async function captureMsg(fn) {
      try { await fn(); } catch (e) { return e instanceof Error ? e.message : String(e); }
      return '';
    }

    const VIRTUAL_MODS = { 'a.mds': '' };
    const VIRTUAL_ENTRY = 'a.mds';

    try {
      // All four file-surface methods must reject {basePath: null} with the same
      // purpose-built message as the raw napi addon (consistency-19: napi's
      // has_named_property fires for null, so the wrapper must also fire).
      const cases = [
        {
          name: 'compileFile',
          wrapperFn: () => compileFile(file, { basePath: null }),
          addonFn:   () => addon.compileFile(file, { basePath: null }),
        },
        {
          name: 'checkFile',
          wrapperFn: () => checkFile(file, { basePath: null }),
          addonFn:   () => addon.checkFile(file, { basePath: null }),
        },
        {
          name: 'lintFile',
          wrapperFn: () => lintFile(file, { basePath: null }),
          addonFn:   () => addon.lintFile(file, { basePath: null }),
        },
        {
          name: 'lintVirtual',
          wrapperFn: () => lintVirtual(VIRTUAL_MODS, VIRTUAL_ENTRY, { basePath: null }),
          addonFn:   () => addon.lintVirtual(VIRTUAL_MODS, VIRTUAL_ENTRY, { basePath: null }),
        },
      ];

      for (const { name, wrapperFn, addonFn } of cases) {
        const wrapperMsg = await captureMsg(wrapperFn);
        const napiMsg    = await captureMsg(addonFn);

        assert.ok(wrapperMsg.length > 0, `wrapper ${name} must throw for {basePath: null}`);
        assert.ok(napiMsg.length > 0,    `napi ${name} must throw for {basePath: null}`);
        // Must be the purpose-built message, not the generic unknown-key form.
        assert.ok(
          !wrapperMsg.startsWith('unknown option key'),
          `${name}: {basePath: null} must not emit generic unknown-key message: "${wrapperMsg}"`,
        );
        assert.strictEqual(
          wrapperMsg,
          napiMsg,
          `wrapper must match napi byte-for-byte for ${name} {basePath: null} — ` +
          `wrapper: "${wrapperMsg}" | napi: "${napiMsg}"`,
        );
      }
    } finally {
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  });

  // ── {basePath: undefined} cross-backend parity (AC-P3-08 / U-OV-29) ────────

  test('U-OV-29: {basePath: undefined} has consistent throw/no-throw on both backends for compile/check/compileFile/checkFile (AC-P3-08)', async () => {
    requireNativeAddon(); // hard-fail without addon

    // Semantic: basePath: undefined is treated as absent ("value is intent") on the
    // wrapper side. The WASM guard checks != null so undefined passes through.
    // On native, the per-surface builders drop the undefined value entirely, so napi's
    // has_named_property("basePath") gate never sees the key. Both backends must agree.
    //
    // AC-P3-08 requires all FOUR methods: the file surfaces are the interesting half,
    // because napi keys off property PRESENCE — if a builder ever forwarded
    // `basePath: undefined` verbatim, native would throw while WASM would not.
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'mds-bp-undef-'));
    const file = path.join(tmp, 'ok.mds');
    fs.writeFileSync(file, 'Hello\n', 'utf8');

    const methods = [
      { name: 'compile',     call: 'compile(SRC, OPTS)',        fn: (opts) => compile('Hello\n', opts) },
      { name: 'check',       call: 'check(SRC, OPTS)',          fn: (opts) => check('Hello\n', opts) },
      { name: 'compileFile', call: 'await compileFile(F, OPTS)', fn: (opts) => compileFile(file, opts) },
      { name: 'checkFile',   call: 'await checkFile(F, OPTS)',   fn: (opts) => checkFile(file, opts) },
    ];

    try {
      for (const { name, call, fn } of methods) {
        let nativeThrew = false;
        try { await fn({ basePath: undefined }); } catch { nativeThrew = true; }

        // WASM path via subprocess. init() must be awaited before any method.
        // The subprocess emits getBackend() to stdout so the parent can verify the
        // WASM backend was actually selected — MDS_BACKEND=wasm is advisory and
        // node.ts only console.warns on unrecognised values, so a drift in the
        // env-var name or accepted values would silently fall back to native.
        // Capturing and asserting the backend string here guards against that.
        const { execFileSync } = await import('node:child_process');
        let wasmThrew = false;
        let wasmBackendStr = '';
        const script = [
          `import { init, getBackend, ${name} } from './dist/node.js';`,
          `const SRC = 'Hello\\n';`,
          `const F = ${JSON.stringify(file)};`,
          `const OPTS = { basePath: undefined };`,
          `await init();`,
          `process.stdout.write(getBackend());`,
          `try { ${call}; process.exit(0); } catch { process.exit(1); }`,
        ].join('\n');
        try {
          wasmBackendStr = execFileSync(process.execPath, ['--input-type=module'], {
            input: script,
            cwd: PKG_DIR,
            env: { ...process.env, MDS_BACKEND: 'wasm' },
            timeout: 15000,
            encoding: 'utf8',
          });
        } catch (e) {
          wasmThrew = true;
          // stdout is populated even when exit code is non-zero (the method threw)
          wasmBackendStr = typeof e.stdout === 'string' ? e.stdout : '';
        }

        assert.strictEqual(
          wasmBackendStr,
          'wasm',
          `${name}: WASM subprocess must use the wasm backend (getBackend() returned ` +
            `${JSON.stringify(wasmBackendStr)}) — MDS_BACKEND=wasm may be unrecognised`,
        );
        assert.strictEqual(
          nativeThrew,
          wasmThrew,
          `${name}: native (threw=${nativeThrew}) and WASM (threw=${wasmThrew}) must agree on {basePath: undefined}`,
        );
      }
    } finally {
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  });

  // ── native lint still honors basePath (AC-P3-11 / U-OV-30) ──────────────

  test('U-OV-30: native lint still honors basePath — no regression from WASM guard (AC-P3-11)', () => {
    requireNativeAddon(); // hard-fail without addon
    // Positive: lint with correct basePath must NOT throw.
    const result = lint(IMPORT_SRC, { basePath: FIXTURES });
    assert.equal(result.version, 1);
    assert.ok(Array.isArray(result.files));
    assert.equal(result.truncated, false);

    // Positive control: wrong basePath causes a throw, proving basePath is operative.
    const empty = fs.mkdtempSync(path.join(os.tmpdir(), 'mds-bp-'));
    try {
      assert.throws(
        () => lint(IMPORT_SRC, { basePath: empty }),
        (err) => {
          assert.ok(isMdsError(err), `expected isMdsError for wrong basePath: ${err}`);
          return true;
        },
        'wrong basePath must throw — proves the value is honored, not just the key',
      );
    } finally {
      fs.rmSync(empty, { recursive: true, force: true });
    }
  });

  // ── plural-form message parity (AC-P3-17 / U-OV-31) ─────────────────────

  test('U-OV-31: wrapper error message is byte-identical to napi for multiple-unknown-key form, all 7 methods (AC-P3-17)', async () => {
    // Hard-fail without addon — same rationale as U-OV-14 (avoids PF-013).
    const addon = require('@mdscript/mds-napi');

    // Two unknown keys: triggers the plural "unknown option keys:" form.
    const BAD_OPT = { sourceMaps: true, varsJson: '{}' };
    const VIRTUAL_MODS = { 'a.mds': '' };
    const VIRTUAL_ENTRY = 'a.mds';

    async function captureMsg(fn) {
      try {
        const r = fn();
        if (r != null && typeof r === 'object' && typeof r.then === 'function') await r;
      } catch (e) { return e instanceof Error ? e.message : String(e); }
      return '';
    }

    const cases = [
      { name: 'compile',     wrapperFn: () => compile('', BAD_OPT),                               addonFn: () => addon.compile('', BAD_OPT) },
      { name: 'check',       wrapperFn: () => check('', BAD_OPT),                                 addonFn: () => addon.check('', BAD_OPT) },
      { name: 'compileFile', wrapperFn: () => compileFile('/nonexistent.mds', BAD_OPT),            addonFn: () => addon.compileFile('/nonexistent.mds', BAD_OPT) },
      { name: 'checkFile',   wrapperFn: () => checkFile('/nonexistent.mds', BAD_OPT),              addonFn: () => addon.checkFile('/nonexistent.mds', BAD_OPT) },
      { name: 'lint',        wrapperFn: () => lint('', BAD_OPT),                                   addonFn: () => addon.lint('', BAD_OPT) },
      { name: 'lintFile',    wrapperFn: () => lintFile('/nonexistent.mds', BAD_OPT),               addonFn: () => addon.lintFile('/nonexistent.mds', BAD_OPT) },
      { name: 'lintVirtual', wrapperFn: () => lintVirtual(VIRTUAL_MODS, VIRTUAL_ENTRY, BAD_OPT),  addonFn: () => addon.lintVirtual(VIRTUAL_MODS, VIRTUAL_ENTRY, BAD_OPT) },
    ];

    for (const { name, wrapperFn, addonFn } of cases) {
      const wrapperMsg = await captureMsg(wrapperFn);
      const addonMsg   = await captureMsg(addonFn);
      assert.ok(wrapperMsg.length > 0, `wrapper must throw for ${name} (positive control)`);
      assert.ok(addonMsg.length > 0,   `napi must throw for ${name} (positive control)`);
      assert.strictEqual(
        wrapperMsg,
        addonMsg,
        `plural form must be byte-identical for ${name} — wrapper: "${wrapperMsg}" | napi: "${addonMsg}"`,
      );
    }
  });

  // ── prototype-chain safety (issue #18 regression prevention) ─────────────

  test('U-OV-19: prototype-chain method names are handled without TypeError (issue #18)', () => {
    // The MethodName literal union catches these at compile time. The hasOwnProperty
    // guard in assertKnownKeys prevents a TypeError at runtime when the union is bypassed
    // via a cast. Verified by calling assertKnownKeys directly in JavaScript (no TS
    // type enforcement).
    for (const badMethod of ['toString', 'constructor', '__proto__', 'valueOf', 'hasOwnProperty']) {
      assert.doesNotThrow(
        () => assertKnownKeys({}, badMethod),
        `assertKnownKeys({}, '${badMethod}') must not throw TypeError`,
      );
    }
  });

  test('U-OV-20: prototype-chain option keys are handled correctly (issue #18)', () => {
    // 'toString' and 'constructor' ARE own enumerable properties when set via object
    // literal syntax — Object.keys returns them, and they are correctly rejected as
    // unknown keys (mds::invalid_options), not TypeError.
    assert.throws(
      () => compile('Hello\n', { toString: 'x' }),
      (err) => {
        assert.ok(isMdsError(err), `expected isMdsError for "toString" key, got: ${err}`);
        assert.equal(err.code, 'mds::invalid_options');
        assert.ok(err.message.includes('"toString"'), `key name in message: ${err.message}`);
        return true;
      },
      '"toString" as option key must be rejected with invalid_options, not TypeError',
    );

    assert.throws(
      () => compile('Hello\n', { constructor: 'x' }),
      (err) => {
        assert.ok(isMdsError(err), `expected isMdsError for "constructor" key, got: ${err}`);
        assert.equal(err.code, 'mds::invalid_options');
        assert.ok(err.message.includes('"constructor"'), `key name in message: ${err.message}`);
        return true;
      },
      '"constructor" as option key must be rejected with invalid_options, not TypeError',
    );

    // '__proto__' in an object literal sets the prototype, not an own enumerable property,
    // so Object.keys returns [] and no unknown-key error fires. No crash.
    assert.doesNotThrow(
      () => compile('Hello\n', { __proto__: 'x' }),
      '__proto__ in object literal is not an own enumerable key; no invalid_options error should fire',
    );
  });

  // ── basePath throws synchronously on file methods (single error channel) ──
  //
  // assertKnownKeys (unknown-key errors) throws synchronously for ALL methods.
  // The basePath guard on compileFile/checkFile must use the SAME channel so that
  // `try { compileFile(f, opts) } catch` captures both error classes.
  // Using assert.throws (not assert.rejects) is intentional: a regression to
  // Promise.reject would escape the catch and surface as an unhandled rejection,
  // failing the test with a process warning rather than a clean assertion failure.

  test('U-OV-32: compileFile throws synchronously for basePath (same channel as assertKnownKeys)', () => {
    // Fires before assertReady(), so no real file or init() is needed.
    assert.throws(
      () => compileFile('/any.mds', { basePath: '.' }),
      (err) => {
        assert.ok(isMdsError(err), `expected isMdsError, got: ${err}`);
        assert.equal(err.code, 'mds::invalid_options');
        assert.ok(err.message.includes('basePath'), `basePath in message: ${err.message}`);
        assert.ok(
          !err.message.startsWith('unknown option key'),
          `must not be generic rejection: "${err.message}"`,
        );
        return true;
      },
      'compileFile must throw synchronously for basePath — not return a rejected promise',
    );
  });

  test('U-OV-33: checkFile throws synchronously for basePath (same channel as assertKnownKeys)', () => {
    // Fires before assertReady(), so no real file or init() is needed.
    assert.throws(
      () => checkFile('/any.mds', { basePath: '.' }),
      (err) => {
        assert.ok(isMdsError(err), `expected isMdsError, got: ${err}`);
        assert.equal(err.code, 'mds::invalid_options');
        assert.ok(err.message.includes('basePath'), `basePath in message: ${err.message}`);
        assert.ok(
          !err.message.startsWith('unknown option key'),
          `must not be generic rejection: "${err.message}"`,
        );
        return true;
      },
      'checkFile must throw synchronously for basePath — not return a rejected promise',
    );
  });

  // ── compile-options builder fast path (AC-P3-22 second half) ────────────────
  //
  // Test-plan item 17: "a unit assertion that the compile-options builder returns
  // undefined (strictly, not {}) … assert compileOpts(undefined) === compileOpts({})
  // by object identity". The WASM backend's compileOpts() fast path at wasm.ts:363-365
  // fires only when forwardOpts (called internally by compileOpts) returns undefined.
  // If forwardOpts returned {} for empty input, every compile call would allocate a
  // new object rather than reusing DEFAULT_COMPILE_OPTS — the regression would be
  // invisible to U-PF1/U-PF2 (too much headroom) and no other test pins this
  // invariant (avoids PF-013: absence of an allocation test cannot detect a silent
  // allocation regression).

  test('U-OV-34: forwardOpts returns undefined for empty compile input, and the WASM backend reuses DEFAULT_COMPILE_OPTS by identity (AC-P3-22)', async () => {
    // Part 1: forwardOpts returns undefined (strictly) for empty input — {} would
    // allocate a new object and defeat the WASM DEFAULT_COMPILE_OPTS identity fast path.
    assert.strictEqual(forwardOpts(undefined, 'compile'), undefined,
      'forwardOpts(undefined, compile) must be undefined — {} would defeat the WASM fast path');
    assert.strictEqual(forwardOpts({}, 'compile'), undefined,
      'forwardOpts({}, compile) must be undefined — {} would defeat the WASM fast path');
    assert.strictEqual(forwardOpts(null, 'compile'), undefined,
      'forwardOpts(null, compile) must be undefined');

    // Part 2: object-identity assertion through a WASM spy. compileOpts() inside
    // the WASM backend must return the SAME DEFAULT_COMPILE_OPTS reference for both
    // undefined and {} options, proving the fast-path branch at wasm.ts:363-365 fires.
    // We test this through a spy wasmModule rather than exporting the private
    // compileOpts / DEFAULT_COMPILE_OPTS symbols from wasm.ts.
    const { createWasmBackend } = await import('../dist/backend/wasm.js');
    let capturedWasmOpts;
    const spyWasmModule = {
      compile:     (_src, opts) => { capturedWasmOpts = opts; return { kind: 'markdown', output: '', warnings: [], dependencies: [] }; },
      check:       (_src, _opts) => ({ warnings: [] }),
      lint:        (_src, _opts) => ({ version: 1, files: [], truncated: false }),
      lintVirtual: (_m, _e, _opts) => ({ version: 1, files: [], truncated: false }),
      scanImports: (_src) => [],
    };
    const wasmBe = createWasmBackend(spyWasmModule);

    wasmBe.compile('', undefined);
    const optsForUndefined = capturedWasmOpts;

    wasmBe.compile('', {});
    const optsForEmpty = capturedWasmOpts;

    assert.strictEqual(
      optsForUndefined,
      optsForEmpty,
      'compileOpts(undefined) and compileOpts({}) must return the same DEFAULT_COMPILE_OPTS object ' +
      '— identity fast path at wasm.ts:363-365 must fire for both inputs',
    );

    // Strengthen: the identity assertion proves A singleton is reused but not WHICH
    // singleton. Assert the documented default shape so a fast path that reuses the
    // wrong constant (any frozen object other than DEFAULT_COMPILE_OPTS) is still
    // caught. AC-P3-22: 'the compile-options builder MUST return undefined (not {})
    // when no option is set' — the shape check locks in that the singleton is the
    // documented default, not an incidental constant (avoids PF-013).
    assert.deepStrictEqual(
      optsForUndefined,
      { filename: 'input.mds', modules: {} },
      'DEFAULT_COMPILE_OPTS must have shape { filename: "input.mds", modules: {} }',
    );
  });

  // ── checkFile WASM-path forwarding invariant (avoids PF-004 / #180 bug class) ──

  test('U-OV-35: METHOD_KEYS.checkFile is a subset of METHOD_KEYS.compileFile (WASM checkFile forwarding invariant)', () => {
    // On the WASM backend, node.ts checkFile routes through prepareFileArgs → fileOpts,
    // which forwards using METHOD_KEYS.compileFile — NOT METHOD_KEYS.checkFile
    // (see the cast comment at node.ts:114-117). That is correct only while every
    // checkFile key is also a compileFile key.
    //
    // If CheckFileOptions ever gains a key that FileOptions lacks, assertKnownKeys
    // would ACCEPT it on checkFile while the WASM path silently DROPPED it —
    // reintroducing the exact #180 bug class (validated-then-discarded) on one backend
    // only. The native path is unaffected, so a native-only test suite would not catch
    // it. This assertion converts that silent drift into a loud failure (avoids PF-004:
    // an alternate code path silently bypassing the authoritative key list).
    const missing = METHOD_KEYS.checkFile.filter((k) => !METHOD_KEYS.compileFile.includes(k));
    assert.deepStrictEqual(
      missing,
      [],
      `METHOD_KEYS.checkFile keys absent from METHOD_KEYS.compileFile: [${missing.join(', ')}]. ` +
        'Either add them to FileOptions, or give checkFile its own forwarding path in ' +
        'node.ts prepareFileArgs/fileOpts — otherwise the WASM backend drops them silently.',
    );

    // Positive controls (avoids PF-013): an empty-vs-empty comparison passes vacuously.
    // Prove both operands are non-empty and that the filter can actually report a key
    // as missing, so the assertion above is capable of failing.
    assert.ok(METHOD_KEYS.checkFile.length > 0, 'METHOD_KEYS.checkFile must be non-empty');
    assert.ok(METHOD_KEYS.compileFile.length > 0, 'METHOD_KEYS.compileFile must be non-empty');
    assert.deepStrictEqual(
      METHOD_KEYS.checkFile.filter((k) => !['__no_such_key__'].includes(k)),
      [...METHOD_KEYS.checkFile],
      'positive control: the subset filter must be able to report keys as missing',
    );
  });
});
