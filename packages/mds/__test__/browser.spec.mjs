/**
 * Browser entry point behavioral tests for @mds/mds.
 * Tests: U-BR1 through U-BR11
 *
 * Imports dist/browser.js directly. Node.js ESM module state is shared within
 * the process. Node.js test runner executes top-level describe blocks
 * sequentially, so pre-init tests complete before the post-init suite starts.
 * init() is called in a before() hook inside the post-init describe block.
 */
import { test, describe, before, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import {
  compile,
  check,
  compileFile,
  checkFile,
  getBackend,
  init,
  isMdsError,
  _resetForTesting as browserReset,
} from '../dist/browser.js';
import { init as wasmInit, _resetForTesting as wasmReset } from '../dist/backend/wasm.js';

// ---------------------------------------------------------------------------
// Pre-init behavior (describe ensures these complete before post-init suite)
// ---------------------------------------------------------------------------

describe('browser entry — pre-init', () => {
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

  test('U-BR3: compileFile always rejects regardless of init state', async () => {
    await assert.rejects(
      () => compileFile('/some/path.mds'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(
          err.message.includes('browser'),
          `expected message to mention browser limitation, got: ${err.message}`,
        );
        return true;
      },
    );
  });

  test('U-BR4: checkFile always rejects regardless of init state', async () => {
    await assert.rejects(
      () => checkFile('/some/path.mds'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(
          err.message.includes('browser'),
          `expected message to mention browser limitation, got: ${err.message}`,
        );
        return true;
      },
    );
  });

  test('U-BR5: getBackend() always returns "wasm"', () => {
    assert.equal(getBackend(), 'wasm');
  });

  // Deduplication test: two concurrent calls must both resolve and must not
  // cause double-initialization errors. Reference equality is not testable
  // because init() is async and wraps each return value in a new Promise.
  test('U-BR6: concurrent init() calls both resolve without error', async () => {
    const p1 = init();
    const p2 = init();
    // Both must settle successfully — no "double init" errors.
    await assert.doesNotReject(Promise.all([p1, p2]));
  });
});

// ---------------------------------------------------------------------------
// Post-init behavior (before() re-calls init() to ensure backend is ready;
// init() is idempotent so the second call is a no-op)
// ---------------------------------------------------------------------------

describe('browser entry — post-init', () => {
  before(() => init());

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

  test('U-BR10: init() is idempotent — repeated call after success is a no-op', async () => {
    await assert.doesNotReject(() => init());
    // Backend must still be functional after repeated init.
    const result = compile('Idempotent!\n');
    assert.ok(result.output.includes('Idempotent!'));
  });
});

// ---------------------------------------------------------------------------
// Retry / rejection reset behavior
// ---------------------------------------------------------------------------

describe('browser entry — init() retry after transient failure', () => {
  // Restore both module singletons after each test so other suites are unaffected.
  // wasmReset(0) clears wasmModule so we must re-warm it with wasmInit() to
  // avoid leaving wasm state blank for other spec files in the same process.
  afterEach(async () => {
    browserReset();
    wasmReset(0);
    await wasmInit();
  });

  test('U-BR11: init() clears cached promise on rejection so next call can retry', async () => {
    // Exhaust wasm.ts retries so createWasmBackend() rejects immediately.
    wasmReset(3); // MAX_INIT_RETRIES = 3
    browserReset();

    // First call: should reject because wasm is exhausted.
    await assert.rejects(
      () => init(),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(
          err.message.includes('failed to initialize after'),
          `expected exhaustion message, got: ${err.message}`,
        );
        return true;
      },
    );

    // Second call: wasm is still exhausted, but the key invariant is that
    // browser's cached initVoidPromise was cleared on the first rejection,
    // so a new promise is created and the rejection is a fresh attempt, not
    // the stale one returned unchanged.
    await assert.rejects(
      () => init(),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(
          err.message.includes('failed to initialize after'),
          `expected exhaustion message on second call, got: ${err.message}`,
        );
        return true;
      },
    );

    // Restore wasm to good state and verify a fresh init() now succeeds,
    // confirming the browser module did not cache the stale rejection.
    wasmReset(0);
    await assert.doesNotReject(() => init());
  });
});
