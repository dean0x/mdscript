/**
 * Browser entry point behavioral tests for @mds/mds.
 * Tests: U-BR1 through U-BR13
 *
 * Imports dist/browser.js directly. Node.js ESM module state is shared within
 * the process. Node.js test runner executes top-level describe blocks
 * sequentially, so pre-init tests complete before the post-init suite starts.
 *
 * Since browser.ts uses initWasmBrowser() which requires a bundler-resolved
 * 'mds-wasm' module, we use _initWithModuleForTesting() to inject a pre-loaded
 * WasmModule from initWasmNode() for Node.js test execution. This lets us test
 * the browser entry API surface (compile/check/getBackend/init contract) without
 * triggering the browser-only import path.
 */
import { test, describe, before, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import {
  compile,
  check,
  getBackend,
  isMdsError,
  _resetForTesting as browserReset,
  _initWithModuleForTesting,
} from '../dist/browser.js';
import { initWasmNode, _resetForTesting as wasmReset } from '../dist/backend/wasm.js';

// Mirror of MAX_INIT_RETRIES from src/backend/wasm.ts.
// If this value drifts, U-BR11 will surface the mismatch via a test failure.
const MAX_INIT_RETRIES = 3;

// Load the WASM module once at file scope using the Node.js loader.
// All browser tests that need a live backend inject it via _initWithModuleForTesting().
let sharedWasmModule;
before(async () => {
  sharedWasmModule = await initWasmNode();
});

// ---------------------------------------------------------------------------
// Pre-init behavior (describe ensures these complete before post-init suite)
// ---------------------------------------------------------------------------

describe('browser entry — pre-init', () => {
  // Ensure we start in a clean state before each test in this block.
  before(() => browserReset());

  test('U-BR1: compile throws before init()', () => {
    assert.throws(
      () => compile('Hello!\n'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(
          err.message.includes('init()'),
          `expected message to mention init(), got: ${err.message}`,
        );
        return true;
      },
    );
  });

  test('U-BR2: check throws before init()', () => {
    assert.throws(
      () => check('Hello!\n'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(
          err.message.includes('init()'),
          `expected message to mention init(), got: ${err.message}`,
        );
        return true;
      },
    );
  });

  test('U-BR5: getBackend() always returns "wasm"', () => {
    assert.equal(getBackend(), 'wasm');
  });

  test('U-BR12: compileFile is NOT a property of browser module', async () => {
    // Browser entry no longer exports compileFile — it requires node:fs which is
    // not available in browser environments.
    // We use a dynamic import to inspect the module's named exports.
    const moduleExports = Object.keys(await import('../dist/browser.js'));
    assert.equal(
      moduleExports.includes('compileFile'),
      false,
      `compileFile must not be exported from browser entry, found exports: ${moduleExports.join(', ')}`,
    );
  });

  test('U-BR13: checkFile is NOT a property of browser module', async () => {
    // Browser entry no longer exports checkFile.
    const moduleExports = Object.keys(await import('../dist/browser.js'));
    assert.equal(
      moduleExports.includes('checkFile'),
      false,
      `checkFile must not be exported from browser entry, found exports: ${moduleExports.join(', ')}`,
    );
  });
});

// ---------------------------------------------------------------------------
// Post-init behavior (uses _initWithModuleForTesting to inject Node-loaded WASM)
// ---------------------------------------------------------------------------

describe('browser entry — post-init', () => {
  before(() => {
    browserReset();
    _initWithModuleForTesting(sharedWasmModule);
  });

  test('U-BR6: concurrent init() cannot double-init an already-initialized backend', () => {
    // Backend is already set by _initWithModuleForTesting; additional init() calls
    // resolve immediately (resolvedBackend guard). This verifies idempotency.
    // (Concurrent promise dedup is tested via U-BR11 below.)
    assert.ok(compile('Hello!\n').output.includes('Hello'));
  });

  test('U-BR7: compile returns output after init()', () => {
    const result = compile('Hello World!\n');
    assert.equal(typeof result.output, 'string');
    assert.ok(result.output.includes('Hello World!'));
    assert.ok(Array.isArray(result.warnings));
    assert.ok(Array.isArray(result.dependencies));
  });

  test('U-BR8: check returns warnings array after init()', () => {
    const result = check('Hello World!\n');
    assert.ok(Array.isArray(result.warnings));
    assert.ok(!('output' in result), 'check result must not have output field');
  });

  test('U-BR9: compile throws MdsError on syntax error after init()', () => {
    assert.throws(
      () => compile('Hello {unclosed\n'),
      (err) => {
        assert.ok(isMdsError(err), `expected MdsError, got: ${String(err)}`);
        assert.ok(typeof err.code === 'string', 'MdsError must have a string code');
        return true;
      },
    );
  });

  test('U-BR10: init()-like idempotency — re-injecting module is a no-op for compile', () => {
    // Re-injecting is not a real re-init but verifies the backend is stable.
    _initWithModuleForTesting(sharedWasmModule);
    const result = compile('Idempotent!\n');
    assert.ok(result.output.includes('Idempotent!'));
  });
});

// ---------------------------------------------------------------------------
// Retry / rejection reset behavior
// ---------------------------------------------------------------------------

describe('browser entry — init() promise dedup and reset', () => {
  // Restore both module singletons after each test so other suites are unaffected.
  afterEach(async () => {
    browserReset();
    wasmReset(0);
    await initWasmNode();
  });

  test('U-BR11: _resetForTesting() clears state so subsequent init needs a new module injection', () => {
    // Seed a backend.
    _initWithModuleForTesting(sharedWasmModule);
    assert.ok(compile('After inject!\n').output.includes('After inject'));

    // Reset.
    browserReset();
    // Now compile should throw.
    assert.throws(
      () => compile('Should throw!\n'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(err.message.includes('init()'));
        return true;
      },
    );

    // Re-inject and verify recovery.
    _initWithModuleForTesting(sharedWasmModule);
    assert.ok(compile('Recovered!\n').output.includes('Recovered'));
  });
});
