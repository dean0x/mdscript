/**
 * Integration tests for @mdscript/bundler-utils using real @mdscript/mds.
 */
import { test, describe, before } from 'node:test';
import assert from 'node:assert/strict';
import { resolve, dirname, join, isAbsolute } from 'node:path';
import { fileURLToPath } from 'node:url';
import { writeFileSync, unlinkSync, mkdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { createMdsTransformer } from '../dist/index.js';

const __dirname = dirname(fileURLToPath(import.meta.url));
const FIXTURES = resolve(__dirname, '../../mds/__test__/fixtures');
const SIMPLE_MDS = join(FIXTURES, 'simple.mds');
const CONSUMER_MDS = join(FIXTURES, 'import_consumer.mds');
const ENTRY_MDS = join(FIXTURES, 'imports/entry.mds');

// ---------------------------------------------------------------------------
// Load real @mdscript/mds
// ---------------------------------------------------------------------------
const mds = await import('../../mds/dist/node.js');
await mds.init();

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
describe('bundler-utils integration', () => {
  test('compiles simple .mds file to valid JS module', async () => {
    const transformer = createMdsTransformer(mds);
    const result = await transformer.transform(SIMPLE_MDS);

    assert.ok(result.code.includes('export default'), 'should have default export');
    assert.ok(result.code.includes('export const metadata'), 'should have metadata export');
    assert.ok(Array.isArray(result.dependencies));
    assert.ok(Array.isArray(result.warnings));
  });

  test('compiles file with imports and returns dependencies', async () => {
    const transformer = createMdsTransformer(mds);
    const result = await transformer.transform(CONSUMER_MDS);

    assert.ok(result.code.includes('export default'), 'should have default export');
    assert.ok(result.dependencies.length >= 1, 'should have at least one dependency');
    // The dependency should be an absolute path (cross-platform: POSIX `/…`,
    // Windows `D:\…` or `\\?\D:\…`).
    for (const dep of result.dependencies) {
      assert.ok(isAbsolute(dep), `dependency should be absolute path: ${dep}`);
    }
  });

  test('compiles deep import chain', async () => {
    const transformer = createMdsTransformer(mds);
    const result = await transformer.transform(ENTRY_MDS);

    assert.ok(result.code.includes('export default'), 'should have default export');
    assert.equal(typeof result.code, 'string');
  });

  test('.md file with type: mds frontmatter is transformed', async () => {
    // Create a temp .md file with type: mds frontmatter
    const tmpDir = join(tmpdir(), `mds-integration-${process.pid}`);
    mkdirSync(tmpDir, { recursive: true });
    const tmpMd = join(tmpDir, 'test.md');
    writeFileSync(tmpMd, '---\ntype: mds\nname: World\n---\nHello {name}!\n');

    try {
      const transformer = createMdsTransformer(mds);
      const should = await transformer.shouldTransform(tmpMd);
      assert.equal(should, true, '.md with type: mds should be transformed');

      const result = await transformer.transform(tmpMd);
      assert.ok(result.code.includes('export default'), 'should have default export');
      // The output should contain compiled content
      assert.ok(result.code.includes('Hello World!'), `expected "Hello World!" in: ${result.code}`);
    } finally {
      try { rmSync(tmpDir, { recursive: true }); } catch { /* ignore */ }
    }
  });

  test('output string is properly escaped in JS', async () => {
    // Create a temp file with special chars that need escaping
    const tmpDir = join(tmpdir(), `mds-integration-esc-${process.pid}`);
    mkdirSync(tmpDir, { recursive: true });
    const tmpFile = join(tmpDir, 'special.mds');
    writeFileSync(tmpFile, 'Line 1\nLine 2\n"quoted"\n');

    try {
      const transformer = createMdsTransformer(mds);
      const result = await transformer.transform(tmpFile);

      // The code should have escaped newlines, not raw newlines in the string.
      // The export default must be a single complete line with no embedded literal newlines.
      const exportLine = result.code.split('\n')[0] ?? '';
      assert.ok(exportLine.startsWith('export default "'), 'first line should be export default');
      assert.ok(exportLine.endsWith('";'), 'export default should end on same line');
      assert.ok(exportLine.includes('\\n'), 'should have escaped newline');
    } finally {
      try { rmSync(tmpDir, { recursive: true }); } catch { /* ignore */ }
    }
  });
});
