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

  // ── basePath passthrough on file methods: purposeful rejection (issue #74) ──
  //
  // U-OV-25 / U-OV-26 replace the now-vacuous U-OV-17 / U-OV-18. The old tests
  // asserted only that the message did NOT start with "unknown option key" — they
  // passed because compileFile/checkFile with basePath succeeded silently (the bug
  // was that basePath was accepted and then dropped). After the fix, the call MUST
  // throw with code 'mds::invalid_options' and a purpose-built message.

  test('U-OV-25: compileFile rejects basePath with a purposeful error (AC-P3-06, AC-P3-07)', async () => {
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'mds-test-'));
    const file = path.join(tmp, 'ok.mds');
    fs.writeFileSync(file, 'Hello\n', 'utf8');
    try {
      await assert.rejects(
        () => compileFile(file, { basePath: '.' }),
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
    } finally {
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  });

  test('U-OV-26: checkFile rejects basePath with a purposeful error (AC-P3-06, AC-P3-07)', async () => {
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'mds-test-'));
    const file = path.join(tmp, 'ok.mds');
    fs.writeFileSync(file, 'Hello\n', 'utf8');
    try {
      await assert.rejects(
        () => checkFile(file, { basePath: '.' }),
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
    } finally {
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  });

  // ── basePath honored on native backend (AC-P3-01 / AC-P3-02 / AC-P3-03) ─────

  // Hard-fail sentinel for tests that require the native addon — a silently-passing
  // skip is exactly how behavioral regressions survive undetected (avoids PF-013).
  function requireNativeAddon() {
    return require('@mdscript/mds-napi');
  }

  const FIXTURES = path.join(new URL('.', import.meta.url).pathname, 'fixtures');
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
    requireNativeAddon(); // hard-fail without addon
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

    const VARS = { k: 1 };
    const RULES = { 'unused-variable': 'warn' };
    const BP = '/some/base/path';

    const cases = [
      {
        name: 'compile',
        call: () => be.compile('', { basePath: BP, vars: VARS, sourceMap: true, sourcesContent: true }),
        expected: { basePath: BP, vars: VARS, sourceMap: true, sourcesContent: true },
      },
      {
        name: 'check',
        call: () => be.check('', { basePath: BP, vars: VARS }),
        expected: { basePath: BP, vars: VARS },
      },
      {
        name: 'compileFile',
        call: () => be.compileFile('/any.mds', { vars: VARS, sourceMap: true, sourcesContent: true }),
        expected: { vars: VARS, sourceMap: true, sourcesContent: true },
      },
      {
        name: 'checkFile',
        call: () => be.checkFile('/any.mds', { vars: VARS }),
        expected: { vars: VARS },
      },
      {
        name: 'lint',
        call: () => be.lint('', { basePath: BP, vars: VARS, rules: RULES }),
        expected: { basePath: BP, vars: VARS, rules: RULES },
      },
      {
        name: 'lintFile',
        call: () => be.lintFile('/any.mds', { vars: VARS, rules: RULES }),
        expected: { vars: VARS, rules: RULES },
      },
      {
        name: 'lintVirtual',
        call: () => be.lintVirtual({ 'a.mds': '' }, 'a.mds', { vars: VARS, rules: RULES }),
        expected: { vars: VARS, rules: RULES },
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

  test('U-OV-27: compileFile/checkFile basePath rejection message is byte-identical on native and WASM (AC-P3-06, avoids PF-007)', async () => {
    requireNativeAddon(); // hard-fail without addon

    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'mds-bp-'));
    const file = path.join(tmp, 'ok.mds');
    fs.writeFileSync(file, 'Hello\n', 'utf8');

    async function captureMsg(fn) {
      try { await fn(); } catch (e) { return e instanceof Error ? e.message : String(e); }
      return '';
    }

    try {
      for (const method of ['compileFile', 'checkFile']) {
        // Native path: comes through the napi addon.
        const nativeMsg = await captureMsg(
          () => (method === 'compileFile' ? compileFile : checkFile)(file, { basePath: '.' }),
        );
        // WASM path: set MDS_BACKEND=wasm via env then run in subprocess — or
        // drive the WASM path directly via wrapWithFileOps. We use a subprocess
        // for full isolation (no shared module singleton).
        const { execFileSync } = await import('node:child_process');
        const wasmMsg = await captureMsg(() => {
          const script = `
import { ${method} } from './dist/node.js';
${method}(${JSON.stringify(file)}, { basePath: '.' }).catch(e => {
  process.stdout.write(e.message);
  process.exit(0);
}).then(r => { if (r !== undefined) process.exit(0); });
`;
          const out = execFileSync(process.execPath, ['--input-type=module'], {
            input: script,
            cwd: new URL('..', import.meta.url).pathname,
            env: { ...process.env, MDS_BACKEND: 'wasm' },
            timeout: 15000,
          });
          throw new Error(out.toString().trim() || 'WASM subprocess produced no output');
        });

        assert.ok(nativeMsg.length > 0, `native ${method} must throw for basePath`);
        assert.ok(wasmMsg.length > 0, `WASM ${method} must throw for basePath`);
        assert.strictEqual(
          nativeMsg,
          wasmMsg,
          `byte-identical messages required for ${method} — native: "${nativeMsg}" | wasm: "${wasmMsg}"`,
        );
      }
    } finally {
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  });

  // ── {basePath: undefined} cross-backend parity (AC-P3-08 / U-OV-29) ────────

  test('U-OV-29: {basePath: undefined} has consistent throw/no-throw on both backends for compile/check (AC-P3-08)', async () => {
    requireNativeAddon(); // hard-fail without addon

    // Semantic: basePath: undefined is treated as absent ("value is intent") on the
    // wrapper side. The WASM guard checks != null so undefined passes through.
    // On native, napi sees the property but its value is undefined/null and does not
    // trigger basePath handling. Both backends must agree.
    const methods = [
      { name: 'compile', fn: (opts) => compile('Hello\n', opts) },
      { name: 'check',   fn: (opts) => check('Hello\n', opts) },
    ];

    for (const { name, fn } of methods) {
      let nativeThrew = false;
      try { fn({ basePath: undefined }); } catch { nativeThrew = true; }

      // WASM path via subprocess. init() must be awaited before compile/check.
      const { execFileSync } = await import('node:child_process');
      let wasmThrew = false;
      try {
        execFileSync(process.execPath, ['--input-type=module'], {
          input: `import { init, ${name} } from './dist/node.js'; await init(); try { ${name}('Hello\\n', { basePath: undefined }); process.exit(0); } catch { process.exit(1); }`,
          cwd: new URL('..', import.meta.url).pathname,
          env: { ...process.env, MDS_BACKEND: 'wasm' },
          timeout: 15000,
        });
      } catch {
        wasmThrew = true;
      }

      assert.strictEqual(
        nativeThrew,
        wasmThrew,
        `${name}: native (threw=${nativeThrew}) and WASM (threw=${wasmThrew}) must agree on {basePath: undefined}`,
      );
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
});
