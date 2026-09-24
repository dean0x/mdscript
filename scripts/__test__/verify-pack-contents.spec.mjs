/**
 * Tests for scripts/verify-pack-contents.mjs
 *
 * The gate's exported pure helpers are unit-tested directly against fake
 * npm-pack listings and a fake file reader — no real `npm pack` invocation is
 * needed for these cases, so this spec is hermetic and does not require a
 * built workspace. The real-tree check (the gate script run against the
 * actual, already-built packages) lives in the `js` CI job's
 * "Pack-contents gate (no source maps)" step, which runs after the
 * packages are built.
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';

import {
  WORKSPACES,
  findMapEntries,
  resolveSafePath,
  findTrailerHits,
  isTrailerCandidate,
  summarize,
} from '../verify-pack-contents.mjs';

// ---------------------------------------------------------------------------
// findMapEntries — planted positive control
// ---------------------------------------------------------------------------
describe('findMapEntries', () => {
  test('a planted .map entry in an otherwise clean listing is found', () => {
    const listing = {
      files: [
        { path: 'index.js' },
        { path: 'index.d.ts' },
        { path: 'index.js.map' },
      ],
    };
    assert.deepEqual(findMapEntries(listing), ['index.js.map']);
  });

  test('a clean listing with no .map entries returns empty', () => {
    const listing = { files: [{ path: 'index.js' }, { path: 'index.d.ts' }] };
    assert.deepEqual(findMapEntries(listing), []);
  });

  test('a .d.ts.map entry is also found (not just .js.map)', () => {
    const listing = { files: [{ path: 'index.d.ts.map' }] };
    assert.deepEqual(findMapEntries(listing), ['index.d.ts.map']);
  });
});

// ---------------------------------------------------------------------------
// resolveSafePath — traversal guard
// ---------------------------------------------------------------------------
describe('resolveSafePath', () => {
  test('a normal relative path resolves inside the workspace directory', () => {
    const dir = '/repo/packages/mds';
    assert.equal(resolveSafePath(dir, 'dist/index.js'), '/repo/packages/mds/dist/index.js');
  });

  test('a traversal path escaping the workspace directory is rejected', () => {
    const dir = '/repo/packages/mds';
    assert.equal(resolveSafePath(dir, '../../etc/passwd'), null);
  });

  test('the workspace directory itself resolves (empty-ish relative path)', () => {
    const dir = '/repo/packages/mds';
    assert.equal(resolveSafePath(dir, '.'), dir);
  });
});

// ---------------------------------------------------------------------------
// findTrailerHits — planted positive control via a fake readFn
// ---------------------------------------------------------------------------
describe('findTrailerHits', () => {
  test('a planted sourceMappingURL trailer is found via the injected reader', () => {
    const entries = [
      { workspace: '@mdscript/fake', workspaceDir: '/repo/packages/fake', path: 'dist/index.js' },
      { workspace: '@mdscript/fake', workspaceDir: '/repo/packages/fake', path: 'dist/clean.js' },
    ];
    const fakeContent = new Map([
      ['/repo/packages/fake/dist/index.js', 'const x = 1;\n//# sourceMappingURL=index.js.map\n'],
      ['/repo/packages/fake/dist/clean.js', 'const x = 1;\n'],
    ]);
    const readFn = (absPath) => fakeContent.get(absPath);
    const hits = findTrailerHits(entries, readFn);
    assert.deepEqual(hits, [{ workspace: '@mdscript/fake', path: 'dist/index.js' }]);
  });

  test('no hits when no file contains a trailer', () => {
    const entries = [{ workspace: '@mdscript/fake', workspaceDir: '/repo/packages/fake', path: 'dist/clean.js' }];
    const readFn = () => 'const x = 1;\n';
    assert.deepEqual(findTrailerHits(entries, readFn), []);
  });

  test('a traversal-escaping path is reported as a hit with a reason, never read', () => {
    const entries = [{ workspace: '@mdscript/fake', workspaceDir: '/repo/packages/fake', path: '../../etc/passwd' }];
    let readCalled = false;
    const readFn = () => { readCalled = true; return 'irrelevant'; };
    const hits = findTrailerHits(entries, readFn);
    assert.equal(readCalled, false, 'readFn must not be invoked for an escaping path');
    assert.equal(hits.length, 1);
    assert.equal(hits[0].path, '../../etc/passwd');
    assert.ok(hits[0].reason.includes('escapes'));
  });
});

// ---------------------------------------------------------------------------
// isTrailerCandidate — extension filter
// ---------------------------------------------------------------------------
describe('isTrailerCandidate', () => {
  test('accepts js/cjs/mjs/d.ts/d.cts/d.mts', () => {
    for (const p of ['x.js', 'x.cjs', 'x.mjs', 'x.d.ts', 'x.d.cts', 'x.d.mts']) {
      assert.equal(isTrailerCandidate(p), true, `expected ${p} to be a candidate`);
    }
  });

  test('rejects unrelated extensions', () => {
    for (const p of ['x.wasm', 'x.json', 'x.md', 'x.map']) {
      assert.equal(isTrailerCandidate(p), false, `expected ${p} to NOT be a candidate`);
    }
  });
});

// ---------------------------------------------------------------------------
// summarize — non-vacuity floors and success/failure shape
// ---------------------------------------------------------------------------
describe('summarize', () => {
  test('a clean run with real counts passes and reports 0 maps', () => {
    const result = summarize({ packageCount: 8, fileCount: 100, mapHits: [], trailerHits: [] });
    assert.equal(result.ok, true);
    assert.match(result.message, /^pack-contents gate: 8 packages, 100 files, 0 maps$/);
  });

  test('any mapHits entry fails regardless of floors', () => {
    const result = summarize({
      packageCount: 8,
      fileCount: 100,
      mapHits: [{ workspace: '@mdscript/mds', path: 'dist/index.js.map' }],
      trailerHits: [],
    });
    assert.equal(result.ok, false);
    assert.match(result.message, /@mdscript\/mds: ships dangling source map "dist\/index\.js\.map"/);
  });

  test('any trailerHits entry fails regardless of floors', () => {
    const result = summarize({
      packageCount: 8,
      fileCount: 100,
      mapHits: [],
      trailerHits: [{ workspace: '@mdscript/mds', path: 'dist/index.js' }],
    });
    assert.equal(result.ok, false);
    assert.match(result.message, /@mdscript\/mds: "dist\/index\.js" contains a sourceMappingURL trailer/);
  });

  test('the package-count floor trips on too few packages', () => {
    const result = summarize({ packageCount: 3, fileCount: 100, mapHits: [], trailerHits: [] });
    assert.equal(result.ok, false);
    assert.match(result.message, /non-vacuity floor: expected >= 8 packages, checked 3/);
  });

  test('the file-count floor trips on too few files', () => {
    const result = summarize({ packageCount: 8, fileCount: 2, mapHits: [], trailerHits: [] });
    assert.equal(result.ok, false);
    assert.match(result.message, /non-vacuity floor: expected >= \d+ files, checked 2/);
  });
});

// ---------------------------------------------------------------------------
// WORKSPACES — golden set (all 8 publishable packages)
// ---------------------------------------------------------------------------
describe('WORKSPACES', () => {
  test('lists exactly the 8 publishable packages', () => {
    assert.deepEqual(WORKSPACES, [
      '@mdscript/mds',
      '@mdscript/mds-wasm',
      '@mdscript/bundler-utils',
      '@mdscript/vite-plugin',
      '@mdscript/rollup-plugin',
      '@mdscript/webpack-loader',
      '@mdscript/rspack-loader',
      '@mdscript/mds-napi',
    ]);
  });
});
