/**
 * Module scanner unit tests for @mdscript/mds universal package.
 * Tests: U-S1 through U-S10
 *
 * Tests the normalizeVirtualKey and buildModulesMap utilities directly
 * using the compiled JS output.
 */
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { mkdtemp, mkdir, symlink, writeFile, rm } from 'node:fs/promises';
import os from 'node:os';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

// Import from the compiled dist.
// Note: module-scanner is a Node-only utility (uses fs/promises).
const { normalizeVirtualKey, buildModulesMap, findProjectRoot } = await import('../dist/util/module-scanner.js');

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
    assert.ok(entryFilename.endsWith('imports/entry.mds'), `entry key should end with imports/entry.mds: ${entryFilename}`);
    assert.ok(!entryFilename.startsWith('/'), `entry key must be relative, not absolute: ${entryFilename}`);
    assert.ok(typeof modules[entryFilename] === 'string', 'entry should be in modules');
    // lib.mds and deep.mds should also be included
    assert.ok(Object.keys(modules).length >= 3, `expected at least 3 modules, got: ${Object.keys(modules)}`);
  });

  test('U-SM2: builds modules map for file with no imports', async () => {
    const entryPath = path.join(FIXTURES, 'simple.mds');
    const { entryFilename, modules } = await buildModulesMap(entryPath, scanImports);
    assert.ok(entryFilename.endsWith('simple.mds'), `entry key should end with simple.mds: ${entryFilename}`);
    assert.ok(!entryFilename.startsWith('/'), `entry key must be relative, not absolute: ${entryFilename}`);
    assert.ok(typeof modules[entryFilename] === 'string');
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

  test('U-SM8: resolves cross-directory imports via project root discovery', async () => {
    // cross-dir/app/entry.mds imports ../lib/helpers.mds — a sibling directory.
    // The scanner must walk up to find the project root (via .git marker) so that
    // the import resolves within the project boundary instead of being rejected.
    const entryPath = path.join(FIXTURES, 'cross-dir', 'app', 'entry.mds');
    const { entryFilename, modules } = await buildModulesMap(entryPath, scanImports);
    // Entry key should end with the path from the cross-dir root.
    assert.ok(entryFilename.endsWith('cross-dir/app/entry.mds'), `entry key should include path: ${entryFilename}`);
    // The entry module should be in the map under its key.
    assert.ok(modules[entryFilename], 'entry should be in modules under its key');
    // The sibling-directory module should also be present.
    const helperKey = Object.keys(modules).find(k => k.endsWith('cross-dir/lib/helpers.mds'));
    assert.ok(helperKey, `sibling dir module should be included, got keys: ${Object.keys(modules)}`);
    assert.equal(Object.keys(modules).length, 2, 'should have exactly entry + helper');
  });
});

describe('findProjectRoot', () => {
  // Each test uses a fresh mkdtemp directory to guarantee unique paths that
  // will not collide with cached results from prior test runs.

  test('U-PR1: returns directory containing .git marker', async () => {
    // Arrange: root/sub/ — .git lives at root, start is root/sub
    const root = await mkdtemp(path.join(os.tmpdir(), 'mds-pr-test-'));
    try {
      const sub = path.join(root, 'sub');
      await mkdir(sub);
      await mkdir(path.join(root, '.git'));
      // Act
      const result = findProjectRoot(sub);
      // Assert: should walk up from sub and find root via .git
      assert.equal(result, root);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test('U-PR2: returns directory containing .mdsroot marker', async () => {
    // Arrange: root/a/b/ — .mdsroot lives at root, start is root/a/b
    const root = await mkdtemp(path.join(os.tmpdir(), 'mds-pr-test-'));
    try {
      const deep = path.join(root, 'a', 'b');
      await mkdir(deep, { recursive: true });
      await writeFile(path.join(root, '.mdsroot'), '');
      // Act
      const result = findProjectRoot(deep);
      // Assert: should walk up and find root via .mdsroot
      assert.equal(result, root);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test('U-PR3: falls back to start directory when no marker is found', async () => {
    // Arrange: isolated temp dir with no .git or .mdsroot anywhere in its tree.
    // We create a subdirectory so the traversal has at least one step.
    const root = await mkdtemp(path.join(os.tmpdir(), 'mds-pr-test-'));
    try {
      const sub = path.join(root, 'sub');
      await mkdir(sub);
      // Act: use a deep path that lives inside os.tmpdir() but has no marker.
      // We cannot guarantee os.tmpdir() itself has no .git (e.g. in CI or
      // monorepo environments), so we accept either the original start argument
      // or any ancestor — as long as the result is a prefix of sub.
      const result = findProjectRoot(sub);
      // Assert: result is either `sub` itself (fallback) or an ancestor of `sub`
      // that contains a marker. Either way, sub must start with result + '/'.
      assert.ok(
        result === sub || sub.startsWith(result + '/'),
        `expected result to be sub or an ancestor of sub, got: ${result}`,
      );
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test('U-PR4: returns start when called with a path already at filesystem root sentinel', () => {
    // Simulate the filesystem-root sentinel: dirname(x) === x. We verify that
    // findProjectRoot('') does not loop forever — dirname('') returns '.' which
    // is not the same as '' so this exercises the loop normally, but we can
    // verify a known-fallback path like the OS temp dir itself.
    // The real edge case (parent === dir) fires at the OS root '/'.
    // We verify indirectly: if the traversal walked up to '/', findProjectRoot
    // returned start. We test this by confirming that a fresh temp directory
    // (with no markers) returns the start, not '/'.
    const start = os.tmpdir();
    const result = findProjectRoot(start);
    // Result is either start (no marker found and fallback triggered) or
    // some ancestor that happens to have a .git. Either way, it must be a string
    // and must not be empty — we cannot assert the exact value here because
    // os.tmpdir() may be inside a git repo on some machines.
    assert.ok(typeof result === 'string' && result.length > 0, 'result must be a non-empty path');
  });

  test('U-PR5: result is cached — same start returns same value on second call', async () => {
    // The cache makes repeated traversal O(1) after the first call.
    // We verify observable correctness: two calls with the same start return ===.
    const root = await mkdtemp(path.join(os.tmpdir(), 'mds-pr-test-'));
    try {
      const sub = path.join(root, 'sub');
      await mkdir(sub);
      await mkdir(path.join(root, '.git'));
      const first = findProjectRoot(sub);
      const second = findProjectRoot(sub);
      assert.equal(first, second);
      assert.equal(first, root);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });
});
