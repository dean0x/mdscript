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

  test('U+2028 and U+2029 in output are escaped in export default line', async () => {
    const u2028 = ' ';
    const u2029 = ' ';
    const mds = createMockMds({
      async compileFile() {
        return {
          output: `before${u2028}middle${u2029}after`,
          warnings: [],
          dependencies: [],
        };
      },
    });
    const transformer = createMdsTransformer(mds);
    const result = await transformer.transform('/file.mds');

    const lines = result.code.split('\n');
    const exportLine = lines.find(l => l.startsWith('export default'));
    assert.ok(exportLine, 'should have export default line');
    // Raw U+2028/U+2029 must not appear — they are JS line terminators
    assert.ok(!exportLine.includes(u2028), 'U+2028 must not appear raw in export default');
    assert.ok(!exportLine.includes(u2029), 'U+2029 must not appear raw in export default');
    // Must appear as explicit unicode escape sequences
    assert.ok(exportLine.includes('\\u2028'), 'U+2028 must be escaped as \\u2028');
    assert.ok(exportLine.includes('\\u2029'), 'U+2029 must be escaped as \\u2029');
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

  test('metadata is safe for inline script embedding (no </script> or U+2028/U+2029)', async () => {
    const u2028 = ' ';
    const u2029 = ' ';
    const mds = createMockMds({
      async compileFile() {
        return {
          output: 'content',
          // Warnings may contain compiler output that includes these characters.
          warnings: ['</script> injection', `line${u2028}sep`, `para${u2029}sep`],
          dependencies: [],
        };
      },
    });
    const transformer = createMdsTransformer(mds);
    const result = await transformer.transform('/file.mds');

    const metaLine = result.code.split('\n').find(l => l.startsWith('export const metadata'));
    assert.ok(metaLine, 'should have metadata export line');
    // '</script>' must not appear verbatim — would close an enclosing <script> block
    assert.ok(!metaLine.includes('</script>'), '</script> must be escaped in metadata');
    // U+2028/U+2029 are JS line terminators and must not appear verbatim
    assert.ok(!metaLine.includes(u2028), 'U+2028 must be escaped in metadata');
    assert.ok(!metaLine.includes(u2029), 'U+2029 must be escaped in metadata');
  });

  test('concurrent transforms call init() exactly once', async () => {
    const mds = createMockMds();
    const transformer = createMdsTransformer(mds);

    // Fire multiple transforms concurrently — the promise-caching pattern must
    // ensure init() is called only once even when all calls race to ensureInit.
    await Promise.all([
      transformer.transform('/file1.mds'),
      transformer.transform('/file2.mds'),
      transformer.transform('/file3.mds'),
    ]);

    assert.equal(mds.initCallCount, 1, 'init should be called exactly once under concurrent load');
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
