/**
 * WASM build parity (#491).
 *
 * 1. Every `wasm-pack build` command that makes a shipped or measured binary passes
 *    `--config profile.release.codegen-units=1`. The ci.yml wasm job measures the
 *    size, the `@mdscript/mds-wasm` build script makes the binary that release.yml
 *    publishes, and the CI js job tests the binary. Without the setting in one of
 *    them, CI measures or tests a binary that the release does not ship.
 * 2. The WASM size limit in release.yml equals the limit in ci.yml.
 *
 * Each check has a positive control (PF-013): a planted bad input must fail.
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(fileURLToPath(import.meta.url), '../../..');
const read = (p) => readFileSync(join(ROOT, p), 'utf8');

const CODEGEN = '--config profile.release.codegen-units=1';

/** Returns each `wasm-pack build` command in `text` that does not carry CODEGEN. */
function buildsWithoutCodegen(text) {
  const commands = text.split(/&&|\n/).filter((c) => c.includes('wasm-pack build'));
  return { count: commands.length, missing: commands.filter((c) => !c.includes(CODEGEN)) };
}

/** Returns every `WASM_SIZE_LIMIT=<digits>` value in `text`. */
function sizeLimits(text) {
  return [...text.matchAll(/^\s*WASM_SIZE_LIMIT=(\d+)\s*$/gm)].map((m) => Number(m[1]));
}

describe('WASM build parity (#491)', () => {
  test('each wasm-pack build command passes one codegen unit', () => {
    // Positive control: a command without the setting is reported.
    const planted = buildsWithoutCodegen('wasm-pack build crates/mds-wasm --target web --out-dir pkg-web');
    assert.equal(planted.missing.length, 1, 'control: a planted command without the setting must be reported');

    const ci = read('.github/workflows/ci.yml');
    const ciWasmJob = ci.slice(ci.indexOf('\n  wasm:\n'), ci.indexOf('\n  js:\n'));
    const ciJsJob = ci.slice(ci.indexOf('\n  js:\n'));
    const script = JSON.parse(read('packages/mds-wasm/package.json')).scripts.build;
    const inputs = [
      ['ci.yml wasm job', ciWasmJob, 2],
      ['ci.yml js job', ciJsJob, 1],
      ['packages/mds-wasm/package.json build script', script, 2],
      ['scripts/build-wasm.sh', read('scripts/build-wasm.sh'), 2],
    ];
    for (const [name, text, expected] of inputs) {
      const { count, missing } = buildsWithoutCodegen(text);
      assert.equal(count, expected, `${name}: expected ${expected} wasm-pack build commands, found ${count}`);
      assert.deepEqual(missing, [], `${name}: wasm-pack build without ${CODEGEN}`);
    }
  });

  test('release.yml and ci.yml enforce the same WASM size limit', () => {
    // Positive controls: two different limits, and no limit, are refused.
    const plantedCi = sizeLimits('          WASM_SIZE_LIMIT=800000\n');
    const plantedRelease = sizeLimits('          WASM_SIZE_LIMIT=850000\n');
    assert.notDeepEqual(plantedCi, plantedRelease, 'control: different limits must compare unequal');
    assert.deepEqual(sizeLimits('raw=1\n'), [], 'control: a step without a limit must yield none');

    const ciLimits = sizeLimits(read('.github/workflows/ci.yml'));
    const releaseLimits = sizeLimits(read('.github/workflows/release.yml'));
    assert.equal(ciLimits.length, 1, `ci.yml: expected one WASM_SIZE_LIMIT, found ${ciLimits.length}`);
    assert.equal(releaseLimits.length, 1, `release.yml: expected one WASM_SIZE_LIMIT, found ${releaseLimits.length}`);
    assert.equal(releaseLimits[0], ciLimits[0], 'release.yml must enforce the ci.yml WASM size limit');
  });
});
