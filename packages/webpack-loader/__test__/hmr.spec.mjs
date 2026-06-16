/**
 * HMR contract tests for @mdscript/webpack-loader.
 *
 * Decision D1/G2: The webpack-loader must NOT inject any HMR runtime code
 * into the emitted module. HMR lifecycle management (accept, dispose) is
 * webpack's responsibility via its built-in module.hot API — the loader
 * emits plain ESM and delegates reloading to webpack's dependency graph.
 *
 * These tests are mock-based — no filesystem watchers, no FSEvents.
 * Runs cross-platform without MDS_HMR gate.
 *
 * Production gap analysis: webpack-loader delegates HMR entirely to webpack's
 * module graph. When webpack detects that a loader dependency changed (tracked
 * via addDependency()), it rebuilds the affected modules and propagates HMR
 * through its own accept/dispose chain. The loader has no HMR hooks to add.
 */
import { test, describe, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const SIMPLE_MDS = resolve(__dirname, '../../mds/__test__/fixtures/simple.mds');

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

describe('mdsLoader HMR contract (webpack)', () => {

  test('no import.meta.webpackHot in emitted module', async () => {
    // Decision D1/G2: no HMR self-accept footer is injected.
    const ctx = createLoaderContext(SIMPLE_MDS);
    await mdsLoader.call(ctx);

    assert.equal(ctx.callbackResult.err, null);
    assert.ok(
      !ctx.callbackResult.content.includes('import.meta.webpackHot'),
      'emitted module must not contain import.meta.webpackHot',
    );
  });

  test('no module.hot in emitted module', async () => {
    // Decision D1/G2: CJS hot-replace API must not be injected either.
    const ctx = createLoaderContext(SIMPLE_MDS);
    await mdsLoader.call(ctx);

    assert.equal(ctx.callbackResult.err, null);
    assert.ok(
      !ctx.callbackResult.content.includes('module.hot'),
      'emitted module must not contain module.hot',
    );
  });

  test('no accept() or dispose() HMR calls in emitted module', async () => {
    // A loader that injects HMR would call module.hot.accept() or
    // import.meta.webpackHot.accept(). Neither should appear.
    const ctx = createLoaderContext(SIMPLE_MDS);
    await mdsLoader.call(ctx);

    assert.equal(ctx.callbackResult.err, null);
    const content = ctx.callbackResult.content;
    // These strings only appear if HMR runtime was injected
    assert.ok(!content.includes('.accept('), 'no .accept() call in emitted module');
    assert.ok(!content.includes('.dispose('), 'no .dispose() call in emitted module');
  });

  test('addDependency called for @import deps — webpack rebuilds on dep change', async () => {
    // The HMR rebuild path: webpack tracks deps via addDependency().
    // When a dep file changes, webpack re-invokes the loader and propagates
    // HMR through its module graph without loader involvement.
    const consumerMds = resolve(__dirname, '../../mds/__test__/fixtures/import_consumer.mds');
    const ctx = createLoaderContext(consumerMds);
    await mdsLoader.call(ctx);

    assert.equal(ctx.callbackResult.err, null);
    assert.ok(ctx.addedDeps.length >= 1, `expected dep registrations via addDependency, got: ${JSON.stringify(ctx.addedDeps)}`);
    // Verify each dep is a string path (not undefined/null)
    for (const dep of ctx.addedDeps) {
      assert.equal(typeof dep, 'string', `addDependency must receive a string path, got: ${dep}`);
    }
  });

  test('emitted module is plain ESM — no runtime-injected module system code', async () => {
    // Decision D1: the loader emits plain ESM (export default, export const).
    // No webpack-specific runtime (webpackChunkName, webpackMode, etc.) is added.
    const ctx = createLoaderContext(SIMPLE_MDS);
    await mdsLoader.call(ctx);

    assert.equal(ctx.callbackResult.err, null);
    const content = ctx.callbackResult.content;
    assert.ok(content.includes('export default'), 'emitted module has export default');
    assert.ok(!content.includes('webpackChunkName'), 'no webpack runtime magic comments');
    assert.ok(!content.includes('__webpack_require__'), 'no webpack runtime injected');
  });

  test('HMR edge case: create-after-error — loader re-invocation recovers cleanly', async () => {
    // Production gap check: if the loader errors for a file then the file is
    // fixed and the loader is re-invoked, the singleton transformer must handle
    // the second invocation cleanly (no stale error state).
    const errTransformer = {
      shouldTransform(_id) { return true; },
      async transform(_id) { throw new Error('simulated parse error'); },
    };
    await _setTransformerForTesting(errTransformer);

    const ctx1 = createLoaderContext(SIMPLE_MDS);
    await mdsLoader.call(ctx1);
    assert.ok(ctx1.callbackResult.err instanceof Error, 'first call should error');

    // Fix the "file" — inject working transformer and reset singleton
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
    assert.ok(ctx2.callbackResult.content.includes('export default'), 'fixed module has export default');
  });
});
