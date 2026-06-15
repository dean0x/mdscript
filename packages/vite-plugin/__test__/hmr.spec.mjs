/**
 * HMR-specific tests for @mdscript/vite-plugin (G1 fix).
 *
 * Tests the `transformed` Set + canon() path that was added in the G1 fix.
 * These complement plugin.spec.mjs tests 9-11 (the isMdsExtension fast-path)
 * by exercising:
 *   - .md files with type:mds frontmatter tracking via transform → Set
 *   - Transitive @import dependency tracking
 *   - canon() symmetry (query-stripped paths match on insert and lookup)
 *   - ctx.modules fallback for transitive dep detection
 *
 * All tests are mock-based — no filesystem watchers, no FSEvents.
 * Runs cross-platform without MDS_HMR gate.
 */
import { test, describe, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import mdsPlugin, { _setTransformerForTesting } from '../dist/index.js';

process.env.NODE_ENV = 'test';

const __dirname = dirname(fileURLToPath(import.meta.url));
const SIMPLE_MDS = resolve(__dirname, '../../mds/__test__/fixtures/simple.mds');
const CONSUMER_MDS = resolve(__dirname, '../../mds/__test__/fixtures/import_consumer.mds');

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function createPluginContext(overrides = {}) {
  const addedWatchFiles = [];
  const warnings = [];
  return {
    warn(msg) { warnings.push(msg); },
    addWatchFile(id) { addedWatchFiles.push(id); },
    get addedWatchFiles() { return addedWatchFiles; },
    get warnings() { return warnings; },
    ...overrides,
  };
}

function createHotCtx(file, sendFn, modules = undefined) {
  return {
    file,
    server: { ws: { send(payload) { sendFn(payload); } } },
    ...(modules !== undefined ? { modules } : {}),
  };
}

// A mock transformer that reports a file as needing transform and returns
// controlled output including an optional dependency list.
function makeMockTransformer({ deps = [], shouldTransform = true } = {}) {
  return {
    shouldTransform(_id) { return shouldTransform; },
    async transform(_id) {
      return {
        code: 'export default "mocked";',
        warnings: [],
        dependencies: deps,
      };
    },
  };
}

// ---------------------------------------------------------------------------
// Setup / teardown
// ---------------------------------------------------------------------------

beforeEach(() => {
  // Ensure no injected transformer leaks between tests
  _setTransformerForTesting(null);
});

afterEach(() => {
  _setTransformerForTesting(null);
});

// ---------------------------------------------------------------------------
// G1 fix: tracked-Set path (transformed Set populated via transform hook)
// ---------------------------------------------------------------------------

describe('mdsPlugin HMR — G1 fix (transformed Set + canon)', () => {

  test('handleHotUpdate triggers full-reload for .md file tracked via transform', async () => {
    // Decision AC-F6 / T-vite-md: a .md file that was compiled must trigger
    // a full-reload when changed — even though isMdsExtension('.md') is false.
    // The G1 fix achieves this via the `transformed` Set.
    const mockTransformer = makeMockTransformer();
    _setTransformerForTesting(mockTransformer);

    const plugin = mdsPlugin();
    const ctx = createPluginContext();
    await plugin.buildStart.call(ctx);

    // Transform the .md file — this populates transformed.add(canon(SIMPLE_MDS))
    await plugin.transform.call(ctx, '', SIMPLE_MDS);

    // Now simulate a hot-update for that same path
    const sentPayloads = [];
    const hotCtx = createHotCtx(SIMPLE_MDS, (p) => sentPayloads.push(p));
    const result = plugin.handleHotUpdate(hotCtx);

    assert.deepEqual(sentPayloads, [{ type: 'full-reload' }], 'should send full-reload for tracked file');
    assert.deepEqual(result, [], 'should return [] to suppress default HMR');
  });

  test('handleHotUpdate does NOT trigger full-reload for untracked .md file', async () => {
    // A .md file that has never been compiled is NOT tracked. handleHotUpdate
    // must return undefined (let Vite handle it) rather than a false positive reload.
    const plugin = mdsPlugin();
    const ctx = createPluginContext();
    // Do NOT call buildStart or transform for this file

    const sentPayloads = [];
    const hotCtx = createHotCtx('/some/untracked/file.md', (p) => sentPayloads.push(p));
    const result = plugin.handleHotUpdate(hotCtx);

    assert.deepEqual(sentPayloads, [], 'should NOT send full-reload for untracked .md file');
    assert.equal(result, undefined, 'should return undefined to let Vite handle it');
  });

  test('handleHotUpdate triggers full-reload for transitive @import dep', async () => {
    // When a .mds file @imports a dep, both the entry AND the dep are added to
    // the transformed Set. Editing the dep should trigger a full-reload.
    const depPath = resolve(__dirname, '../../mds/__test__/fixtures/import_provider.mds');
    const mockTransformer = makeMockTransformer({ deps: [depPath] });
    _setTransformerForTesting(mockTransformer);

    const plugin = mdsPlugin();
    const ctx = createPluginContext();
    await plugin.buildStart.call(ctx);

    // Transform the consumer — this registers both consumer and dep in the Set
    await plugin.transform.call(ctx, '', CONSUMER_MDS);

    // Simulate a hot-update for the dependency (dep was edited)
    const sentPayloads = [];
    const hotCtx = createHotCtx(depPath, (p) => sentPayloads.push(p));
    const result = plugin.handleHotUpdate(hotCtx);

    assert.deepEqual(sentPayloads, [{ type: 'full-reload' }], 'dep change should trigger full-reload');
    assert.deepEqual(result, [], 'should return []');
  });

  test('canon() symmetry: transform with query suffix, hot-update without suffix', async () => {
    // Vite sometimes passes ids with query suffixes to transform (e.g. ?t=123
    // for cache-busting). handleHotUpdate receives the bare path without suffix.
    // canon() must strip the suffix on BOTH sides so the Set lookup succeeds.
    const mockTransformer = makeMockTransformer();
    _setTransformerForTesting(mockTransformer);

    const plugin = mdsPlugin();
    const ctx = createPluginContext();
    await plugin.buildStart.call(ctx);

    // Transform called with query-suffixed id
    const idWithQuery = SIMPLE_MDS + '?t=12345';
    await plugin.transform.call(ctx, '', idWithQuery);

    // Hot-update called with bare path (no query suffix)
    const sentPayloads = [];
    const hotCtx = createHotCtx(SIMPLE_MDS, (p) => sentPayloads.push(p));
    const result = plugin.handleHotUpdate(hotCtx);

    assert.deepEqual(sentPayloads, [{ type: 'full-reload' }], 'canon() should match suffixed-insert to bare-lookup');
    assert.deepEqual(result, []);
  });

  test('handleHotUpdate triggers full-reload via ctx.modules (transitive module path)', () => {
    // Vite's ctx.modules array can contain modules whose id is tracked in the
    // transformed Set even when ctx.file itself is not tracked. This covers the
    // case where a JS file imports an .mds module and the JS file changes — Vite
    // includes the .mds module in ctx.modules. We should reload.
    const plugin = mdsPlugin();
    // Manually inject into the plugin's transformed Set by using the internal
    // mock transformer approach: transform, then verify module lookup path.
    // Here we simulate the scenario with a fresh plugin and no real transform
    // by noting the plugin won't have the file tracked. Instead test that when
    // ctx.modules IS populated with a tracked id, we get a reload.
    // We need to actually populate the Set first via transform.
    const ctx = createPluginContext();

    const mockTransformer = makeMockTransformer();
    _setTransformerForTesting(mockTransformer);

    // Re-create plugin after setting mock transformer
    const plugin2 = mdsPlugin();
    const ctx2 = createPluginContext();

    // Simulate: the .mds file was already transformed (ctx.modules path)
    const sentPayloads = [];

    // We can't easily test this without actually running buildStart + transform first.
    // We do it properly: buildStart → transform → handleHotUpdate with ctx.modules.
    return (async () => {
      await plugin2.buildStart.call(ctx2);
      await plugin2.transform.call(ctx2, '', SIMPLE_MDS);

      // Now simulate a hot-update where ctx.file is some JS file, but
      // ctx.modules includes a module whose id is SIMPLE_MDS (the tracked file).
      const hotCtx = {
        file: '/path/to/some/consumer.js',
        server: { ws: { send(p) { sentPayloads.push(p); } } },
        modules: [{ id: SIMPLE_MDS }],
      };
      const result = plugin2.handleHotUpdate(hotCtx);

      assert.deepEqual(sentPayloads, [{ type: 'full-reload' }], 'ctx.modules with tracked id should trigger reload');
      assert.deepEqual(result, []);
    })();
  });

  test('handleHotUpdate ctx.modules with null id entries does not throw', async () => {
    // Vite may pass modules with null ids (e.g. virtual modules with no id).
    // The G1 fix uses ?. and != null checks — verify it handles null gracefully.
    const mockTransformer = makeMockTransformer();
    _setTransformerForTesting(mockTransformer);

    const plugin = mdsPlugin();
    const ctx = createPluginContext();
    await plugin.buildStart.call(ctx);
    await plugin.transform.call(ctx, '', SIMPLE_MDS);

    const sentPayloads = [];
    const hotCtx = {
      file: '/path/to/consumer.js',
      server: { ws: { send(p) { sentPayloads.push(p); } } },
      // Mix of null and non-tracked ids — none should match
      modules: [{ id: null }, { id: '/path/to/untracked.js' }, { id: null }],
    };
    // Should not throw, and since ctx.file is not tracked either:
    // the untracked consumer.js should not trigger a reload
    assert.doesNotThrow(() => {
      const result = plugin.handleHotUpdate(hotCtx);
      // ctx.file is not in transformed, modules don't match → undefined
      assert.equal(result, undefined);
    });
  });

  test('multiple transforms accumulate in the transformed Set (bounded growth)', async () => {
    // The Set grows with each new file transformed. Verify it tracks each distinct
    // file and is bounded (no duplicates for same file).
    const mockTransformer = makeMockTransformer();
    _setTransformerForTesting(mockTransformer);

    const plugin = mdsPlugin();
    const ctx = createPluginContext();
    await plugin.buildStart.call(ctx);

    // Transform same file twice — should not double-trigger
    await plugin.transform.call(ctx, '', SIMPLE_MDS);
    await plugin.transform.call(ctx, '', SIMPLE_MDS);

    const sentPayloads = [];
    const hotCtx = createHotCtx(SIMPLE_MDS, (p) => sentPayloads.push(p));
    plugin.handleHotUpdate(hotCtx);

    // Should send exactly one full-reload (not two)
    assert.equal(sentPayloads.length, 1, 'Set deduplication: one reload per change event');
    assert.deepEqual(sentPayloads[0], { type: 'full-reload' });
  });
});

// ---------------------------------------------------------------------------
// Suite 3 edge cases — mock-based observation of G1 behavior
// ---------------------------------------------------------------------------

describe('mdsPlugin HMR edge cases (Suite 3)', () => {

  test('create-after-error: file that errored then compiled succeeds triggers reload on next change', async () => {
    // If transform throws for a file (e.g. parse error), the file is NOT added
    // to the transformed Set. Once the file is fixed and transform succeeds, the
    // Set is populated. Subsequent hot-updates should trigger a reload.
    //
    // We use a .md path (not .mds) so the isMdsExtension fast-path does NOT fire
    // — this isolates the transformed-Set behavior from the extension check.
    // The path does not need to exist on disk; the mock transformer controls all behavior.
    const MD_PATH = '/tmp/mds-test-suite3/page.md';

    const errTransformer = {
      shouldTransform(_id) { return true; },
      async transform(_id) { throw new Error('parse error — simulated'); },
    };
    _setTransformerForTesting(errTransformer);

    const plugin = mdsPlugin();
    const ctx = createPluginContext();
    await plugin.buildStart.call(ctx);

    // First transform attempt throws — file NOT added to Set
    try {
      await plugin.transform.call(ctx, '', MD_PATH);
    } catch {
      // Expected — error is intentional
    }

    // Verify: hot-update does NOT trigger reload (file not tracked, not .mds extension)
    const payloads1 = [];
    plugin.handleHotUpdate(createHotCtx(MD_PATH, (p) => payloads1.push(p)));
    assert.deepEqual(payloads1, [], 'errored .md file should not be in tracked Set → no reload');

    // Now inject a working transformer (file is "fixed")
    const okTransformer = makeMockTransformer();
    _setTransformerForTesting(okTransformer);
    await plugin.buildStart.call(ctx);
    await plugin.transform.call(ctx, '', MD_PATH);

    // Verify: hot-update NOW triggers reload (file tracked after successful compile)
    const payloads2 = [];
    plugin.handleHotUpdate(createHotCtx(MD_PATH, (p) => payloads2.push(p)));
    assert.deepEqual(payloads2, [{ type: 'full-reload' }], 'fixed .md file should be tracked → reload on change');
  });

  test('md-flip: .md file gaining type:mds causes reload after next transform', async () => {
    // A .md file starts without type:mds — shouldTransform returns false.
    // User adds type:mds frontmatter — on next transform, shouldTransform returns
    // true and the file enters the tracked Set.
    //
    // We use a .md path so the isMdsExtension fast-path does NOT fire — this
    // purely tests the transformed-Set behavior.
    const MD_PATH = '/tmp/mds-test-suite3/doc.md';

    const noFrontmatterTransformer = {
      shouldTransform(_id) { return false; },
      async transform(_id) { throw new Error('should not be called'); },
    };
    _setTransformerForTesting(noFrontmatterTransformer);

    const plugin = mdsPlugin();
    const ctx = createPluginContext();
    await plugin.buildStart.call(ctx);

    // First transform: shouldTransform returns false → not tracked
    const result1 = await plugin.transform.call(ctx, '', MD_PATH);
    assert.equal(result1, null, 'shouldTransform=false → transform returns null');

    // hot-update should NOT trigger reload (file not tracked, not .mds extension)
    const payloads1 = [];
    plugin.handleHotUpdate(createHotCtx(MD_PATH, (p) => payloads1.push(p)));
    assert.deepEqual(payloads1, [], 'untracked .md file (no type:mds) should not trigger reload');

    // User adds type:mds — shouldTransform now returns true
    const withFrontmatterTransformer = makeMockTransformer();
    _setTransformerForTesting(withFrontmatterTransformer);
    await plugin.buildStart.call(ctx);
    const result2 = await plugin.transform.call(ctx, '', MD_PATH);
    assert.ok(result2 !== null, 'shouldTransform=true → transform returns result');

    // hot-update SHOULD now trigger reload (file is now tracked)
    const payloads2 = [];
    plugin.handleHotUpdate(createHotCtx(MD_PATH, (p) => payloads2.push(p)));
    assert.deepEqual(payloads2, [{ type: 'full-reload' }], 'tracked .md file should trigger reload after type:mds added');
  });

  test('delete/recreate: stale entry in Set causes one extra reload check but no crash', async () => {
    // After a file is deleted, canon() falls back to resolvePath (no realpathSync).
    // The stale entry in the Set may or may not match the new file's canon path,
    // but it must not throw. This verifies the canon() catch block works.
    const mockTransformer = makeMockTransformer();
    _setTransformerForTesting(mockTransformer);

    const plugin = mdsPlugin();
    const ctx = createPluginContext();
    await plugin.buildStart.call(ctx);

    // Transform an existing file (populates Set with real canon path)
    await plugin.transform.call(ctx, '', SIMPLE_MDS);

    // Simulate hot-update for a path that no longer exists on disk
    // (the file was deleted — canon() will catch realpathSync failure and
    // fall back to resolvePath)
    const deletedPath = '/nonexistent/deleted/file.mds';
    const payloads = [];
    assert.doesNotThrow(() => {
      plugin.handleHotUpdate(createHotCtx(deletedPath, (p) => payloads.push(p)));
    }, 'handleHotUpdate for deleted path must not throw');
    // A bare .mds path hits isMdsExtension fast-path → full-reload
    // (this is expected behavior: we don't know it was deleted)
    assert.deepEqual(payloads, [{ type: 'full-reload' }], '.mds extension fast-path still applies to deleted file path');
  });
});
