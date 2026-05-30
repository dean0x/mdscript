/**
 * Tests for @mdscript/webpack-loader.
 */
import { test, describe, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const SIMPLE_MDS = resolve(__dirname, '../../mds/__test__/fixtures/simple.mds');

// ---------------------------------------------------------------------------
// Import loader and reset helper
// ---------------------------------------------------------------------------
const { default: mdsLoader, _resetForTesting, _setTransformerForTesting } = await import('../dist/index.js');

// ---------------------------------------------------------------------------
// Mock LoaderContext factory
// ---------------------------------------------------------------------------
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
      return (err, content) => {
        callbackResult = { err, content };
      };
    },
    get addedDeps() { return addedDeps; },
    get emittedWarnings() { return emittedWarnings; },
    get callbackResult() { return callbackResult; },
    ...overrides,
  };
  return ctx;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
describe('mdsLoader', () => {
  beforeEach(() => {
    _resetForTesting();
  });

  test('default export is a function', () => {
    assert.equal(typeof mdsLoader, 'function');
  });

  test('loader calls async callback with compiled content for .mds file', async () => {
    const ctx = createLoaderContext(SIMPLE_MDS);
    await mdsLoader.call(ctx);

    assert.ok(ctx.callbackResult !== null, 'callback should have been called');
    assert.equal(ctx.callbackResult.err, null, 'should not error');
    assert.ok(
      typeof ctx.callbackResult.content === 'string',
      'content should be a string',
    );
    assert.ok(
      ctx.callbackResult.content.includes('export default'),
      'content should have export default',
    );
  });

  test('loader calls addDependency for each dependency', async () => {
    // Use import_consumer which imports import_provider
    const consumerMds = resolve(__dirname, '../../mds/__test__/fixtures/import_consumer.mds');
    const ctx = createLoaderContext(consumerMds);
    await mdsLoader.call(ctx);

    assert.equal(ctx.callbackResult.err, null);
    // The consumer imports provider so at least 1 dep
    assert.ok(ctx.addedDeps.length >= 1, `expected deps, got: ${JSON.stringify(ctx.addedDeps)}`);
  });

  test('loader calls callback with error for nonexistent file', async () => {
    const ctx = createLoaderContext('/nonexistent/path/file.mds');
    await mdsLoader.call(ctx);

    assert.ok(ctx.callbackResult !== null);
    assert.ok(ctx.callbackResult.err instanceof Error, 'should call back with error');
  });

  test('transformer singleton is reused across calls', async () => {
    const ctx1 = createLoaderContext(SIMPLE_MDS);
    const ctx2 = createLoaderContext(SIMPLE_MDS);

    // Both should succeed without double-initializing
    await mdsLoader.call(ctx1);
    await mdsLoader.call(ctx2);

    assert.equal(ctx1.callbackResult.err, null);
    assert.equal(ctx2.callbackResult.err, null);
  });

  test('_resetForTesting clears singleton state', async () => {
    const ctx1 = createLoaderContext(SIMPLE_MDS);
    await mdsLoader.call(ctx1);
    assert.equal(ctx1.callbackResult.err, null);

    // Reset and run again — should re-initialize cleanly
    _resetForTesting();
    const ctx2 = createLoaderContext(SIMPLE_MDS);
    await mdsLoader.call(ctx2);
    assert.equal(ctx2.callbackResult.err, null);
  });

  test('no warnings emitted for simple fixture', async () => {
    const ctx = createLoaderContext(SIMPLE_MDS);
    await mdsLoader.call(ctx);
    assert.equal(ctx.emittedWarnings.length, 0);
  });

  test('emitWarning called when options differ on subsequent invocation', async () => {
    // First invocation captures options.
    const ctx1 = createLoaderContext(SIMPLE_MDS, {
      getOptions() { return { vars: { env: 'prod' } }; },
    });
    await mdsLoader.call(ctx1);
    assert.equal(ctx1.callbackResult.err, null);
    assert.equal(ctx1.emittedWarnings.length, 0, 'no warning on first call');

    // Second invocation with different options should emit a warning.
    const ctx2 = createLoaderContext(SIMPLE_MDS, {
      getOptions() { return { vars: { env: 'dev' } }; },
    });
    await mdsLoader.call(ctx2);
    assert.equal(ctx2.callbackResult.err, null);
    assert.equal(ctx2.emittedWarnings.length, 1, 'should warn when options differ');
    assert.ok(
      ctx2.emittedWarnings[0].message.includes('options changed between invocations'),
      'warning message should describe the problem',
    );
  });

  test('no warning when options are identical on subsequent invocation', async () => {
    const makeCtx = () => createLoaderContext(SIMPLE_MDS, {
      getOptions() { return { vars: { env: 'prod' } }; },
    });
    await mdsLoader.call(makeCtx());
    const ctx2 = makeCtx();
    await mdsLoader.call(ctx2);
    assert.equal(ctx2.emittedWarnings.length, 0, 'identical options should not warn');
  });

  test('emitWarning called once per compiler warning, each wrapped in Error', async () => {
    // Inject a mock transformer that returns two warnings to exercise the
    // for-loop in the loader that calls this.emitWarning(new Error(warning)).
    const mockTransformer = {
      shouldTransform(_id) { return true; },
      async transform(_id) {
        return {
          code: 'export default "ok";',
          warnings: ['first warning', 'second warning'],
          dependencies: [],
        };
      },
    };
    await _setTransformerForTesting(mockTransformer);

    const ctx = createLoaderContext(SIMPLE_MDS);
    await mdsLoader.call(ctx);

    assert.equal(ctx.callbackResult.err, null, 'should not error');
    assert.equal(ctx.emittedWarnings.length, 2, 'should emit one warning per compiler warning');
    assert.ok(ctx.emittedWarnings[0] instanceof Error, 'each warning should be an Error instance');
    assert.ok(ctx.emittedWarnings[1] instanceof Error, 'each warning should be an Error instance');
    assert.equal(ctx.emittedWarnings[0].message, 'first warning');
    assert.equal(ctx.emittedWarnings[1].message, 'second warning');
  });
});
