/**
 * PF-007 differential: per-surface goldens each pin their OWN value and cannot
 * catch a cross-backend divergence — only a test that runs the SAME transform
 * under BOTH backends and compares the results can. This spec runs
 * createMdsTransformer against real @mdscript/mds under MDS_BACKEND=native and
 * MDS_BACKEND=wasm (one subprocess per leg — the backend is chosen once at
 * module scope, so the two legs cannot share a process) and deep-equals the
 * emitted metadata and the returned watch dependencies.
 *
 * PF-013: the native leg THROWS with the exact build command when the addon is
 * missing — a skipped leg would read as green while comparing nothing.
 */
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { resolve, dirname, join, isAbsolute } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, '../../..');
const CONSUMER_MDS = join(REPO_ROOT, 'packages/mds/__test__/fixtures/import_consumer.mds');
const ADDON_PATH = join(REPO_ROOT, 'crates/mds-napi/mds-napi.node');
const MDS_URL = pathToFileURL(join(REPO_ROOT, 'packages/mds/dist/node.js')).href;
const BUNDLER_URL = pathToFileURL(join(__dirname, '../dist/index.js')).href;

const NATIVE_BUILD_HINT =
  'build it with: npm run build:native -w @mdscript/mds-napi (requires the Rust toolchain)';

/**
 * Run one transform leg in a subprocess pinned to `backend` via MDS_BACKEND.
 * Returns { backend, metadata, dependencies } as reported by that process.
 */
function runLeg(backend) {
  const script = `
    const mds = await import(${JSON.stringify(MDS_URL)});
    const { createMdsTransformer } = await import(${JSON.stringify(BUNDLER_URL)});
    await mds.init();
    const transformer = createMdsTransformer(mds);
    const result = await transformer.transform(${JSON.stringify(CONSUMER_MDS)});
    const line = result.code.split('\\n').find((l) => l.startsWith('export const metadata = '));
    if (!line) throw new Error('metadata export line missing');
    const metadata = JSON.parse(line.slice('export const metadata = '.length, -1));
    console.log(JSON.stringify({ backend: mds.getBackend(), metadata, dependencies: result.dependencies }));
  `;
  const proc = spawnSync(process.execPath, ['--input-type=module', '-e', script], {
    env: { ...process.env, MDS_BACKEND: backend },
    encoding: 'utf8',
  });
  if (proc.status !== 0) {
    const hint = backend === 'native' ? ` — if the addon is missing, ${NATIVE_BUILD_HINT}` : '';
    throw new Error(`${backend} leg failed (exit ${proc.status})${hint}\n${proc.stderr}`);
  }
  const lastLine = proc.stdout.trim().split('\n').pop();
  return JSON.parse(lastLine);
}

describe('backend parity (PF-007)', () => {
  test('transformer metadata and watch deps are identical under native and wasm backends', () => {
    // PF-013: never skip — a missing addon must fail loudly with the fix.
    if (!existsSync(ADDON_PATH)) {
      throw new Error(`native addon missing at ${ADDON_PATH} — ${NATIVE_BUILD_HINT}`);
    }

    const native = runLeg('native');
    const wasm = runLeg('wasm');

    // PF-013: assert backend identity explicitly — a leg silently falling back
    // to the other backend would make the differential vacuously green.
    assert.equal(native.backend, 'native', 'native leg must actually run the native backend');
    assert.equal(wasm.backend, 'wasm', 'wasm leg must actually run the wasm backend');

    assert.ok(native.metadata.dependencies.length >= 1, 'non-vacuity: fixture must produce dependencies');
    assert.deepEqual(wasm.metadata, native.metadata, 'emitted metadata must be identical across backends');

    for (const [leg, deps] of [['native', native.dependencies], ['wasm', wasm.dependencies]]) {
      for (const dep of deps) {
        assert.ok(isAbsolute(dep), `${leg} watch dependency must be absolute: ${dep}`);
      }
    }
    assert.deepEqual(wasm.dependencies, native.dependencies,
      'watch dependencies must agree across backends after boundary absolutization');
  });
});
