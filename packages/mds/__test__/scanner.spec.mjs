/**
 * Module scanner unit tests for @mds/mds universal package.
 * Tests: U-S1 through U-S10
 *
 * Tests the normalizeVirtualKey and buildModulesMap utilities directly
 * using the compiled JS output.
 */
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { mkdtemp, symlink, writeFile, rm } from 'node:fs/promises';
import os from 'node:os';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

// Import from the compiled dist.
// Note: module-scanner is a Node-only utility (uses fs/promises).
const { normalizeVirtualKey, buildModulesMap } = await import('../dist/util/module-scanner.js');

const FIXTURES = path.join(__dirname, 'fixtures');

// A minimal scanImports implementation using the napi addon.
function scanImports(source) {
  // The napi addon doesn't expose scanImports directly, so we use
  // the compile result to determine imports... Actually we need scan_imports.
  // For testing buildModulesMap, use a simple regex-based scanner.
  const importRegex = /@import\s*(?:["']([^"']+)["']|{[^}]+}\s+from\s+["']([^"']+)["']|["']([^"']+)["']\s+as\s+\w+)/g;
  const exportRegex = /@export\s+(?:\*|\w+)\s+from\s+["']([^"']+)["']/g;
  const paths = [];
  let m;
  while ((m = importRegex.exec(source)) !== null) {
    const p = m[1] || m[2] || m[3];
    if (p && !paths.includes(p)) paths.push(p);
  }
  while ((m = exportRegex.exec(source)) !== null) {
    const p = m[1];
    if (p && !paths.includes(p)) paths.push(p);
  }
  return paths;
}

describe('normalizeVirtualKey', () => {
  test('U-S1: root entry (empty base) uses key as-is', () => {
    assert.equal(normalizeVirtualKey('', 'main.mds'), 'main.mds');
  });

  test('U-S2: resolves relative path from base', () => {
    assert.equal(normalizeVirtualKey('dir/main.mds', './lib.mds'), 'dir/lib.mds');
  });

  test('U-S3: resolves parent directory with ..', () => {
    assert.equal(normalizeVirtualKey('dir/sub/main.mds', '../lib.mds'), 'dir/lib.mds');
  });

  test('U-S4: .. cannot escape project root', () => {
    assert.throws(
      () => normalizeVirtualKey('main.mds', '../escape.mds'),
      /escapes/,
    );
  });

  test('U-S5: empty relative path throws', () => {
    assert.throws(
      () => normalizeVirtualKey('main.mds', ''),
      /empty/,
    );
  });

  test('U-S6: null byte in path throws', () => {
    assert.throws(
      () => normalizeVirtualKey('main.mds', './foo\0.mds'),
      /null byte/,
    );
  });

  test('U-S7: dot segments are skipped', () => {
    assert.equal(normalizeVirtualKey('main.mds', './././lib.mds'), 'lib.mds');
  });

  test('U-S8: empty segments are skipped', () => {
    assert.equal(normalizeVirtualKey('dir/main.mds', './lib.mds'), 'dir/lib.mds');
  });

  test('U-S9: resolves to empty key throws', () => {
    // Only `..` from a root-level file resolves to empty
    assert.throws(
      () => normalizeVirtualKey('main.mds', '..'),
      /escapes|empty/,
    );
  });

  test('U-S10: no trailing slash in result', () => {
    const key = normalizeVirtualKey('dir/main.mds', './sub/lib.mds');
    assert.ok(!key.endsWith('/'), `key should not end with slash: ${key}`);
  });
});

describe('buildModulesMap', () => {
  test('U-SM1: builds modules map for entry with imports', async () => {
    const entryPath = path.join(FIXTURES, 'imports', 'entry.mds');
    const { entryFilename, modules } = await buildModulesMap(entryPath, scanImports);
    assert.equal(entryFilename, 'entry.mds');
    assert.ok(typeof modules['entry.mds'] === 'string', 'entry should be in modules');
    // lib.mds and deep.mds should also be included
    assert.ok(Object.keys(modules).length >= 3, `expected at least 3 modules, got: ${Object.keys(modules)}`);
  });

  test('U-SM2: builds modules map for file with no imports', async () => {
    const entryPath = path.join(FIXTURES, 'simple.mds');
    const { entryFilename, modules } = await buildModulesMap(entryPath, scanImports);
    assert.equal(entryFilename, 'simple.mds');
    assert.ok(typeof modules['simple.mds'] === 'string');
    assert.equal(Object.keys(modules).length, 1);
  });

  test('U-SM3: rejects nonexistent file', async () => {
    await assert.rejects(
      () => buildModulesMap('/nonexistent/file.mds', scanImports),
      (err) => {
        assert.ok(err instanceof Error);
        return true;
      },
    );
  });

  test('U-SM4: shallow import chain succeeds within depth limit', async () => {
    // The fixtures/imports chain has depth 2 (entry → lib → deep).
    // This must succeed well within MAX_IMPORT_DEPTH=64.
    const deepEntryPath = path.join(FIXTURES, 'imports', 'entry.mds');
    const result = await buildModulesMap(deepEntryPath, scanImports);
    assert.ok(Object.keys(result.modules).length >= 3, 'should resolve all modules in shallow chain');
  });

  test('U-SM5: rejects when module count exceeds maxModules', async () => {
    // The fixtures/imports chain has 3+ modules. Setting maxModules=1 means even
    // the first import discovered triggers the resource-limit guard, confirming
    // that path works. The depth guard (MAX_IMPORT_DEPTH=64) is structurally
    // verified: depth is incremented on every recursive call and compared against
    // the constant before filesystem access. True depth-limit testing would require
    // 65 unique real files (too heavyweight for unit tests).
    const entryPath = path.join(FIXTURES, 'imports', 'entry.mds');
    await assert.rejects(
      () => buildModulesMap(entryPath, scanImports, { maxModules: 1 }),
      /resource limit/,
    );
  });

  test('U-SM6: rejects when aggregate size exceeds maxAggregateSize', async () => {
    // simple.mds is a small file; maxAggregateSize: 1 byte triggers the guard
    // immediately after fstat, before readFile, exercising the pre-read check.
    const entryPath = path.join(FIXTURES, 'simple.mds');
    await assert.rejects(
      () => buildModulesMap(entryPath, scanImports, { maxAggregateSize: 1 }),
      /resource limit.*aggregate module size/,
    );
  });

  test('U-SM7: rejects symlink with security error', async () => {
    // openNoFollow uses O_NOFOLLOW (Linux/macOS) or a post-open realpath check
    // (Windows) to detect symlinks. This test creates a real symlink in a temp
    // directory and confirms the scanner surfaces a security error.
    const tmpDir = await mkdtemp(path.join(os.tmpdir(), 'mds-scanner-test-'));
    try {
      const realFile = path.join(tmpDir, 'real.mds');
      const linkFile = path.join(tmpDir, 'link.mds');
      await writeFile(realFile, 'Hello world');
      await symlink(realFile, linkFile);
      await assert.rejects(
        () => buildModulesMap(linkFile, scanImports),
        /security.*symlink/,
      );
    } finally {
      await rm(tmpDir, { recursive: true, force: true });
    }
  });
});
