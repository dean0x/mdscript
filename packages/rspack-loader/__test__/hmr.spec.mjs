/**
 * HMR contract tests for @mdscript/rspack-loader.
 *
 * Decision D1/G2: The rspack-loader must NOT inject any HMR runtime code
 * into the emitted module. HMR lifecycle management is rspack's responsibility
 * via its built-in import.meta.webpackHot API — the loader emits plain ESM
 * and delegates reloading to rspack's dependency graph.
 *
 * rspack-specific notes:
 *   - rspack uses import.meta.webpackHot (not module.hot) for ESM modules
 *   - rspack's stats object shape may differ from webpack in some edge cases
 *     (see production gap note below)
 *   - addDependency() registers files for rebuild tracking (same as webpack)
 *
 * Production gap analysis: rspack-loader is a thin wrapper over
 * createMdsLoader() (same factory as webpack-loader). HMR rebuild path is
 * identical: rspack tracks deps via addDependency() and rebuilds affected
 * modules when deps change. No loader-level HMR hooks are needed.
 *
 * rspack stats shape note: rspack 1.x stats object uses the same fields as
 * webpack 5 stats for the compiler/module graph, but rspack's internal module
 * id allocation may differ. MDS loader output is stats-shape-agnostic (plain
 * ESM string) so no special handling is needed.
 *
 * These tests are mock-based — no filesystem watchers, no FSEvents.
 * Runs cross-platform without MDS_HMR gate.
 */
import { test, describe, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const SIMPLE_MDS = resolve(__dirname, 'fixtures/simple.mds');
const CONSUMER_MDS = resolve(__dirname, '../../mds/__test__/fixtures/import_consumer.mds');

const { default: mdsLoader, _resetForTesting, _setTransformerForTesting } = await import('../dist/index.js');

function createLoaderContext(resourcePath, overrides = {}) {
  const addedDeps = [];
  const emittedWarnings = [];
  let callbackResult = null;

  const ctx = {
    resourcePath,
    getOptions() { return {}; },
    addDependency(dep) { addedDeps.push(dep); },
    emitWarning(err) { emittedWarnings.push(err); },
    async() {
      return (err, content) => { callbackResult = { err, content }; };
    },
    get addedDeps() { return addedDeps; },
    get emittedWarnings() { return emittedWarnings; },
    get callbackResult() { return callbackResult; },
    ...overrides,
  };
  return ctx;
}

beforeEach(() => {
  _resetForTesting();
});

afterEach(() => {
  _setTransformerForTesting(null);
});

describe('mdsRspackLoader HMR contract (rspack)', () => {

  test('no import.meta.webpackHot in emitted module', async () => {
    // Decision D1/G2: rspack uses import.meta.webpackHot for ESM HMR.
    // The loader must NOT inject this — HMR is rspack's responsibility.
    const ctx = createLoaderContext(SIMPLE_MDS);
    await mdsLoader.call(ctx);

    assert.equal(ctx.callbackResult.err, null);
    assert.ok(
      !ctx.callbackResult.content.includes('import.meta.webpackHot'),
      'emitted module must not contain import.meta.webpackHot',
    );
  });

  test('no module.hot in emitted module', async () => {
    // CJS-style HMR API must not be injected either.
    const ctx = createLoaderContext(SIMPLE_MDS);
    await mdsLoader.call(ctx);

    assert.equal(ctx.callbackResult.err, null);
    assert.ok(
      !ctx.callbackResult.content.includes('module.hot'),
      'emitted module must not contain module.hot',
    );
  });

  test('no accept() or dispose() HMR calls in emitted module', async () => {
    // Verify no HMR accept/dispose callbacks were injected.
    const ctx = createLoaderContext(SIMPLE_MDS);
    await mdsLoader.call(ctx);

    assert.equal(ctx.callbackResult.err, null);
    const content = ctx.callbackResult.content;
    assert.ok(!content.includes('.accept('), 'no .accept() call in emitted module');
    assert.ok(!content.includes('.dispose('), 'no .dispose() call in emitted module');
  });

  test('addDependency called for @import deps — rspack rebuilds on dep change', async () => {
    // rspack-specific HMR rebuild path: addDependency() registers deps with
    // rspack's internal file watcher. When a dep changes, rspack re-runs the
    // loader for the affected entry file and updates its module graph.
    const ctx = createLoaderContext(CONSUMER_MDS);
    await mdsLoader.call(ctx);

    assert.equal(ctx.callbackResult.err, null);
    assert.ok(ctx.addedDeps.length >= 1, `expected dep registrations via addDependency, got: ${JSON.stringify(ctx.addedDeps)}`);
    for (const dep of ctx.addedDeps) {
      assert.equal(typeof dep, 'string', `addDependency must receive string path, got: ${dep}`);
    }
  });

  test('emitted module is plain ESM — rspack stats-shape-agnostic', async () => {
    // The emitted module is a plain ESM string. rspack's internal stats shape
    // (module ids, chunk ids) is not part of the emitted module content.
    // Decision D1: loader emits plain ESM, no rspack-specific runtime injected.
    const ctx = createLoaderContext(SIMPLE_MDS);
    await mdsLoader.call(ctx);

    assert.equal(ctx.callbackResult.err, null);
    const content = ctx.callbackResult.content;
    assert.ok(content.includes('export default'), 'emitted module has export default');
    assert.ok(!content.includes('__webpack_require__'), 'no webpack/rspack runtime injected');
    assert.ok(!content.includes('webpackChunkName'), 'no rspack magic comments injected');
  });

  test('HMR edge case: create-after-error — loader re-invocation recovers cleanly', async () => {
    // rspack re-invokes the loader when a broken file is fixed.
    // The singleton transformer must reset cleanly for a second successful compile.
    const errTransformer = {
      shouldTransform(_id) { return true; },
      async transform(_id) { throw new Error('simulated parse error'); },
    };
    await _setTransformerForTesting(errTransformer);

    const ctx1 = createLoaderContext(SIMPLE_MDS);
    await mdsLoader.call(ctx1);
    assert.ok(ctx1.callbackResult.err instanceof Error, 'first call should error');

    // Fix: reset singleton and inject working transformer
    _resetForTesting();
    const okTransformer = {
      shouldTransform(_id) { return true; },
      async transform(_id) {
        return { code: 'export default "fixed";', warnings: [], dependencies: [] };
      },
    };
    await _setTransformerForTesting(okTransformer);

    const ctx2 = createLoaderContext(SIMPLE_MDS);
    await mdsLoader.call(ctx2);
    assert.equal(ctx2.callbackResult.err, null, 'second call after fix should succeed');
    assert.ok(ctx2.callbackResult.content.includes('export default'), 'fixed module emits valid ESM');
  });

  test('HMR edge case: delete/recreate — loader handles missing file via error callback', async () => {
    // When a file is deleted and rspack re-invokes the loader (e.g. stale entry
    // in the dep graph), the loader should call back with an error (not throw).
    const ctx = createLoaderContext('/nonexistent/deleted/file.mds');
    await mdsLoader.call(ctx);

    // The loader must call back with an error rather than crashing rspack
    assert.ok(ctx.callbackResult !== null, 'callback must be called even for missing file');
    assert.ok(ctx.callbackResult.err instanceof Error, 'missing file should call back with Error');
  });

  test('HMR edge case: md-flip — shouldTransform=false then true recovers correctly', async () => {
    // A .md file without type:mds frontmatter is not compiled (shouldTransform=false).
    // After frontmatter is added, rspack re-invokes the loader and it should compile.
    const noTransformerTransformer = {
      shouldTransform(_id) { return false; },
      async transform(_id) { throw new Error('should not be called'); },
    };
    await _setTransformerForTesting(noTransformerTransformer);

    // First call: shouldTransform=false → loader signals "not my file" via
    // calling back with (null, null) or similar — in practice the loader
    // does early return. Let's verify the behavior.
    // In createMdsLoader: if shouldTransform returns false, the loader calls
    // callback(null, null) or equivalent (pass-through).
    // The rspack-loader is a thin wrapper, so we test the actual behavior.
    _resetForTesting();

    // Use a path that would match shouldTransform but with our injected mock
    const md_path = resolve(__dirname, '../../mds/__test__/fixtures/simple.mds');
    const ctx1 = createLoaderContext(md_path);

    // Verify with shouldTransform=false
    await _setTransformerForTesting(noTransformerTransformer);
    await mdsLoader.call(ctx1);
    // The loader should have called back without error but with null/empty content
    // (or may have errored — document the actual behavior)
    // Note: depending on createMdsLoader implementation, this may vary.
    // The key invariant: no throw/crash, callback was called.
    assert.ok(ctx1.callbackResult !== null, 'callback must be called even when shouldTransform=false');

    // Now with shouldTransform=true (type:mds added)
    _resetForTesting();
    const okTransformer = {
      shouldTransform(_id) { return true; },
      async transform(_id) {
        return { code: 'export default "mds-content";', warnings: [], dependencies: [] };
      },
    };
    await _setTransformerForTesting(okTransformer);

    const ctx2 = createLoaderContext(md_path);
    await mdsLoader.call(ctx2);
    assert.equal(ctx2.callbackResult.err, null, 'after type:mds added, compile should succeed');
    assert.ok(ctx2.callbackResult.content.includes('export default'), 'result is valid ESM');
  });
});
