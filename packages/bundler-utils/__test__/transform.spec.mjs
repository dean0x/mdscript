/**
 * Tests for createMdsTransformer.
 */
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { createMdsTransformer } from '../dist/index.js';

// ---------------------------------------------------------------------------
// Mock MdsApi
// ---------------------------------------------------------------------------
function createMockMds(overrides = {}) {
  let initCallCount = 0;
  const compileFileCalls = [];

  const mds = {
    async init() {
      initCallCount++;
    },
    async compileFile(path, options) {
      compileFileCalls.push({ path, options });
      return {
        output: `compiled: ${path}`,
        warnings: [],
        dependencies: [],
      };
    },
    isMdsError(err) {
      return err instanceof Error && typeof err.code === 'string' && err.code.startsWith('mds::');
    },
    get initCallCount() { return initCallCount; },
    get compileFileCalls() { return compileFileCalls; },
    ...overrides,
  };
  return mds;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
describe('createMdsTransformer', () => {
  test('init() called exactly once across multiple transforms', async () => {
    const mds = createMockMds();
    const transformer = createMdsTransformer(mds);

    await transformer.transform('/file1.mds');
    await transformer.transform('/file2.mds');
    await transformer.transform('/file3.mds');

    assert.equal(mds.initCallCount, 1, 'init should be called exactly once');
  });

  test('compileFile called with correct path', async () => {
    const mds = createMockMds();
    const transformer = createMdsTransformer(mds);

    await transformer.transform('/path/to/file.mds');

    assert.equal(mds.compileFileCalls.length, 1);
    assert.equal(mds.compileFileCalls[0].path, '/path/to/file.mds');
  });

  test('output is valid JS with default export', async () => {
    const mds = createMockMds({
      async compileFile() {
        return { output: 'Hello World!', warnings: [], dependencies: [] };
      },
    });
    const transformer = createMdsTransformer(mds);
    const result = await transformer.transform('/file.mds');

    // Should be parseable JS
    assert.ok(result.code.includes('export default'), 'should have default export');
    assert.ok(result.code.includes('export const metadata'), 'should have metadata export');
  });

  test('special chars in output are escaped', async () => {
    const mds = createMockMds({
      async compileFile() {
        return {
          output: 'Hello\nWorld\r\n"quoted"\\backslash',
          warnings: [],
          dependencies: [],
        };
      },
    });
    const transformer = createMdsTransformer(mds);
    const result = await transformer.transform('/file.mds');

    // The code should not have raw newlines inside the string literal
    // Validate by parsing
    const lines = result.code.split('\n');
    const exportLine = lines.find(l => l.startsWith('export default'));
    assert.ok(exportLine, 'should have export default line');
    // Verify that the special characters are properly escaped in the JS string literal.
    // After escapeForJs, \n → \\n, \r → \\r, " → \", \\ → \\\\ (backslash).
    assert.ok(exportLine.includes('\\n'), 'newline should be escaped as \\n');
    assert.ok(exportLine.includes('\\r'), 'carriage return should be escaped as \\r');
    assert.ok(exportLine.includes('\\"'), 'double quote should be escaped as \\"');
    assert.ok(exportLine.includes('\\\\'), 'backslash should be escaped as \\\\');
  });

  test('dependencies passed through in result', async () => {
    const mds = createMockMds({
      async compileFile() {
        return {
          output: 'content',
          warnings: [],
          dependencies: ['/dep1.mds', '/dep2.mds'],
        };
      },
    });
    const transformer = createMdsTransformer(mds);
    const result = await transformer.transform('/file.mds');

    assert.deepEqual(result.dependencies, ['/dep1.mds', '/dep2.mds']);
  });

  test('warnings passed through in result', async () => {
    const mds = createMockMds({
      async compileFile() {
        return {
          output: 'content',
          warnings: ['warn1', 'warn2'],
          dependencies: [],
        };
      },
    });
    const transformer = createMdsTransformer(mds);
    const result = await transformer.transform('/file.mds');

    assert.deepEqual(result.warnings, ['warn1', 'warn2']);
  });

  test('vars forwarded to compileFile', async () => {
    const mds = createMockMds();
    const options = { vars: { name: 'Alice', count: 42 } };
    const transformer = createMdsTransformer(mds, options);

    await transformer.transform('/file.mds');

    assert.equal(mds.compileFileCalls.length, 1);
    assert.deepEqual(mds.compileFileCalls[0].options, { vars: { name: 'Alice', count: 42 } });
  });

  test('no vars option does not pass vars to compileFile', async () => {
    const mds = createMockMds();
    const transformer = createMdsTransformer(mds);

    await transformer.transform('/file.mds');

    assert.equal(mds.compileFileCalls[0].options, undefined);
  });

  test('empty output produces valid JS', async () => {
    const mds = createMockMds({
      async compileFile() {
        return { output: '', warnings: [], dependencies: [] };
      },
    });
    const transformer = createMdsTransformer(mds);
    const result = await transformer.transform('/file.mds');

    assert.ok(result.code.includes('export default ""'), 'should export empty string');
  });

  test('shouldTransform returns true for .mds', async () => {
    const mds = createMockMds();
    const transformer = createMdsTransformer(mds);
    const result = await transformer.shouldTransform('/path/to/file.mds');
    assert.equal(result, true);
  });

  test('shouldTransform returns false for non-mds', async () => {
    const mds = createMockMds();
    const transformer = createMdsTransformer(mds);
    const result = await transformer.shouldTransform('/path/to/file.ts');
    assert.equal(result, false);
  });

  test('null byte in output is escaped', async () => {
    const mds = createMockMds({
      async compileFile() {
        return { output: 'before\x00after', warnings: [], dependencies: [] };
      },
    });
    const transformer = createMdsTransformer(mds);
    const result = await transformer.transform('/file.mds');

    const lines = result.code.split('\n');
    const exportLine = lines.find(l => l.startsWith('export default'));
    assert.ok(exportLine, 'should have export default line');
    assert.ok(!exportLine.includes('\x00'), 'null byte must be escaped in JS string literal');
    assert.ok(exportLine.includes('\\0'), 'null byte must be escaped as \\0');
  });

  test('poisoned promise resets on init rejection, allowing retry', async () => {
    let callCount = 0;
    const mds = createMockMds({
      async init() {
        callCount++;
        if (callCount === 1) throw new Error('transient init failure');
      },
    });
    const transformer = createMdsTransformer(mds);

    // First call — init() rejects transiently
    await assert.rejects(() => transformer.transform('/file.mds'), /transient init failure/);

    // Second call — must retry init, not re-use the rejected promise
    await transformer.transform('/file.mds');
    assert.equal(callCount, 2, 'init should have been called twice (once for each attempt)');
  });
});
