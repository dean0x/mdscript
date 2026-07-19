/**
 * Options-validation tests — assertKnownKeys wrapper-level enforcement.
 * Tests: U-OV-1 through U-OV-20
 *
 * Verifies that the universal @mdscript/mds wrapper rejects unknown option keys
 * with code === 'mds::invalid_options' before dispatching to any backend.
 * Since validation runs inside the wrapper (before backend dispatch) it is
 * backend-agnostic: the same rejection fires on native and WASM paths.
 *
 * U-OV-14 performs a byte-identical message parity check across all seven
 * methods against the native napi backend. That test hard-fails when the
 * native addon is absent — a silently-passing skip is how parity regressions
 * survive undetected (avoids PF-007).
 */
import { test, describe, before } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
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
} from '../dist/node.js';
import { assertKnownKeys } from '../dist/util/options.js';
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

  test('U-OV-8: compile accepts all valid keys without error', () => {
    assert.doesNotThrow(() => compile('Hello\n', { vars: {}, sourceMap: false, sourcesContent: false }));
    // basePath is now also accepted (reconciled with napi parse_compile_opts — issue #180)
    assert.doesNotThrow(() => compile('Hello\n', { basePath: '.' }));
  });

  test('U-OV-9: check accepts vars and basePath without error', () => {
    assert.doesNotThrow(() => check('Hello\n', { vars: {} }));
    // basePath is now also accepted (reconciled with napi parse_check_opts — issue #180)
    assert.doesNotThrow(() => check('Hello\n', { basePath: '.' }));
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
    assert.doesNotThrow(
      () => compile('Hello\n', { basePath: '.' }),
      'compile must not throw invalid_options for basePath after wrapper reconciliation',
    );
  });

  test('U-OV-16: check now accepts basePath (reconciled with napi parse_check_opts — issue #180)', () => {
    // napi's parse_check_opts has always accepted basePath; the wrapper was wrong to
    // reject it. After the fix, the wrapper no longer intercepts it.
    assert.doesNotThrow(
      () => check('Hello\n', { basePath: '.' }),
      'check must not throw invalid_options for basePath after wrapper reconciliation',
    );
  });

  // ── basePath passthrough on file methods (issue #74) ──────────────────────

  test('U-OV-17: compileFile does not intercept basePath with a generic unknown-key message (issue #74)', async () => {
    // Before the fix, the wrapper emitted "unknown option key 'basePath'; recognised
    // keys are: vars, sourceMap, sourcesContent", masking the backend's purpose-built
    // "not valid for compileFile/checkFile" message. After the fix, basePath is passed
    // through without wrapper interception.
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'mds-test-'));
    const file = path.join(tmp, 'ok.mds');
    fs.writeFileSync(file, 'Hello\n', 'utf8');
    try {
      let errorMsg = '';
      try {
        await compileFile(file, { basePath: '.' });
      } catch (e) {
        errorMsg = e instanceof Error ? e.message : String(e);
      }
      assert.ok(
        !errorMsg.startsWith('unknown option key "basePath"'),
        `wrapper must not intercept basePath for compileFile with a generic message; got: "${errorMsg}"`,
      );
    } finally {
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  });

  test('U-OV-18: checkFile does not intercept basePath with a generic unknown-key message (issue #74)', async () => {
    // Same as U-OV-17 but for checkFile.
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'mds-test-'));
    const file = path.join(tmp, 'ok.mds');
    fs.writeFileSync(file, 'Hello\n', 'utf8');
    try {
      let errorMsg = '';
      try {
        await checkFile(file, { basePath: '.' });
      } catch (e) {
        errorMsg = e instanceof Error ? e.message : String(e);
      }
      assert.ok(
        !errorMsg.startsWith('unknown option key "basePath"'),
        `wrapper must not intercept basePath for checkFile with a generic message; got: "${errorMsg}"`,
      );
    } finally {
      fs.rmSync(tmp, { recursive: true, force: true });
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
});
