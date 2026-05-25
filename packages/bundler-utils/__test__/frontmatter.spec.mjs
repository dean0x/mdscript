/**
 * Tests for frontmatter detection utilities.
 */
import { test, describe, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { writeFileSync, unlinkSync, mkdirSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { shouldTransform, isMdsExtension, cleanId } from '../dist/index.js';

// ---------------------------------------------------------------------------
// Temp file helpers
// ---------------------------------------------------------------------------
const TMP = join(tmpdir(), `mds-bundler-utils-test-${process.pid}`);

let mdWithTypeInFrontmatter = '';
let mdWithoutType = '';
let mdWithTypeInBody = '';
let mdEmpty = '';
let mdDashesOnly = '';
let mdTypeAfterClose = '';

before(() => {
  mkdirSync(TMP, { recursive: true });

  mdWithTypeInFrontmatter = join(TMP, 'with-type.md');
  writeFileSync(mdWithTypeInFrontmatter, '---\ntitle: hi\ntype: mds\n---\nContent\n');

  mdWithoutType = join(TMP, 'without-type.md');
  writeFileSync(mdWithoutType, '---\ntitle: hi\n---\nContent without type\n');

  mdWithTypeInBody = join(TMP, 'type-in-body.md');
  writeFileSync(mdWithTypeInBody, '---\ntitle: hi\n---\ntype: mds in body\n');

  mdEmpty = join(TMP, 'empty.md');
  writeFileSync(mdEmpty, '');

  mdDashesOnly = join(TMP, 'dashes-only.md');
  writeFileSync(mdDashesOnly, '---\ntitle: hi\n');

  mdTypeAfterClose = join(TMP, 'type-after-close.md');
  writeFileSync(mdTypeAfterClose, '---\ntitle: hi\n---\n\ntype: mds\n');
});

after(() => {
  const files = [
    mdWithTypeInFrontmatter,
    mdWithoutType,
    mdWithTypeInBody,
    mdEmpty,
    mdDashesOnly,
    mdTypeAfterClose,
  ];
  for (const f of files) {
    try { unlinkSync(f); } catch { /* ignore */ }
  }
  try { rmSync(TMP, { recursive: true }); } catch { /* ignore */ }
});

// ---------------------------------------------------------------------------
// isMdsExtension
// ---------------------------------------------------------------------------
describe('isMdsExtension', () => {
  test('returns true for .mds file', () => {
    assert.equal(isMdsExtension('/path/to/file.mds'), true);
  });

  test('returns false for .md file', () => {
    assert.equal(isMdsExtension('/path/to/file.md'), false);
  });

  test('returns false for .txt file', () => {
    assert.equal(isMdsExtension('/path/to/file.txt'), false);
  });

  test('returns false for .ts file', () => {
    assert.equal(isMdsExtension('/path/to/file.ts'), false);
  });

  test('returns true for .mds with query params (pre-clean)', () => {
    // isMdsExtension checks the raw string including query params
    // Callers are expected to cleanId first
    assert.equal(isMdsExtension('file.mds'), true);
  });
});

// ---------------------------------------------------------------------------
// cleanId
// ---------------------------------------------------------------------------
describe('cleanId', () => {
  test('passthrough when no query or hash', () => {
    assert.equal(cleanId('/path/to/file.mds'), '/path/to/file.mds');
  });

  test('strips ?query', () => {
    assert.equal(cleanId('/path/to/file.mds?inline'), '/path/to/file.mds');
  });

  test('strips #hash', () => {
    assert.equal(cleanId('/path/to/file.mds#section'), '/path/to/file.mds');
  });

  test('strips both ?query and #hash', () => {
    assert.equal(cleanId('/path/to/file.mds?inline#section'), '/path/to/file.mds');
  });

  test('preserves path with no special chars', () => {
    assert.equal(cleanId('simple'), 'simple');
  });
});

// ---------------------------------------------------------------------------
// shouldTransform
// ---------------------------------------------------------------------------
describe('shouldTransform', () => {
  test('.mds file returns true synchronously', () => {
    const result = shouldTransform('/path/to/file.mds');
    assert.equal(result, true);
  });

  test('.txt file returns false synchronously', () => {
    const result = shouldTransform('/path/to/file.txt');
    assert.equal(result, false);
  });

  test('.ts file returns false synchronously', () => {
    const result = shouldTransform('/path/to/file.ts');
    assert.equal(result, false);
  });

  test('.md with type: mds in frontmatter returns true (async)', async () => {
    const result = await shouldTransform(mdWithTypeInFrontmatter);
    assert.equal(result, true);
  });

  test('.md without type returns false (async)', async () => {
    const result = await shouldTransform(mdWithoutType);
    assert.equal(result, false);
  });

  test('.md with type: mds in body (after frontmatter close) returns false', async () => {
    const result = await shouldTransform(mdWithTypeInBody);
    assert.equal(result, false);
  });

  test('empty .md file returns false', async () => {
    const result = await shouldTransform(mdEmpty);
    assert.equal(result, false);
  });

  test('.md with only opening --- (no closing) returns false', async () => {
    const result = await shouldTransform(mdDashesOnly);
    assert.equal(result, false);
  });

  test('.md with type: mds appearing after frontmatter close returns false', async () => {
    const result = await shouldTransform(mdTypeAfterClose);
    assert.equal(result, false);
  });

  test('nonexistent .md file returns false gracefully', async () => {
    const result = await shouldTransform('/nonexistent/path/file.md');
    assert.equal(result, false);
  });
});
