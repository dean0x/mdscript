/**
 * Tests for @mds/vite-plugin.
 */
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import mdsPlugin from '../dist/index.js';

const __dirname = dirname(fileURLToPath(import.meta.url));
const SIMPLE_MDS = resolve(__dirname, '../../mds/__test__/fixtures/simple.mds');

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

function createPluginContext(overrides = {}) {
  const addedWatchFiles = [];
  const warnings = [];
  const errors = [];

  return {
    warn(msg) { warnings.push(msg); },
    addWatchFile(id) { addedWatchFiles.push(id); },
    error(msg) {
      const err = typeof msg === 'string' ? new Error(msg) : new Error(msg.message);
      if (typeof msg === 'object' && msg.id) err.id = msg.id;
      if (typeof msg === 'object' && msg.loc) err.loc = msg.loc;
      errors.push(err);
      throw err;
    },
    get addedWatchFiles() { return addedWatchFiles; },
    get warnings() { return warnings; },
    get errors() { return errors; },
    ...overrides,
  };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
describe('mdsPlugin', () => {
  test('plugin has correct name', () => {
    const plugin = mdsPlugin();
    assert.equal(plugin.name, 'mds');
  });

  test('plugin enforces pre', () => {
    const plugin = mdsPlugin();
    assert.equal(plugin.enforce, 'pre');
  });

  test('has transform and buildStart hooks', () => {
    const plugin = mdsPlugin();
    assert.equal(typeof plugin.buildStart, 'function');
    assert.equal(typeof plugin.transform, 'function');
  });

  test('has handleHotUpdate hook', () => {
    const plugin = mdsPlugin();
    assert.equal(typeof plugin.handleHotUpdate, 'function');
  });

  test('transform returns null for non-mds file (before buildStart)', async () => {
    const plugin = mdsPlugin();
    const ctx = createPluginContext();
    const result = await plugin.transform.call(ctx, '', '/path/to/file.ts');
    assert.equal(result, null);
  });

  test('buildStart initializes transformer', async () => {
    const plugin = mdsPlugin();
    const ctx = createPluginContext();
    // Should not throw
    await plugin.buildStart.call(ctx);
    // After buildStart, transform should work for a real .mds fixture
    const result = await plugin.transform.call(ctx, '', SIMPLE_MDS);
    assert.ok(result !== null, 'should not return null for .mds after init');
    assert.ok(result.code.includes('export default'), 'should have export default');
    assert.equal(result.map, null);
  });

  test('transform returns null for non-mds after buildStart', async () => {
    const plugin = mdsPlugin();
    const ctx = createPluginContext();
    await plugin.buildStart.call(ctx);
    const result = await plugin.transform.call(ctx, '', '/path/to/file.ts');
    assert.equal(result, null);
  });

  test('transform calls addWatchFile for each dependency', async () => {
    const CONSUMER_MDS = resolve(__dirname, '../../mds/__test__/fixtures/import_consumer.mds');
    const plugin = mdsPlugin();
    const ctx = createPluginContext();
    await plugin.buildStart.call(ctx);
    await plugin.transform.call(ctx, '', CONSUMER_MDS);
    assert.ok(ctx.addedWatchFiles.length >= 1, 'expected at least one watch file');
  });

  test('handleHotUpdate sends full-reload for .mds file', () => {
    const plugin = mdsPlugin();
    const sentPayloads = [];
    const ctx = {
      file: '/path/to/file.mds',
      server: {
        ws: {
          send(payload) { sentPayloads.push(payload); },
        },
      },
    };
    const result = plugin.handleHotUpdate(ctx);
    assert.deepEqual(sentPayloads, [{ type: 'full-reload' }]);
    assert.deepEqual(result, []);
  });

  test('handleHotUpdate returns undefined for non-mds file', () => {
    const plugin = mdsPlugin();
    const ctx = {
      file: '/path/to/file.ts',
      server: { ws: { send() {} } },
    };
    const result = plugin.handleHotUpdate(ctx);
    assert.equal(result, undefined);
  });

  test('handleHotUpdate strips query params before checking extension', () => {
    const plugin = mdsPlugin();
    const sentPayloads = [];
    const ctx = {
      file: '/path/to/file.mds?t=123',
      server: { ws: { send(p) { sentPayloads.push(p); } } },
    };
    const result = plugin.handleHotUpdate(ctx);
    assert.deepEqual(sentPayloads, [{ type: 'full-reload' }]);
    assert.deepEqual(result, []);
  });

  test('transform throws error with .id when compile fails', async () => {
    const plugin = mdsPlugin();
    const ctx = createPluginContext();
    await plugin.buildStart.call(ctx);

    // Compiling a nonexistent .mds file should throw with .id attached for the
    // Vite error overlay (equivalent to Rollup's this.error behavior).
    const err = await plugin.transform.call(ctx, '', '/nonexistent/path/file.mds').then(
      () => null,
      (e) => e,
    );
    assert.ok(err instanceof Error, 'should throw an Error');
    assert.equal(err.id, '/nonexistent/path/file.mds', 'error should have .id set to the file path');
  });

  test('options passed to plugin are available', () => {
    const options = { vars: { env: 'test' } };
    const plugin = mdsPlugin(options);
    assert.ok(plugin, 'plugin should be created with options');
  });
});
