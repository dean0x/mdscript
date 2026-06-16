/**
 * Watch/HMR contract tests for @mdscript/rollup-plugin.
 *
 * Rollup's watch mode does not use a browser HMR protocol like Vite's WS.
 * When a watched file changes, Rollup triggers a full rebuild. The plugin
 * participates by:
 *   1. Calling this.addWatchFile(dep) for each @import dependency so Rollup
 *      knows to rebuild when dependencies change.
 *   2. NOT injecting any browser-side HMR runtime code into the emitted module.
 *
 * Production gap analysis: Rollup watch is a server-side rebuild loop.
 * There is no browser hot-module-replacement protocol — the bundled output
 * is replaced wholesale on rebuild. The plugin has no HMR hooks to add.
 *
 * These tests are mock-based — no filesystem watchers, no FSEvents.
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

function createPluginContext(overrides = {}) {
  const addedWatchFiles = [];
  const warnings = [];
  const errors = [];
  return {
    warn(msg) { warnings.push(msg); },
    addWatchFile(id) { addedWatchFiles.push(id); },
    error(msg, pos) {
      const message = typeof msg === 'string' ? msg : msg.message;
      const err = new Error(message);
      if (pos) err.pos = pos;
      errors.push(err);
      throw err;
    },
    get addedWatchFiles() { return addedWatchFiles; },
    get warnings() { return warnings; },
    get errors() { return errors; },
    ...overrides,
  };
}

beforeEach(() => {
  _setTransformerForTesting(null);
});

afterEach(() => {
  _setTransformerForTesting(null);
});

describe('mdsPlugin watch contract (rollup)', () => {

  test('addWatchFile called for each @import dep — Rollup triggers rebuild on dep change', async () => {
    // The watch rebuild path: Rollup tracks files registered via addWatchFile().
    // When a dep changes, Rollup re-runs the plugin's transform hook.
    // The plugin must register all declared @import dependencies.
    const plugin = mdsPlugin();
    const ctx = createPluginContext();
    await plugin.buildStart.call(ctx);
    await plugin.transform.call(ctx, '', CONSUMER_MDS);

    assert.ok(ctx.addedWatchFiles.length >= 1, `expected at least one addWatchFile call, got: ${JSON.stringify(ctx.addedWatchFiles)}`);
    for (const dep of ctx.addedWatchFiles) {
      assert.equal(typeof dep, 'string', `addWatchFile must receive string path, got: ${dep}`);
    }
  });

  test('no HMR runtime injected — emitted module has no module.hot', async () => {
    // Decision D1/G2: Rollup does not have a browser HMR protocol.
    // The emitted module must not contain any HMR runtime code.
    const plugin = mdsPlugin();
    const ctx = createPluginContext();
    await plugin.buildStart.call(ctx);
    const result = await plugin.transform.call(ctx, '', SIMPLE_MDS);

    assert.ok(result !== null);
    assert.ok(!result.code.includes('module.hot'), 'no module.hot in emitted module');
  });

  test('no HMR runtime injected — emitted module has no import.meta.hot', async () => {
    // Vite uses import.meta.hot for HMR — Rollup output must not contain it.
    const plugin = mdsPlugin();
    const ctx = createPluginContext();
    await plugin.buildStart.call(ctx);
    const result = await plugin.transform.call(ctx, '', SIMPLE_MDS);

    assert.ok(result !== null);
    assert.ok(!result.code.includes('import.meta.hot'), 'no import.meta.hot in emitted module');
  });

  test('emitted module is plain ESM for Rollup tree-shaking compatibility', async () => {
    // Rollup's watch mode rebuilds and re-tree-shakes. The emitted module must
    // be plain ESM (export default, export const) with no runtime injections.
    const plugin = mdsPlugin();
    const ctx = createPluginContext();
    await plugin.buildStart.call(ctx);
    const result = await plugin.transform.call(ctx, '', SIMPLE_MDS);

    assert.ok(result !== null);
    assert.ok(result.code.includes('export default'), 'emitted module has export default');
    assert.equal(result.map, null, 'Rollup plugin returns null sourcemap (by design)');
  });

  test('plugin does NOT expose handleHotUpdate (Rollup-specific, not Vite)', () => {
    // Rollup does not use Vite's handleHotUpdate hook. The plugin must not
    // expose it so consumers know the contract clearly.
    const plugin = mdsPlugin();
    assert.equal(plugin.handleHotUpdate, undefined, 'Rollup plugin must not have handleHotUpdate hook');
  });

  test('watch edge case: create-after-error — transform re-invocation after fix succeeds', async () => {
    // When a file has a parse error and is then fixed, Rollup re-invokes transform.
    // The plugin must handle the second invocation cleanly (no stale error state).
    const errTransformer = {
      shouldTransform(_id) { return true; },
      async transform(_id) { throw new Error('simulated parse error'); },
    };
    _setTransformerForTesting(errTransformer);

    const plugin = mdsPlugin();
    const ctx = createPluginContext();
    await plugin.buildStart.call(ctx);

    // First transform: throws (file has error)
    await assert.rejects(
      () => plugin.transform.call(ctx, '', SIMPLE_MDS),
      (err) => {
        assert.ok(err instanceof Error, 'should throw on parse error');
        return true;
      },
    );

    // Fix the file — inject working transformer
    const okTransformer = {
      shouldTransform(_id) { return true; },
      async transform(_id) {
        return { code: 'export default "fixed";', warnings: [], dependencies: [] };
      },
    };
    _setTransformerForTesting(okTransformer);
    // Rollup re-runs buildStart on the next build cycle
    await plugin.buildStart.call(ctx);
    const result = await plugin.transform.call(ctx, '', SIMPLE_MDS);

    assert.ok(result !== null, 'second transform after fix should succeed');
    assert.ok(result.code.includes('export default'), 'fixed module has export default');
  });

  test('watch edge case: md-flip — .md file not transformed until shouldTransform returns true', async () => {
    // A .md file starts without type:mds frontmatter (shouldTransform=false).
    // After user adds frontmatter, Rollup re-invokes transform and the plugin
    // must now compile it.
    const noFrontmatterTransformer = {
      shouldTransform(_id) { return false; },
      async transform(_id) { throw new Error('should not be called'); },
    };
    _setTransformerForTesting(noFrontmatterTransformer);

    const plugin = mdsPlugin();
    const ctx = createPluginContext();
    await plugin.buildStart.call(ctx);

    // Before type:mds: transform returns null
    const result1 = await plugin.transform.call(ctx, '', SIMPLE_MDS);
    assert.equal(result1, null, 'shouldTransform=false → transform returns null');

    // After type:mds added: inject working transformer
    const withFrontmatterTransformer = {
      shouldTransform(_id) { return true; },
      async transform(_id) {
        return { code: 'export default "with-mds";', warnings: [], dependencies: [] };
      },
    };
    _setTransformerForTesting(withFrontmatterTransformer);
    await plugin.buildStart.call(ctx);
    const result2 = await plugin.transform.call(ctx, '', SIMPLE_MDS);

    assert.ok(result2 !== null, 'shouldTransform=true → transform returns result');
    assert.ok(result2.code.includes('export default'), 'result has export default');
  });
});
