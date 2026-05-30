/**
 * CJS compatibility tests for @mdscript/webpack-loader.
 *
 * Verifies that the CJS build (dist-cjs/) can be loaded via require() and
 * exports the default loader function. This is the primary condition for
 * Webpack 5 interoperability — Webpack resolves loaders using require().
 */
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';
import { existsSync } from 'node:fs';

const __dirname = dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);

const cjsPath = resolve(__dirname, '../dist-cjs/index.js');
const hasCjsBuild = existsSync(cjsPath);

describe('webpack-loader CJS build', { skip: !hasCjsBuild && 'dist-cjs/ not built — run build first' }, () => {
  const cjsBuild = require(cjsPath);
  const { default: mdsLoader, _resetForTesting, _setTransformerForTesting } = cjsBuild;

  test('loads without error via require()', () => {
    assert.ok(cjsBuild, 'CJS build should load successfully');
  });

  test('exports default as an async function (the loader)', () => {
    assert.equal(typeof mdsLoader, 'function', 'default export should be a function');
    // Webpack loaders must return a Promise. Verify the behavioral contract by
    // invoking the loader with a minimal mock context that satisfies its
    // interface: async() returns a no-op callback, getOptions() returns {}.
    // We only check the return type — we do not assert on side effects.
    const mockContext = {
      resourcePath: '/dev/null/nonexistent.mds',
      async() { return () => {}; },
      addDependency() {},
      emitWarning() {},
      getOptions() { return {}; },
    };
    const result = mdsLoader.call(mockContext);
    assert.ok(
      result instanceof Promise,
      'default export should return a Promise when called (async function)',
    );
    // Drain the promise so the test runner does not report an unhandled rejection.
    return result.catch(() => {});
  });

  test('exports _resetForTesting helper', () => {
    assert.equal(typeof _resetForTesting, 'function', '_resetForTesting should be a function');
  });

  test('exports _setTransformerForTesting helper', () => {
    assert.equal(typeof _setTransformerForTesting, 'function', '_setTransformerForTesting should be a function');
  });

});
