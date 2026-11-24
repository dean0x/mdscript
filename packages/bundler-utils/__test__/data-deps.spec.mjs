/**
 * The bundler adapters watch the local data files of a template (#405).
 * Tests: TP-64.
 *
 * Each leg runs the vite and rollup plugins and the webpack and rspack loaders on one
 * template, against real @mdscript/mds under one backend, in its own process: the
 * backend is chosen once per process. The two legs are compared with each other, so a
 * difference between the backends fails the test; a golden of one leg cannot show that.
 * A missing backend fails the leg: a skipped leg would compare nothing.
 */
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, '../../..');
const urlOf = (relative) => pathToFileURL(join(REPO_ROOT, relative)).href;
const URLS = {
  mds: urlOf('packages/mds/dist/node.js'),
  vite: urlOf('packages/vite-plugin/dist/index.js'),
  rollup: urlOf('packages/rollup-plugin/dist/index.js'),
  webpack: urlOf('packages/webpack-loader/dist/index.js'),
  rspack: urlOf('packages/rspack-loader/dist/index.js'),
};

/**
 * Run the four adapters on `entry` with `backend`, in a fresh process. Return the backend
 * that ran, the files each adapter registered for watch, and whether the vite plugin asks
 * for a reload when each file of `changed` changes.
 */
function runLeg(backend, entry, changed) {
  const script = `
    const urls = JSON.parse(process.env.MDS_TEST_URLS);
    const mds = await import(urls.mds);
    const entry = process.env.MDS_TEST_ENTRY;
    const watched = {};
    const reloads = {};
    for (const name of ['vite', 'rollup']) {
      const plugin = (await import(urls[name])).default();
      const files = [];
      const ctx = {
        warn() {},
        addWatchFile(id) { files.push(id); },
        error(message) { throw new Error(message); },
      };
      await plugin.buildStart.call(ctx);
      const result = await plugin.transform.call(ctx, '', entry);
      if (result === null) throw new Error(name + ' did not transform the entry');
      watched[name] = files;
      if (name === 'vite') {
        for (const file of JSON.parse(process.env.MDS_TEST_CHANGED)) {
          const sent = [];
          plugin.handleHotUpdate({ file, modules: [], server: { ws: { send(payload) { sent.push(payload.type); } } } });
          reloads[file] = sent;
        }
      }
    }
    for (const name of ['webpack', 'rspack']) {
      const loader = (await import(urls[name])).default;
      const files = [];
      let outcome = null;
      await loader.call({
        resourcePath: entry,
        getOptions() { return {}; },
        addDependency(dep) { files.push(dep); },
        emitWarning() {},
        async() { return (err, code) => { outcome = { err: err ? err.message : null, code }; }; },
      });
      if (outcome === null || outcome.err !== null) throw new Error(name + ' failed: ' + JSON.stringify(outcome));
      watched[name] = files;
    }
    console.log(JSON.stringify({ backend: mds.getBackend(), watched, reloads }));
  `;
  const proc = spawnSync(process.execPath, ['--input-type=module', '-e', script], {
    cwd: REPO_ROOT,
    env: {
      ...process.env,
      MDS_BACKEND: backend,
      MDS_TEST_URLS: JSON.stringify(URLS),
      MDS_TEST_ENTRY: entry,
      MDS_TEST_CHANGED: JSON.stringify(changed),
    },
    encoding: 'utf8',
    timeout: 60000,
  });
  if (proc.status !== 0) {
    throw new Error(`${backend} leg failed (exit ${proc.status}): ${proc.stderr}`);
  }
  return JSON.parse(proc.stdout.trim().split('\n').pop());
}

describe('bundler adapters watch local data files (#405)', () => {
  test('TP-64: vite, rollup, webpack and rspack watch the data file after the modules, on both backends', () => {
    const dir = realpathSync(mkdtempSync(join(tmpdir(), 'mds-tp64-')));
    try {
      writeFileSync(join(dir, '.mdsroot'), '');
      mkdirSync(join(dir, 'conf'));
      const data = join(dir, 'conf', 'site.json');
      writeFileSync(data, '{"name": "Ada"}\n');
      // A JSON file the template does not name: no adapter watches it.
      const other = join(dir, 'other.json');
      writeFileSync(other, '{}\n');
      const module = join(dir, 'part.mds');
      writeFileSync(module, '@define hi():\nHi\n@end\n');
      const entry = join(dir, 'main.mds');
      writeFileSync(entry, '---\ndata:\n  site: ./conf/site.json\n---\n@import "./part.mds" as p\n{{p.hi()}} {{site.name}}\n');

      const legs = ['native', 'wasm'].map((backend) => runLeg(backend, entry, [data, other]));
      const [native, wasm] = legs;
      // A leg that fell back to the other backend would compare a backend with itself.
      assert.equal(native.backend, 'native', 'the native leg must run the native backend');
      assert.equal(wasm.backend, 'wasm', 'the WASM leg must run the WASM backend');
      // Each adapter watches the imported module, then the data file, as absolute paths.
      const expected = [module, data];
      for (const name of ['vite', 'rollup', 'webpack', 'rspack']) {
        assert.deepEqual(native.watched[name], expected, `native ${name}`);
      }
      assert.deepEqual(wasm.watched, native.watched, 'the WASM backend must watch the same files');
      // The vite plugin reloads when the data file changes. Control: a JSON file the
      // template does not name starts no reload.
      assert.deepEqual(native.reloads, { [data]: ['full-reload'], [other]: [] });
      assert.deepEqual(wasm.reloads, native.reloads);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
