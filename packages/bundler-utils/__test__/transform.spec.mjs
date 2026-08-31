/**
 * Tests for createMdsTransformer.
 */
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, realpathSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
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
      // Return discriminated-union CompileResult (kind:'markdown' for default mock)
      return {
        kind: 'markdown',
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
        return { kind: 'markdown', output: 'Hello World!', warnings: [], dependencies: [] };
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
          kind: 'markdown',
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
          kind: 'markdown',
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
          kind: 'markdown',
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
        return { kind: 'markdown', output: '', warnings: [], dependencies: [] };
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
    const u2028 = String.fromCodePoint(0x2028);
    const u2029 = String.fromCodePoint(0x2029);
    const mds = createMockMds({
      async compileFile() {
        return {
          kind: 'markdown',
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
        return { kind: 'markdown', output: 'before\x00after', warnings: [], dependencies: [] };
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
    const u2028 = String.fromCodePoint(0x2028);
    const u2029 = String.fromCodePoint(0x2029);
    const mds = createMockMds({
      async compileFile() {
        return {
          kind: 'markdown',
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

  test('markdown default export is safe for inline script embedding (no </script>)', async () => {
    const mds = createMockMds({
      async compileFile() {
        return {
          kind: 'markdown',
          output: 'Before </script><script>alert(1)</script> after',
          warnings: [],
          dependencies: [],
        };
      },
    });
    const transformer = createMdsTransformer(mds);
    const result = await transformer.transform('/file.mds');

    const exportLine = result.code.split('\n').find(l => l.startsWith('export default'));
    assert.ok(exportLine, 'should have export default line');
    // '</script>' must not appear verbatim — it would close an enclosing <script> block.
    // '<' is escaped to '<' so '</script>' becomes '</script>'.
    assert.ok(!exportLine.includes('</script>'), '</script> must not appear verbatim in markdown default export');
    assert.ok(exportLine.includes('\\u003c'), '< must be escaped as \\u003c in markdown default export');
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

// ---------------------------------------------------------------------------
// Intrinsic bundler export — messages kind (AC-API-14)
// ---------------------------------------------------------------------------

describe('createMdsTransformer — intrinsic bundler export', () => {
  test('AC-API-14: messages source → export default [...] array literal', async () => {
    const messages = [
      { role: 'system', content: 'You are helpful.' },
      { role: 'user', content: 'Hello!' },
    ];
    const mds = createMockMds({
      async compileFile() {
        return {
          kind: 'messages',
          messages,
          warnings: [],
          dependencies: [],
        };
      },
    });
    const transformer = createMdsTransformer(mds);
    const result = await transformer.transform('/chat.mds');

    // The emitted module must have a JSON array literal as default export
    assert.ok(result.code.includes('export default ['), 'messages result must emit array default export (AC-API-14)');
    // Must NOT emit a string default (that would be the markdown path)
    assert.ok(!result.code.includes('export default "'), 'messages result must not emit string default export');
    // Parse the emitted array and verify content round-trips correctly
    const match = result.code.match(/^export default (\[[\s\S]*?\]);/m);
    assert.ok(match, 'export default must be followed by an array literal');
    const parsed = JSON.parse(match[1]);
    assert.deepEqual(parsed, messages);
  });

  test('AC-API-14: messages result metadata is emitted correctly', async () => {
    const mds = createMockMds({
      async compileFile() {
        return {
          kind: 'messages',
          messages: [{ role: 'user', content: 'Hello.' }],
          warnings: ['orphan-warning'],
          dependencies: ['/dep.mds'],
        };
      },
    });
    const transformer = createMdsTransformer(mds);
    const result = await transformer.transform('/chat.mds');

    assert.ok(result.code.includes('export const metadata'), 'metadata export must be present');
    assert.deepEqual(result.dependencies, ['/dep.mds']);
    assert.deepEqual(result.warnings, ['orphan-warning']);
  });

  test('AC-API-15: markdown source → string default export still works (regression)', async () => {
    const mds = createMockMds({
      async compileFile() {
        return {
          kind: 'markdown',
          output: 'Hello World!',
          warnings: [],
          dependencies: [],
        };
      },
    });
    const transformer = createMdsTransformer(mds);
    const result = await transformer.transform('/file.mds');

    // Markdown path must still emit a string default export
    assert.ok(result.code.includes('export default "Hello World!"'), 'markdown result must emit string default export');
    assert.ok(result.code.includes('export const metadata'), 'metadata must be present for markdown too');
  });

  test('AC-API-14: messages with U+2028/U+2029 are safe in JSON array export', async () => {
    const u2028 = String.fromCodePoint(0x2028);
    const u2029 = String.fromCodePoint(0x2029);
    const mds = createMockMds({
      async compileFile() {
        return {
          kind: 'messages',
          messages: [{ role: 'user', content: `before${u2028}after${u2029}end` }],
          warnings: [],
          dependencies: [],
        };
      },
    });
    const transformer = createMdsTransformer(mds);
    const result = await transformer.transform('/chat.mds');

    // U+2028/U+2029 are JS line terminators — must be escaped in the emitted code
    const exportLine = result.code.split('\n').find(l => l.startsWith('export default'));
    assert.ok(exportLine, 'should have export default line');
    assert.ok(!exportLine.includes(u2028), 'U+2028 must not appear raw in messages export');
    assert.ok(!exportLine.includes(u2029), 'U+2029 must not appear raw in messages export');
  });

  test('AC-API-14: messages with </script> in content are safe in JSON array export', async () => {
    const mds = createMockMds({
      async compileFile() {
        return {
          kind: 'messages',
          messages: [{ role: 'assistant', content: 'Here is code: </script>' }],
          warnings: [],
          dependencies: [],
        };
      },
    });
    const transformer = createMdsTransformer(mds);
    const result = await transformer.transform('/chat.mds');

    // </script> must be escaped to prevent closing an enclosing <script> tag
    const exportLine = result.code.split('\n').find(l => l.startsWith('export default'));
    assert.ok(exportLine, 'should have export default line');
    assert.ok(!exportLine.includes('</script>'), '</script> must not appear verbatim in messages export');
  });
});

// ---------------------------------------------------------------------------
// J1: dependency path contracts (spec.md "Carve-out: functional path references")
//
// Two contracts, one datum:
// - TransformResult.dependencies is a FUNCTIONAL watch input (addWatchFile /
//   addDependency resolve relative paths against cwd) — entries must be ABSOLUTE.
// - The emitted `metadata` literal lands in production bundles — entries must be
//   project-root-relative POSIX paths, never host-absolute (PF-033 leak class).
// ---------------------------------------------------------------------------
describe('dependency path contracts (J1)', () => {
  /** Create a temp project root marked with .mdsroot (realpath'd for macOS /var symlink). */
  function makeTmpRoot(tag) {
    const root = realpathSync(mkdtempSync(join(tmpdir(), `mds-j1-${tag}-`)));
    writeFileSync(join(root, '.mdsroot'), '');
    mkdirSync(join(root, 'src'), { recursive: true });
    return root;
  }

  /** Extract and JSON.parse the emitted `export const metadata = {...};` literal. */
  function parseMetadata(code) {
    const line = code.split('\n').find((l) => l.startsWith('export const metadata = '));
    assert.ok(line, 'metadata export line present');
    return JSON.parse(line.slice('export const metadata = '.length, -1));
  }

  function mdsWithDeps(deps) {
    return createMockMds({
      async compileFile() {
        return { kind: 'markdown', output: 'content', warnings: [], dependencies: deps };
      },
    });
  }

  test('T-J1-1: metadata deps root-relative, TransformResult deps absolute, no root prefix in code', async () => {
    const root = makeTmpRoot('leak');
    try {
      const dep = join(root, 'lib', 'helper.mds');
      const transformer = createMdsTransformer(mdsWithDeps([dep]));
      const result = await transformer.transform(join(root, 'src', 'module.mds'));

      assert.deepEqual(result.dependencies, [dep],
        'TransformResult.dependencies must stay ABSOLUTE (functional watch input)');
      assert.deepEqual(parseMetadata(result.code).dependencies, ['lib/helper.mds'],
        'metadata deps must be project-root-relative POSIX');

      const leaks = (code) => code.includes(root);
      assert.equal(leaks(result.code), false, `emitted code must not contain the project root ${root}`);
      // PF-013 positive control: the predicate must catch a planted absolute path.
      assert.equal(leaks(result.code + root), true,
        'positive control: leak predicate must detect an injected absolute path');
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test('T-J1-2: WASM-shaped root-relative input deps are absolutized in TransformResult (idempotent round-trip)', async () => {
    const root = makeTmpRoot('wasm');
    try {
      // The WASM backend emits project-root-relative POSIX deps.
      const transformer = createMdsTransformer(mdsWithDeps(['lib/helper.mds']));
      const result = await transformer.transform(join(root, 'src', 'module.mds'));

      assert.deepEqual(result.dependencies, [resolve(root, 'lib/helper.mds')],
        'root-relative input must come out absolute — watch wiring must work under WASM fallback');
      assert.deepEqual(parseMetadata(result.code).dependencies, ['lib/helper.mds'],
        'relativize(absolutize(rel)) must round-trip to the same relative path');
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test('T-J1-3: dependency escaping the project root degrades to basename in metadata', async () => {
    const root = makeTmpRoot('escape');
    try {
      // Parent of the temp root is outside the project root by construction.
      const outside = join(resolve(root, '..'), 'mds-j1-outside.mds');
      const transformer = createMdsTransformer(mdsWithDeps([outside]));
      const result = await transformer.transform(join(root, 'src', 'module.mds'));

      assert.deepEqual(result.dependencies, [outside],
        'escaping dep must stay absolute in TransformResult (functional watch input)');
      const metaDeps = parseMetadata(result.code).dependencies;
      assert.deepEqual(metaDeps, ['mds-j1-outside.mds'],
        'root-escaping dep must degrade to basename in metadata (no ../ disclosure)');
      assert.ok(!result.code.includes('..'), 'metadata must not carry ../ traversal segments');
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test('T-J1-4: foreign drive-qualified dependency degrades to basename in metadata', async () => {
    const root = makeTmpRoot('drive');
    try {
      const transformer = createMdsTransformer(mdsWithDeps(['D:\\evil\\dep.mds']));
      const result = await transformer.transform(join(root, 'src', 'module.mds'));

      assert.deepEqual(parseMetadata(result.code).dependencies, ['dep.mds'],
        'drive-qualified dep must degrade to basename in metadata on every platform');
      assert.ok(!result.code.includes('evil'), 'foreign drive path segments must not reach the emitted code');
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test('T-J1-5: metadata deps use POSIX separators for nested paths', async () => {
    const root = makeTmpRoot('sep');
    try {
      const dep = join(root, 'lib', 'sub', 'helper.mds');
      const transformer = createMdsTransformer(mdsWithDeps([dep]));
      const result = await transformer.transform(join(root, 'src', 'module.mds'));

      const metaDeps = parseMetadata(result.code).dependencies;
      assert.deepEqual(metaDeps, ['lib/sub/helper.mds'], 'nested metadata dep must be POSIX-joined');
      for (const d of metaDeps) {
        assert.ok(!d.includes('\\'), `metadata dep must not contain backslashes: ${d}`);
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test('T-J1-6: result and metadata shapes are stable', async () => {
    const root = makeTmpRoot('shape');
    try {
      const transformer = createMdsTransformer(mdsWithDeps([join(root, 'lib', 'helper.mds')]));
      const result = await transformer.transform(join(root, 'src', 'module.mds'));

      assert.deepEqual(Object.keys(result), ['code', 'dependencies', 'warnings'],
        'TransformResult key set/order must not drift');
      assert.deepEqual(Object.keys(parseMetadata(result.code)), ['warnings', 'dependencies'],
        'metadata key set/order must not drift');
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});
