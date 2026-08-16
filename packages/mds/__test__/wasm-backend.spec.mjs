/**
 * WASM backend unit tests for @mdscript/mds universal package.
 * Tests: U-WB1 through U-WB20
 *
 * Imports dist/backend/wasm.js directly to exercise internal state
 * without going through the full node.ts entry point.
 */
import { test, describe, before, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { initWasmNode, initWasmBrowser, createWasmBackend, _resetForTesting, validateWasmShape } from '../dist/backend/wasm.js';

// Mirror of MAX_INIT_RETRIES from src/backend/wasm.ts.
// If this value drifts from the source, U-WB2 will fail to trigger the
// exhaustion path, surfacing the mismatch via a test failure rather than
// silently testing the wrong threshold.
const MAX_INIT_RETRIES = 3;

describe('wasm backend — circuit breaker', () => {
  afterEach(() => {
    // Restore a clean state after each test so the module singleton does not
    // bleed into subsequent tests or into the main backend.spec tests.
    _resetForTesting(0);
  });

  test('U-WB1: initWasmNode() attempts loading when failures are below the limit', async () => {
    // Pre-seed 2 failures (one below the threshold of 3).
    _resetForTesting(MAX_INIT_RETRIES - 1);
    // Should succeed because failures (2) < MAX_INIT_RETRIES (3).
    await assert.doesNotReject(initWasmNode());
  });

  test('U-WB2: initWasmNode() throws permanently once failure count reaches MAX_INIT_RETRIES', async () => {
    // Pre-seed exactly MAX_INIT_RETRIES failures to simulate exhaustion.
    _resetForTesting(MAX_INIT_RETRIES);
    await assert.rejects(
      () => initWasmNode(),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(
          err.message.includes('failed to initialize after'),
          `expected permanent-failure message, got: ${err.message}`,
        );
        assert.ok(
          err.message.includes(String(MAX_INIT_RETRIES)),
          `expected retry count in message, got: ${err.message}`,
        );
        return true;
      },
    );
  });

  test('U-WB3: initWasmNode() succeeds and produces a valid WasmModule when WASM module is present', async () => {
    // Regression: shape-check in tryLoadCandidate must accept a well-formed WASM
    // module (compile/check/scanImports all present). Before the fix, ALL errors
    // were swallowed and the module was cast blindly via "as WasmModule".
    // This test confirms the happy path: a correct module passes the shape check.
    await assert.doesNotReject(
      () => initWasmNode(),
      'initWasmNode() should resolve when a valid WASM module is on the candidate path',
    );
  });

  test('U-WB4: circuit breaker message includes retry count and is non-empty after exhaustion', async () => {
    // Verify the circuit breaker fires with a diagnostic message that cites the
    // attempt count — confirming errors are never silently swallowed.
    // Regression: prior "bare catch { return null; }" discarded all errors.
    _resetForTesting(MAX_INIT_RETRIES);
    await assert.rejects(
      () => initWasmNode(),
      (err) => {
        assert.ok(err instanceof Error, 'must be an Error instance');
        assert.ok(err.message.length > 0, 'error message must not be empty');
        assert.ok(
          err.message.includes('failed to initialize after'),
          `expected circuit-breaker message, got: ${err.message}`,
        );
        assert.ok(
          err.message.includes(String(MAX_INIT_RETRIES)),
          `expected retry count ${MAX_INIT_RETRIES} in message, got: ${err.message}`,
        );
        return true;
      },
    );
  });

  // ---------------------------------------------------------------------------
  // New tests for the split API
  // ---------------------------------------------------------------------------

  test('U-WB5: initWasmNode() returns WasmModule with compile, check, and scanImports (no compileMessages)', async () => {
    const mod = await initWasmNode();
    assert.equal(typeof mod.compile, 'function', 'WasmModule must have compile');
    assert.equal(typeof mod.check, 'function', 'WasmModule must have check');
    assert.equal(typeof mod.scanImports, 'function', 'WasmModule must have scanImports');
    // compileMessages was removed in PR-2/PR-3 — output kind is intrinsic
    assert.equal(typeof mod.compileMessages, 'undefined', 'WasmModule must NOT have compileMessages');
  });

  test('U-WB6: concurrent initWasmNode() calls share single promise', async () => {
    _resetForTesting(0);
    // Fire two concurrent calls — they must both resolve and share state.
    const [mod1, mod2] = await Promise.all([initWasmNode(), initWasmNode()]);
    // Both must be the same object (promise deduplication guarantee).
    assert.strictEqual(mod1, mod2, 'concurrent initWasmNode() calls must return the same module reference');
  });

  test('U-WB8: failed initWasmNode() does not poison subsequent calls (circuit breaker allows retries below limit)', async () => {
    // Seed 1 failure (below limit). First call should still succeed by reloading.
    _resetForTesting(1);
    await assert.doesNotReject(
      () => initWasmNode(),
      'initWasmNode() should retry and succeed when failures < MAX_INIT_RETRIES',
    );
  });

  test('U-WB9: createWasmBackend(mod) is synchronous and returns MdsBaseBackend', async () => {
    const mod = await initWasmNode();
    const backend = createWasmBackend(mod);
    assert.equal(typeof backend.compile, 'function', 'must have compile');
    assert.equal(typeof backend.check, 'function', 'must have check');
    assert.equal(typeof backend.getBackend, 'function', 'must have getBackend');
  });

  test('U-WB10: createWasmBackend(mod).compile("Hello!\\n") returns kind:markdown with correct output', async () => {
    const mod = await initWasmNode();
    const backend = createWasmBackend(mod);
    const result = backend.compile('Hello!\n');
    assert.equal(result.kind, 'markdown', `expected kind "markdown", got: ${result.kind}`);
    assert.equal(result.output, 'Hello!\n', `expected "Hello!\\n", got: ${result.output}`);
    assert.ok(Array.isArray(result.warnings));
    assert.ok(Array.isArray(result.dependencies));
  });

  test('U-WB11: createWasmBackend(mod).getBackend() returns "wasm"', async () => {
    const mod = await initWasmNode();
    const backend = createWasmBackend(mod);
    assert.equal(backend.getBackend(), 'wasm');
  });

  test('U-WB12: createWasmBackend(mod) has NO compileFile or checkFile', async () => {
    const mod = await initWasmNode();
    const backend = createWasmBackend(mod);
    assert.equal(
      'compileFile' in backend,
      false,
      'MdsBaseBackend must not have compileFile',
    );
    assert.equal(
      'checkFile' in backend,
      false,
      'MdsBaseBackend must not have checkFile',
    );
  });

  test('U-WB13: initWasmNode() only succeeds when module has scanImports', async () => {
    // Verifies that initWasmNode() only resolves when the loaded module passes
    // validateWasmShape (compile, check, and scanImports all present). The built
    // WASM module must expose scanImports for this call to succeed.
    const mod = await initWasmNode();
    assert.equal(
      typeof mod.scanImports,
      'function',
      'initWasmNode() must only succeed when scanImports is present in the WASM module',
    );
  });
});

// ---------------------------------------------------------------------------
// Browser circuit breaker (issues wasm:206 and wasm:232)
// ---------------------------------------------------------------------------

// Mirror of MAX_BROWSER_RETRIES from src/backend/wasm.ts.
const MAX_BROWSER_RETRIES = 3;

describe('wasm backend — browser circuit breaker', () => {
  afterEach(() => {
    _resetForTesting(0);
  });

  test('U-WB14: initWasmBrowser() throws permanently once browserFailures reaches MAX_BROWSER_RETRIES', async () => {
    // Pre-seed exactly MAX_BROWSER_RETRIES failures to simulate exhaustion.
    // _resetForTesting(failures, browserFailures) — second param seeds browser counter.
    _resetForTesting(0, MAX_BROWSER_RETRIES);
    await assert.rejects(
      () => initWasmBrowser(),
      (err) => {
        assert.ok(err instanceof Error, 'must be an Error instance');
        assert.ok(
          err.message.includes('failed to initialize after'),
          `expected permanent-failure message, got: ${err.message}`,
        );
        assert.ok(
          err.message.includes(String(MAX_BROWSER_RETRIES)),
          `expected retry count in message, got: ${err.message}`,
        );
        return true;
      },
    );
  });

  test('U-WB15: initWasmBrowser() allows retries below MAX_BROWSER_RETRIES', async () => {
    // With failures below the limit, initWasmBrowser() should not throw the
    // circuit-breaker error on the first call (though it may fail for other reasons
    // like missing bundler module in Node.js — we only verify the CB is not triggered).
    _resetForTesting(0, MAX_BROWSER_RETRIES - 1);
    // We expect any error other than the circuit-breaker permanent-failure message.
    const err = await initWasmBrowser().then(() => null, (e) => e);
    if (err !== null) {
      assert.ok(
        !err.message.includes('failed to initialize after'),
        `circuit breaker must not fire below limit, got: ${err.message}`,
      );
    }
  });

  test('U-WB16: _resetForTesting resets browserFailures counter', async () => {
    // Exhaust browser retries, then reset, and verify the circuit breaker no
    // longer fires immediately.
    _resetForTesting(0, MAX_BROWSER_RETRIES);
    // Confirm it fires.
    await assert.rejects(() => initWasmBrowser(), /failed to initialize after/);
    // Reset to 0 browser failures.
    _resetForTesting(0, 0);
    // Should NOT throw the circuit-breaker error immediately.
    const err = await initWasmBrowser().then(() => null, (e) => e);
    if (err !== null) {
      assert.ok(
        !err.message.includes('failed to initialize after'),
        `after reset, circuit breaker must not fire immediately, got: ${err.message}`,
      );
    }
  });

  test('U-WB21: browserFailures counter increments on actual initWasmBrowser() failure', async () => {
    // Verify the browserFailures += 1 catch handler in initWasmBrowser() fires
    // on a real failure (not just pre-seeded state). Pre-seed to one below the
    // limit, fail once via an actual call, then confirm the circuit breaker now
    // fires on the next call — proving the counter was incremented.
    //
    // In Node.js, import('mds-wasm') always fails (no bundler alias), so every
    // initWasmBrowser() call here produces a real failure that should increment
    // the counter.
    _resetForTesting(0, MAX_BROWSER_RETRIES - 1);
    // This call must fail for a reason OTHER than the circuit breaker.
    const firstErr = await initWasmBrowser().then(() => null, (e) => e);
    assert.ok(firstErr instanceof Error, 'initWasmBrowser() must fail in Node.js environment');
    assert.ok(
      !firstErr.message.includes('failed to initialize after'),
      `first failure must not be the circuit-breaker error; got: ${firstErr.message}`,
    );
    // The catch handler must have incremented browserFailures to MAX_BROWSER_RETRIES.
    // The next call should hit the circuit breaker immediately.
    await assert.rejects(
      () => initWasmBrowser(),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(
          err.message.includes('failed to initialize after'),
          `second call must hit circuit breaker, got: ${err.message}`,
        );
        return true;
      },
    );
  });
});

// ---------------------------------------------------------------------------
// WASM basePath rejection (OD-1 / AC-P3-09 / AC-P3-10)
// ---------------------------------------------------------------------------

describe('wasm backend — basePath rejection (OD-1)', () => {
  let wasmMod;
  before(async () => {
    _resetForTesting(0);
    wasmMod = await initWasmNode();
  });

  afterEach(() => {
    _resetForTesting(0);
  });

  test('U-WB22: createWasmBackend.compile throws mds::invalid_options when basePath is set (AC-P3-09)', () => {
    const be = createWasmBackend(wasmMod);
    let caughtErr;
    try { be.compile('Hello\n', { basePath: '.' }); } catch (e) { caughtErr = e; }

    assert.ok(caughtErr instanceof Error, 'must throw an Error');
    assert.equal(caughtErr.code, 'mds::invalid_options');
    assert.ok(
      caughtErr.message.includes('basePath'),
      `message must name basePath: "${caughtErr.message}"`,
    );
    assert.ok(
      caughtErr.message.includes('WASM') || caughtErr.message.includes('filesystem'),
      `message must name the constraint: "${caughtErr.message}"`,
    );
    assert.ok(
      caughtErr.message.includes('MDS_BACKEND=native') || caughtErr.message.includes('native'),
      `message must name a remedy: "${caughtErr.message}"`,
    );

    // AC-P3-10: message must NOT contain internal WASM keys.
    assert.ok(!caughtErr.message.includes('filename'), `must not expose "filename": "${caughtErr.message}"`);
    assert.ok(!caughtErr.message.includes('modules'),  `must not expose "modules": "${caughtErr.message}"`);

    // PF-013 positive control: calling the raw WASM module directly DOES produce the
    // internal "filename"/"modules" error — proving the negative assertion above can
    // detect a leak if the guard were removed.
    let rawErr;
    try { wasmMod.compile('Hello\n', { basePath: '.' }); } catch (e) { rawErr = e; }
    assert.ok(rawErr instanceof Error, 'raw WASM module must throw for unknown basePath');
    const rawMsg = rawErr.message ?? '';
    assert.ok(
      rawMsg.includes('filename') || rawMsg.includes('modules'),
      `raw WASM error must contain internal keys (positive control): "${rawMsg}"`,
    );
  });

  test('U-WB23: createWasmBackend.check throws mds::invalid_options when basePath is set (AC-P3-09)', () => {
    const be = createWasmBackend(wasmMod);
    assert.throws(
      () => be.check('Hello\n', { basePath: '.' }),
      (err) => {
        assert.ok(err instanceof Error);
        assert.equal(err.code, 'mds::invalid_options');
        assert.ok(err.message.includes('basePath'));
        assert.ok(!err.message.includes('filename'), `must not expose "filename": "${err.message}"`);
        assert.ok(!err.message.includes('modules'),  `must not expose "modules": "${err.message}"`);
        return true;
      },
    );
  });

  test('U-WB24: createWasmBackend.lint throws mds::invalid_options when basePath is set (AC-P3-09)', () => {
    const be = createWasmBackend(wasmMod);
    assert.throws(
      () => be.lint('Hello\n', { basePath: '.' }),
      (err) => {
        assert.ok(err instanceof Error);
        assert.equal(err.code, 'mds::invalid_options');
        assert.ok(err.message.includes('basePath'));
        assert.ok(!err.message.includes('filename'), `must not expose "filename": "${err.message}"`);
        assert.ok(!err.message.includes('modules'),  `must not expose "modules": "${err.message}"`);
        return true;
      },
    );
  });

  test('U-WB25: createWasmBackend accepts {basePath: undefined} for compile/check/lint (OD-6 semantic: undefined = absent)', () => {
    const be = createWasmBackend(wasmMod);
    // basePath: undefined is treated as absent (value-is-intent semantic).
    assert.doesNotThrow(() => be.compile('Hello\n', { basePath: undefined }));
    assert.doesNotThrow(() => be.check('Hello\n', { basePath: undefined }));
    assert.doesNotThrow(() => be.lint('Hello\n', { basePath: undefined }));
  });
});

describe('wasm backend — browser shape validation', () => {
  afterEach(() => {
    _resetForTesting(0);
  });

  test('U-WB17: validateWasmShape accepts a well-formed module (compile, check, lint, scanImports, lintVirtual)', () => {
    const validMod = {
      compile: () => {},
      check: () => {},
      lint: () => {},
      scanImports: () => [],
      lintVirtual: () => {},
    };
    assert.doesNotThrow(
      () => validateWasmShape(validMod),
      'validateWasmShape must not throw for a well-formed module',
    );
  });

  test('U-WB18: validateWasmShape throws when compile is missing', () => {
    const mod = { check: () => {}, scanImports: () => [] };
    assert.throws(
      () => validateWasmShape(mod),
      (err) => {
        assert.ok(err instanceof Error, 'must throw an Error');
        assert.ok(
          err.message.includes('compile'),
          `error must mention missing function "compile", got: ${err.message}`,
        );
        return true;
      },
    );
  });

  test('U-WB19: validateWasmShape throws when check is missing', () => {
    const mod = { compile: () => {}, scanImports: () => [] };
    assert.throws(
      () => validateWasmShape(mod),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(
          err.message.includes('check'),
          `error must mention missing function "check", got: ${err.message}`,
        );
        return true;
      },
    );
  });

  test('U-WB20: validateWasmShape throws when scanImports is missing', () => {
    const mod = { compile: () => {}, check: () => {}, lint: () => {}, lintVirtual: () => {} };
    assert.throws(
      () => validateWasmShape(mod),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(
          err.message.includes('scanImports'),
          `error must mention missing function "scanImports", got: ${err.message}`,
        );
        return true;
      },
    );
  });
});
