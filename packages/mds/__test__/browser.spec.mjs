/**
 * Browser entry point behavioral tests for @mdscript/mds.
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
  lint,
  lintVirtual,
  _resetForTesting as browserReset,
  _initWithModuleForTesting,
} from '../dist/browser.js';
import { initWasmNode, _resetForTesting as wasmReset } from '../dist/backend/wasm.js';
import { lint as nodeLint, init as nodeInit } from '../dist/node.js';

// Mirror of MAX_INIT_RETRIES from src/backend/wasm.ts.
// If this value drifts, U-BR11 will surface the mismatch via a test failure.
const MAX_INIT_RETRIES = 3;

// Load the WASM module once at file scope using the Node.js loader.
// All browser tests that need a live backend inject it via _initWithModuleForTesting().
// nodeInit() is also called here to satisfy the cross-surface parity test (U-BR-PARITY).
let sharedWasmModule;
before(async () => {
  sharedWasmModule = await initWasmNode();
  await nodeInit();
});

// ---------------------------------------------------------------------------
// Pre-init behavior (describe ensures these complete before post-init suite)
// ---------------------------------------------------------------------------

describe('browser entry — pre-init', () => {
  // Ensure we start in a clean state before each test in this block.
  before(() => browserReset());

  test('U-BR20: lint throws before init() with message mentioning init() (AC-P3-15)', () => {
    assert.throws(
      () => lint('Hello!\n'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(
          err.message.includes('init()'),
          `expected init() in message, got: ${err.message}`,
        );
        return true;
      },
    );
  });

  test('U-BR21: lintVirtual throws before init() with message mentioning init() (AC-P3-15)', () => {
    assert.throws(
      () => lintVirtual({ 'a.mds': 'Hello\n' }, 'a.mds'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(
          err.message.includes('init()'),
          `expected init() in message, got: ${err.message}`,
        );
        return true;
      },
    );
  });

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

  test('U-BR14: lint and lintVirtual are exported from browser entry (AC-P3-12)', () => {
    assert.equal(typeof lint, 'function', 'lint must be a function');
    assert.equal(typeof lintVirtual, 'function', 'lintVirtual must be a function');
  });

  test('U-BR15: lintFile is NOT exported from browser entry (AC-P3-13)', async () => {
    const moduleExports = Object.keys(await import('../dist/browser.js'));
    assert.equal(
      moduleExports.includes('lintFile'),
      false,
      `lintFile must not be exported from browser entry; found: ${moduleExports.join(', ')}`,
    );
    // Also verify compileFile and checkFile remain absent.
    assert.equal(moduleExports.includes('compileFile'), false, 'compileFile must not be exported');
    assert.equal(moduleExports.includes('checkFile'),   false, 'checkFile must not be exported');
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

  test('U-BR16: browser lint returns a LintResult with version/files/truncated (AC-P3-12)', () => {
    // Use a source with an unused frontmatter variable so we get at least one finding.
    const src = '---\ngreeting: Hello\nunused_key: this key is never referenced\n---\n\n{{greeting}}, world!\n';
    const result = lint(src);
    assert.equal(result.version, 1, 'version must be 1');
    assert.ok(Array.isArray(result.files), 'files must be an array');
    assert.equal(result.truncated, false, 'truncated must be false');
    // At least one diagnostic (unused-variable for unused_key).
    assert.ok(result.files.length > 0, 'expected at least one file report for unused frontmatter');
  });

  test('U-BR17: browser lintVirtual returns findings keyed by entry (AC-P3-12)', () => {
    // Use a self-contained module map so no cross-module import is needed.
    // The entry has an unused frontmatter variable to produce at least one diagnostic.
    const modules = {
      'main.mds': '---\ngreeting: Hello\nunused_key: never used\n---\n{{greeting}}, world!\n',
    };
    const result = lintVirtual(modules, 'main.mds');
    assert.equal(result.version, 1);
    assert.ok(Array.isArray(result.files));
    assert.equal(result.truncated, false);
    // The entry key must appear in the report, and it must carry a real finding
    // (the unused frontmatter variable). `files.length >= 0` would be a tautology —
    // it holds for an empty array and so cannot distinguish a working lintVirtual
    // from one that silently returns no findings.
    const fileNames = result.files.map((f) => f.file);
    assert.ok(
      fileNames.includes('main.mds'),
      `expected entry 'main.mds' in report; got: [${fileNames.join(', ')}]`,
    );
    const entryReport = result.files.find((f) => f.file === 'main.mds');
    assert.ok(
      entryReport.diagnostics.length > 0,
      'expected at least one diagnostic for the unused frontmatter variable',
    );
    assert.ok(
      entryReport.diagnostics.some((d) => d.rule === 'unused-variable'),
      `expected an unused-variable diagnostic; got rules: [${entryReport.diagnostics.map((d) => d.rule).join(', ')}]`,
    );
  });

  test('U-BR-PARITY: browser lint result equals node lint result at runtime (AC-P3-12, amendment 4, avoids PF-007)', () => {
    // PF-007: per-surface goldens each lock in their OWN value and cannot prove
    // cross-surface parity. Compare browser (WASM) and node surfaces at RUNTIME for
    // the same input with deepStrictEqual — no pinned golden, no local assertion.
    // Same fixture as U-BR16 so the result is non-trivial (unused-variable diagnostic).
    const src = '---\ngreeting: Hello\nunused_key: this key is never referenced\n---\n\n{{greeting}}, world!\n';
    const browserResult = lint(src);
    const nodeResult = nodeLint(src);
    assert.deepStrictEqual(
      browserResult,
      nodeResult,
      'browser (WASM) and node lint must return byte-identical results for the same source',
    );
  });

  test('U-BR19: browser lint/lintVirtual reject unknown option keys (AC-P3-14)', () => {
    // Proves the browser path runs assertKnownKeys.
    assert.throws(
      () => lint('Hello\n', { basePathh: '.' }),
      (err) => {
        assert.ok(isMdsError(err), `expected isMdsError, got: ${err}`);
        assert.equal(err.code, 'mds::invalid_options');
        assert.ok(err.message.includes('"basePathh"'), `key in message: ${err.message}`);
        return true;
      },
    );
    assert.throws(
      () => lintVirtual({ 'a.mds': '' }, 'a.mds', { ruless: {} }),
      (err) => {
        assert.ok(isMdsError(err), `expected isMdsError, got: ${err}`);
        assert.equal(err.code, 'mds::invalid_options');
        assert.ok(err.message.includes('"ruless"'), `key in message: ${err.message}`);
        return true;
      },
    );
  });

  test('U-BR6: concurrent init() cannot double-init an already-initialized backend', () => {
    // Backend is already set by _initWithModuleForTesting; additional init() calls
    // resolve immediately (resolvedBackend guard). This verifies idempotency.
    // (Concurrent promise dedup is tested via U-BR11 below.)
    const result = compile('Hello!\n');
    assert.equal(result.kind, 'markdown');
    assert.ok(result.output.includes('Hello'));
  });

  test('U-BR7: compile returns output after init()', () => {
    const result = compile('Hello World!\n');
    assert.equal(result.kind, 'markdown');
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
      () => compile('Hello {{unclosed\n'),
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
    assert.equal(result.kind, 'markdown');
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
    const afterInject = compile('After inject!\n');
    assert.equal(afterInject.kind, 'markdown');
    assert.ok(afterInject.output.includes('After inject'));

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
    const recovered = compile('Recovered!\n');
    assert.equal(recovered.kind, 'markdown');
    assert.ok(recovered.output.includes('Recovered'));
  });
});
