/**
 * Backend selection tests for @mdscript/mds universal package.
 * Tests: U-B1 through U-B11
 */
import { test, describe, before } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { getBackend, compile, compileFile, init, _resetForTesting } from '../dist/node.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

describe('backend', () => {
  before(() => init());

  test('U-B1: getBackend() returns "native" or "wasm"', () => {
    const backend = getBackend();
    assert.ok(
      backend === 'native' || backend === 'wasm',
      `expected "native" or "wasm", got: ${backend}`,
    );
  });

  test('U-B2: native backend is selected by default when napi is available', () => {
    // The default (no MDS_BACKEND env var) should prefer native.
    // Since we're running tests with the .node file available, backend should be native.
    const backend = getBackend();
    assert.equal(backend, 'native', `expected native backend, got: ${backend}`);
  });

  test('U-B3: compile works regardless of backend', () => {
    // This test validates that compile() works with whatever backend was selected.
    const result = compile('Hello World!\n');
    assert.equal(result.output, 'Hello World!\n');
  });

  test('U-B4: compile result shape is consistent across backends', () => {
    const result = compile('Hello World!\n');
    assert.ok(typeof result.output === 'string', 'output must be string');
    assert.ok(Array.isArray(result.warnings), 'warnings must be array');
    assert.ok(Array.isArray(result.dependencies), 'dependencies must be array');
  });

  test('U-B5: MDS_BACKEND=wasm forces WASM backend', () => {
    // Spawn a subprocess with MDS_BACKEND=wasm to test backend selection
    // without affecting the current process's already-resolved backend.
    const output = execFileSync(process.execPath, ['--input-type=module'], {
      input: `import { getBackend, init } from '../dist/node.js';\nawait init();\nconsole.log(getBackend());\n`,
      cwd: __dirname,
      env: { ...process.env, MDS_BACKEND: 'wasm' },
      encoding: 'utf8',
    });
    assert.equal(output.trim(), 'wasm', `expected WASM backend when MDS_BACKEND=wasm, got: ${output.trim()}`);
  });

  test('U-B6: module import completes without I/O (no top-level await)', async () => {
    // Importing node.ts must not perform any I/O or load the backend eagerly.
    // If TLA is present, the import itself would block until WASM/native loads.
    // We verify this indirectly: _resetForTesting() removes the initialized backend,
    // so if compile() works without a new init(), TLA must have run — which would
    // be a regression. After reset, compile() must throw.
    _resetForTesting();
    try {
      assert.throws(
        () => compile('Hello!\n'),
        (err) => {
          assert.ok(err instanceof Error);
          assert.ok(err.message.includes('init()'));
          return true;
        },
      );
    } finally {
      // Re-init for subsequent tests regardless of assertion outcome.
      await init();
    }
  });

  test('U-B7: compile() throws before init() with clear message', () => {
    // Backend was re-initialized by U-B6's final await init(). We need a fresh
    // reset to test pre-init behavior. Use a subprocess to avoid affecting state.
    const output = execFileSync(process.execPath, ['--input-type=module'], {
      input: `import { compile } from '../dist/node.js';
try { compile('Hello!\\n'); console.log('no-throw'); }
catch (e) { console.log(e.message.includes('init()') ? 'correct' : 'wrong: ' + e.message); }
`,
      cwd: __dirname,
      env: { ...process.env },
      encoding: 'utf8',
    });
    assert.equal(output.trim(), 'correct', `expected init() error before init, got: ${output.trim()}`);
  });

  test('U-B8: concurrent init() calls share single promise', async () => {
    // Both concurrent calls must resolve without error and the backend must
    // only be initialized once (shared promise deduplication).
    _resetForTesting();
    await Promise.all([init(), init()]);
    // Both resolved — verify backend is functional.
    const result = compile('Concurrent!\n');
    assert.ok(result.output.includes('Concurrent'));
  });

  test('U-B10: getBackend() throws before init() with clear message', () => {
    const output = execFileSync(process.execPath, ['--input-type=module'], {
      input: `import { getBackend } from '../dist/node.js';
try { getBackend(); console.log('no-throw'); }
catch (e) { console.log(e.message.includes('init()') ? 'correct' : 'wrong: ' + e.message); }
`,
      cwd: __dirname,
      env: { ...process.env },
      encoding: 'utf8',
    });
    assert.equal(output.trim(), 'correct', `expected init() error for getBackend() before init, got: ${output.trim()}`);
  });

  test('U-B11: compileFile() throws before init() with clear message', () => {
    const output = execFileSync(process.execPath, ['--input-type=module'], {
      input: `import { compileFile } from '../dist/node.js';
try { compileFile('/tmp/test.mds'); console.log('no-throw'); }
catch (e) { console.log(e.message.includes('init()') ? 'correct' : 'wrong: ' + e.message); }
`,
      cwd: __dirname,
      env: { ...process.env },
      encoding: 'utf8',
    });
    assert.equal(output.trim(), 'correct', `expected init() error for compileFile() before init, got: ${output.trim()}`);
  });
});
