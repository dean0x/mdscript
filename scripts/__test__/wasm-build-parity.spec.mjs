/**
 * WASM build parity (#491).
 *
 * 1. Every `wasm-pack build` command in a workflow, a repo script or a package
 *    build script passes `--config profile.release.codegen-units=1`. The ci.yml
 *    wasm job measures the size, the `@mdscript/mds-wasm` build script makes the
 *    binary that release.yml publishes, and the CI js job tests the binary.
 *    Without the setting in one of them, CI measures or tests a binary that the
 *    release does not build. `wasm-pack test` builds a test binary that is not
 *    shipped, so it is out of scope.
 * 2. release.yml and ci.yml set the same WASM_SIZE_LIMIT, and each compares the
 *    size against that variable, not against a literal number.
 *
 * Each check has a positive control (PF-013): a planted bad input must fail.
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(fileURLToPath(import.meta.url), '../../..');
const read = (p) => readFileSync(join(ROOT, p), 'utf8');
const list = (dir, ext) =>
  readdirSync(join(ROOT, dir)).filter((f) => f.endsWith(ext)).map((f) => `${dir}/${f}`);

const CODEGEN = '--config profile.release.codegen-units=1';

/** Returns the `wasm-pack build` commands in `text` (comment lines skipped). */
function wasmBuilds(text) {
  return text
    .split('\n')
    .filter((line) => !line.trim().startsWith('#'))
    .flatMap((line) => line.split('&&'))
    .filter((cmd) => cmd.includes('wasm-pack build'));
}

/** Returns a list of `<source>: <command>` for each build without CODEGEN. */
function buildsWithoutCodegen(sources) {
  return sources.flatMap(([name, text]) =>
    wasmBuilds(text).filter((c) => !c.includes(CODEGEN)).map((c) => `${name}: ${c.trim()}`),
  );
}

/** Returns the problems of the size-limit steps in a ci.yml and a release.yml text. */
function limitProblems(ciText, releaseText) {
  const problems = [];
  const values = {};
  for (const [name, text] of [['ci.yml', ciText], ['release.yml', releaseText]]) {
    const found = [...text.matchAll(/^\s*WASM_SIZE_LIMIT=(\d+)\s*$/gm)].map((m) => Number(m[1]));
    if (found.length !== 1) problems.push(`${name}: expected one WASM_SIZE_LIMIT, found ${found.length}`);
    values[name] = found[0];
    if (!text.includes('-gt "$WASM_SIZE_LIMIT"')) problems.push(`${name}: no -gt "$WASM_SIZE_LIMIT" comparison`);
    if (/-gt\s+"?\d{5,}/.test(text)) problems.push(`${name}: compares a size against a literal number`);
  }
  if (problems.length === 0 && values['ci.yml'] !== values['release.yml']) {
    problems.push(`release.yml limit ${values['release.yml']} differs from ci.yml limit ${values['ci.yml']}`);
  }
  return problems;
}

describe('WASM build parity (#491)', () => {
  test('each wasm-pack build command passes one codegen unit', () => {
    // Positive controls: a command without the setting is reported; a comment is not a command.
    assert.equal(
      buildsWithoutCodegen([['planted', 'run: wasm-pack build crates/mds-wasm --target web']]).length,
      1,
      'control: a planted command without the setting must be reported',
    );
    assert.equal(wasmBuilds('# wasm-pack build in a comment').length, 0, 'control: comments are skipped');

    const packageScripts = readdirSync(join(ROOT, 'packages')).flatMap((dir) => {
      try {
        const scripts = JSON.parse(read(`packages/${dir}/package.json`)).scripts ?? {};
        return Object.entries(scripts).map(([k, v]) => [`packages/${dir} script ${k}`, v]);
      } catch {
        return []; // a folder without a package.json has no build script to check
      }
    });
    const files = [
      ...list('.github/workflows', '.yml'),
      ...list('scripts', '.sh'),
      ...list('scripts', '.mjs'),
    ].map((p) => [p, read(p)]);
    const sources = [...files, ...packageScripts];

    // Non-vacuity: the known sites must be found.
    const count = (name) => wasmBuilds(sources.find(([n]) => n === name)?.[1] ?? '').length;
    assert.equal(count('.github/workflows/ci.yml'), 3, 'ci.yml: expected 3 wasm-pack build commands (wasm job 2, js job 1)');
    assert.equal(count('packages/mds-wasm script build'), 2, 'packages/mds-wasm build script: expected 2 commands');
    assert.equal(count('scripts/build-wasm.sh'), 2, 'scripts/build-wasm.sh: expected 2 commands');

    assert.deepEqual(buildsWithoutCodegen(sources), [], `wasm-pack build without ${CODEGEN}`);
  });

  test('release.yml and ci.yml enforce the same WASM size limit', () => {
    const step = (n) => `          WASM_SIZE_LIMIT=${n}\n            if [ "$raw" -gt "$WASM_SIZE_LIMIT" ]; then\n`;
    // Positive controls (PF-013): each planted defect is reported.
    assert.deepEqual(limitProblems(step(800000), step(800000)), [], 'control: equal limits pass');
    assert.equal(limitProblems(step(800000), step(850000)).length, 1, 'control: different limits fail');
    assert.ok(limitProblems(step(800000), 'raw=1\n').length > 0, 'control: a missing limit fails');
    assert.ok(
      limitProblems(step(800000), `${step(800000)}            if [ "$raw" -gt 850000 ]; then\n`).length > 0,
      'control: a literal comparison fails',
    );
    assert.ok(
      limitProblems(step(800000), '          WASM_SIZE_LIMIT=800000\n').length > 0,
      'control: a limit that no comparison uses fails',
    );

    assert.deepEqual(limitProblems(read('.github/workflows/ci.yml'), read('.github/workflows/release.yml')), []);
  });
});
