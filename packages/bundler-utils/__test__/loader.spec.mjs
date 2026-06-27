/**
 * Unit tests for createMdsLoader() factory (bundler-utils/src/loader.ts).
 *
 * Coverage:
 *  T-C3: factory shape, per-instance isolation, real-import() CJS path
 *  T-C5: vars forwarding, options-changed warning, identical-options no-warning
 *  T-P3: init() called once across N transforms (warm reuse)
 *
 * All tests are cross-OS (no watcher, no filesystem events).
 */
import { test, describe, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createMdsLoader } from '../dist/index.js';

const __dirname = dirname(fileURLToPath(import.meta.url));
const SIMPLE_MDS = resolve(__dirname, '../../mds/__test__/fixtures/simple.mds');

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/**
 * Build a minimal LoaderContext mock suitable for the loader function.
 */
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

/**
 * Build a minimal mock MdsApi.
 * @param {object} [overrides]
 * @returns {{ mdsApi: object, initCallCount: () => number, compileCallCount: () => number }}
 */
function createMockMdsApi(overrides = {}) {
  let initCalls = 0;
  let compileCalls = 0;

  const mdsApi = {
    async init() { initCalls++; },
    async compileFile(path, _options) {
      compileCalls++;
      return {
        kind: 'markdown',
        output: `compiled:${path}`,
        warnings: [],
        dependencies: [],
      };
    },
    ...overrides,
  };

  return {
    mdsApi,
    initCallCount() { return initCalls; },
    compileCallCount() { return compileCalls; },
  };
}

// ---------------------------------------------------------------------------
// T-C3: Factory shape and per-instance isolation
// ---------------------------------------------------------------------------
describe('createMdsLoader — T-C3: factory shape', () => {
  test('factory returns object with exactly loader, _resetForTesting, _setTransformerForTesting', () => {
    const instance = createMdsLoader();
    const keys = Object.keys(instance).sort();
    assert.deepEqual(
      keys,
      ['_resetForTesting', '_setTransformerForTesting', 'loader'].sort(),
      'factory must return exactly these three keys',
    );
    assert.equal(typeof instance.loader, 'function', 'loader must be a function');
    assert.equal(typeof instance._resetForTesting, 'function', '_resetForTesting must be a function');
    assert.equal(typeof instance._setTransformerForTesting, 'function', '_setTransformerForTesting must be a function');
  });

  test('two factory instances with distinct mock transformers do NOT cross-contaminate', async () => {
    const { loader: loaderA, _setTransformerForTesting: setA, _resetForTesting: resetA } = createMdsLoader();
    const { loader: loaderB, _setTransformerForTesting: setB, _resetForTesting: resetB } = createMdsLoader();

    const sentinelA = {
      shouldTransform() { return true; },
      async transform(_id) {
        return { code: 'export default "sentinel-A";', warnings: [], dependencies: [] };
      },
    };
    const sentinelB = {
      shouldTransform() { return true; },
      async transform(_id) {
        return { code: 'export default "sentinel-B";', warnings: [], dependencies: [] };
      },
    };

    setA(sentinelA);
    setB(sentinelB);

    const ctxA = createLoaderContext(SIMPLE_MDS);
    const ctxB = createLoaderContext(SIMPLE_MDS);
    await loaderA.call(ctxA);
    await loaderB.call(ctxB);

    assert.ok(
      ctxA.callbackResult.content.includes('sentinel-A'),
      `loaderA should use sentinelA, got: ${ctxA.callbackResult.content}`,
    );
    assert.ok(
      ctxB.callbackResult.content.includes('sentinel-B'),
      `loaderB should use sentinelB, got: ${ctxB.callbackResult.content}`,
    );
    assert.ok(
      !ctxA.callbackResult.content.includes('sentinel-B'),
      'loaderA must NOT see sentinelB output',
    );
    assert.ok(
      !ctxB.callbackResult.content.includes('sentinel-A'),
      'loaderB must NOT see sentinelA output',
    );

    resetA();
    resetB();
  });

  test('T-C3 real-import() CJS path: compiles simple.mds without injected transformer', async () => {
    // This test exercises the moved new Function() shim in loader.ts.
    // No mock transformer is injected — the real @mdscript/mds must be loaded
    // dynamically via the CJS-safe esmImport wrapper.
    // FALLBACK GATE: if this test fails, the factory extraction is broken and
    // the fallback (duplicate source in rspack-loader) must be taken instead.
    const { loader, _resetForTesting } = createMdsLoader();
    _resetForTesting();

    const ctx = createLoaderContext(SIMPLE_MDS);
    await loader.call(ctx);

    assert.equal(ctx.callbackResult.err, null, 'real-import() path must not error');
    assert.ok(
      typeof ctx.callbackResult.content === 'string' &&
        ctx.callbackResult.content.includes('export default'),
      `real-import() path must return compiled JS module, got: ${ctx.callbackResult.content}`,
    );

    _resetForTesting();
  });
});

// ---------------------------------------------------------------------------
// T-C5: vars forwarding, options-changed warning, identical-options no-warning
// ---------------------------------------------------------------------------
describe('createMdsLoader — T-C5: options semantics', () => {
  let loaderInstance;

  beforeEach(() => {
    loaderInstance = createMdsLoader();
  });

  afterEach(() => {
    loaderInstance._resetForTesting();
  });

  test('vars forwarded to compileFile verbatim', async () => {
    let capturedVars;

    const { mdsApi } = createMockMdsApi({
      async compileFile(_path, options) {
        capturedVars = options?.vars;
        return { kind: 'markdown', output: 'ok', warnings: [], dependencies: [] };
      },
    });

    // We need to inject the mock MDS API via the transformer for this test.
    // Use _setTransformerForTesting with a transformer built from our mock MDS.
    const { createMdsTransformer } = await import('../dist/index.js');
    const vars = { env: 'production', version: 42 };
    const transformer = createMdsTransformer(mdsApi, { vars });
    loaderInstance._setTransformerForTesting(transformer);

    const ctx = createLoaderContext(SIMPLE_MDS);
    await loaderInstance.loader.call(ctx);

    assert.deepEqual(capturedVars, vars, 'vars must be forwarded verbatim to compileFile');
  });

  test('vars absent from compileFile when not set in options', async () => {
    let capturedOptions;

    const { mdsApi } = createMockMdsApi({
      async compileFile(_path, options) {
        capturedOptions = options;
        return { kind: 'markdown', output: 'ok', warnings: [], dependencies: [] };
      },
    });

    const { createMdsTransformer } = await import('../dist/index.js');
    const transformer = createMdsTransformer(mdsApi); // no vars
    loaderInstance._setTransformerForTesting(transformer);

    const ctx = createLoaderContext(SIMPLE_MDS);
    await loaderInstance.loader.call(ctx);

    // When no vars set, options passed to compileFile should be undefined
    assert.equal(capturedOptions, undefined, 'options should be undefined when no vars set');
  });

  test('options-changed warning fires once on second call with different options', async () => {
    const { loader, _resetForTesting } = createMdsLoader();
    _resetForTesting();

    // First invocation with vars: { env: 'prod' }
    const ctx1 = createLoaderContext(SIMPLE_MDS, {
      getOptions() { return { vars: { env: 'prod' } }; },
    });
    await loader.call(ctx1);
    assert.equal(ctx1.emittedWarnings.length, 0, 'no warning on first call');

    // Second invocation with different options
    const ctx2 = createLoaderContext(SIMPLE_MDS, {
      getOptions() { return { vars: { env: 'dev' } }; },
    });
    await loader.call(ctx2);
    assert.equal(ctx2.emittedWarnings.length, 1, 'should warn once when options differ');
    assert.ok(
      ctx2.emittedWarnings[0].message.includes('options changed between invocations'),
      `warning message should describe the problem, got: ${ctx2.emittedWarnings[0].message}`,
    );

    _resetForTesting();
  });

  test('identical options across calls produce no warning', async () => {
    const { loader, _resetForTesting } = createMdsLoader();
    _resetForTesting();

    const makeCtx = () => createLoaderContext(SIMPLE_MDS, {
      getOptions() { return { vars: { env: 'prod' } }; },
    });

    await loader.call(makeCtx());
    const ctx2 = makeCtx();
    await loader.call(ctx2);
    assert.equal(ctx2.emittedWarnings.length, 0, 'identical options should not warn');

    _resetForTesting();
  });
});

// ---------------------------------------------------------------------------
// T-P3: init() called once across N transforms (warm reuse)
// ---------------------------------------------------------------------------
describe('createMdsLoader — T-P3: init warm reuse', () => {
  let loaderInstance;

  beforeEach(() => {
    loaderInstance = createMdsLoader();
  });

  afterEach(() => {
    loaderInstance._resetForTesting();
  });

  test('init() called exactly once across multiple transform() calls', async () => {
    const { mdsApi, initCallCount } = createMockMdsApi();
    const { createMdsTransformer } = await import('../dist/index.js');
    const transformer = createMdsTransformer(mdsApi);
    loaderInstance._setTransformerForTesting(transformer);

    const N = 5;
    for (let i = 0; i < N; i++) {
      const ctx = createLoaderContext(SIMPLE_MDS);
      await loaderInstance.loader.call(ctx);
      assert.equal(ctx.callbackResult.err, null, `call ${i + 1} should not error`);
    }

    assert.equal(initCallCount(), 1, `init() must be called exactly once across ${N} transforms`);
  });
});
