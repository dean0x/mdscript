/**
 * CJS compatibility tests for @mds/bundler-utils.
 *
 * Verifies that the CJS build (dist-cjs/) can be loaded via require() and
 * exports all expected symbols. This ensures the package is usable from
 * CommonJS consumers such as Webpack loaders and older toolchains.
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

describe('bundler-utils CJS build', { skip: !hasCjsBuild && 'dist-cjs/ not built — run build first' }, () => {
  const cjsBuild = require(cjsPath);
  const { createMdsTransformer, formatMdsError, shouldTransform, LazyInit } = cjsBuild;

  test('loads without error via require()', () => {
    assert.ok(cjsBuild, 'CJS build should load successfully');
  });

  test('exports createMdsTransformer', () => {
    assert.equal(typeof createMdsTransformer, 'function', 'createMdsTransformer should be a function');
  });

  test('exports formatMdsError', () => {
    assert.equal(typeof formatMdsError, 'function', 'formatMdsError should be a function');
  });

  test('exports shouldTransform', () => {
    assert.equal(typeof shouldTransform, 'function', 'shouldTransform should be a function');
  });

  test('exports LazyInit', () => {
    assert.equal(typeof LazyInit, 'function', 'LazyInit should be a constructor function');
  });

  test('LazyInit works correctly when loaded via require()', async () => {
    let callCount = 0;
    const lazy = new LazyInit(async () => {
      callCount++;
      return 'cjs-value';
    });

    const v1 = await lazy.get();
    const v2 = await lazy.get();

    assert.equal(v1, 'cjs-value');
    assert.equal(v2, 'cjs-value');
    assert.equal(callCount, 1, 'factory should only be called once');
  });

  test('shouldTransform returns true for .mds files', () => {
    assert.equal(shouldTransform('/path/to/file.mds'), true);
    assert.equal(shouldTransform('/path/to/file.ts'), false);
  });

  test('formatMdsError handles plain Error objects', () => {
    const err = new Error('Something went wrong');
    const result = formatMdsError(err, '/file.mds');
    assert.equal(typeof result.message, 'string');
    assert.ok(result.message.length > 0);
  });
});
