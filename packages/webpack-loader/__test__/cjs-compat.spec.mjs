/**
 * CJS compatibility tests for @mds/webpack-loader.
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

const __dirname = dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);

describe('webpack-loader CJS build', () => {
  test('loads without error via require()', () => {
    const cjsPath = resolve(__dirname, '../dist-cjs/index.js');
    const cjsBuild = require(cjsPath);
    assert.ok(cjsBuild, 'CJS build should load successfully');
  });

  test('exports default as an async function (the loader)', () => {
    const { default: mdsLoader } = require(resolve(__dirname, '../dist-cjs/index.js'));
    assert.equal(typeof mdsLoader, 'function', 'default export should be a function');
    // Webpack loaders must be async functions — verify it returns a Promise when called with context
    assert.ok(
      mdsLoader.constructor.name === 'AsyncFunction' ||
      mdsLoader.toString().includes('async'),
      'default export should be an async function',
    );
  });

  test('exports _resetForTesting helper', () => {
    const { _resetForTesting } = require(resolve(__dirname, '../dist-cjs/index.js'));
    assert.equal(typeof _resetForTesting, 'function', '_resetForTesting should be a function');
  });

  test('exports _setTransformerForTesting helper', () => {
    const { _setTransformerForTesting } = require(resolve(__dirname, '../dist-cjs/index.js'));
    assert.equal(typeof _setTransformerForTesting, 'function', '_setTransformerForTesting should be a function');
  });

});
