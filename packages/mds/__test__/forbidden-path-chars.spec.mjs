/**
 * #265 forbidden path characters — Rust↔JS differential (AC-265-5).
 * Tests: U-FP1 through U-FP5
 *
 * The JS pre-scanner (src/util/path-chars.ts + src/util/module-scanner.ts)
 * re-implements `mds::is_forbidden_path_char` and the refusal messages in another
 * language, so it is a parity surface of its own (PF-007): a golden on either side
 * would only pin that side's value. These tests compare the JS verdicts and
 * messages against the REAL Rust engine, reached through both backends (the native
 * addon and the WASM module), never against a hand-written expectation alone.
 *
 * In CI both backends must be present; locally a missing backend skips the test
 * visibly (build with `npm run build:native -w @mdscript/mds-napi` and
 * `wasm-pack build crates/mds-wasm --target nodejs --out-dir pkg`).
 */
import { test, describe, before } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdtemp, mkdir, symlink, writeFile, rm } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {
  __dirname,
  FORBIDDEN_PATH_CODEPOINTS,
  assertNoForbiddenChars,
  errorShape,
  escapeText,
  uPlus,
} from './helpers.mjs';

const { isForbiddenPathChar } = await import('../dist/util/path-chars.js');
const { normalizeVirtualKey } = await import('../dist/util/module-scanner.js');

const exec = promisify(execFile);
const pkgRoot = path.join(__dirname, '..');

function codepointRange(first, last) {
  return Array.from({ length: last - first + 1 }, (_, i) => first + i);
}

const LF = 0x0a;
const QUOTE = 0x22;

/**
 * The codepoints the backends are asked about: every member of the class plus a
 * control set of non-members chosen to sit next to one — all of Latin-1 (C0,
 * ASCII, DEL, C1, NBSP, soft hyphen), the Arabic block around U+061C, the General
 * Punctuation block (zero-width characters, bidi marks, embeddings, overrides and
 * isolates, the line/paragraph separators, invisible operators), the BOM and its
 * neighbours, the interlinear-annotation and replacement characters, and astral
 * format/tag/emoji codepoints up to the last scalar value.
 */
const CODEPOINTS = [
  ...codepointRange(0x00, 0xff),
  0x034f,
  ...codepointRange(0x0600, 0x061f),
  0x180e,
  ...codepointRange(0x2000, 0x206f),
  0xfefe,
  0xfeff,
  0xff00,
  ...codepointRange(0xfff9, 0xfffd),
  0x1d173,
  0x1f600,
  0xe0001,
  0xe007f,
  0x10ffff,
];

/**
 * Load the raw native addon and WASM module — the same Rust engine the two
 * `@mdscript/mds` backends call. Either is null when it has not been built.
 */
async function loadEngines() {
  let native = null;
  let wasm = null;
  try {
    const require = createRequire(import.meta.url);
    native = require(path.join(__dirname, '../../../crates/mds-napi/index.js'));
  } catch {
    native = null;
  }
  try {
    const { initWasmNode } = await import('../dist/backend/wasm.js');
    wasm = await initWasmNode();
  } catch {
    wasm = null;
  }
  return { native, wasm };
}

/** Skip visibly when an engine is missing — except in CI, where that is a failure. */
function requireEngines(t, engines, label) {
  const missing = Object.entries(engines)
    .filter(([, engine]) => engine === null)
    .map(([name]) => name);
  if (missing.length === 0) return true;
  if (process.env.CI) {
    throw new Error(`${label}: the ${missing.join(' and ')} backend is required in CI`);
  }
  t.skip(`${missing.join(' and ')} backend not built`);
  return false;
}

/** The Rust verdict on `modules`/`entry`: the error shape it throws, or 'accepted'. */
function rustOutcome(engine, modules, entry) {
  try {
    engine.lintVirtual(modules, entry);
    return 'accepted';
  } catch (err) {
    return errorShape(err);
  }
}

/** The JS verdict on the same input, through the pre-scanner's own function. */
function jsOutcome(fn) {
  try {
    fn();
    return 'accepted';
  } catch (err) {
    return errorShape(err);
  }
}

/**
 * `compileFile` each of `files` through the public `@mdscript/mds` API with the
 * backend forced by MDS_BACKEND, in a fresh process (the backend is a module-level
 * singleton). Returns the backend that actually ran and, per file, the error shape
 * it threw or `{ output }`.
 */
async function compileFileOutcomes(backend, files) {
  const script = `
    import { init, compileFile, getBackend } from './dist/node.js';
    const files = JSON.parse(process.env.MDS_TEST_FILES);
    await init();
    const outcomes = [];
    for (const file of files) {
      try {
        const r = await compileFile(file);
        outcomes.push({ output: r.output });
      } catch (err) {
        outcomes.push({ code: err.code, message: err.message, help: err.help ?? null, span: err.span ?? null });
      }
    }
    process.stdout.write(JSON.stringify({ backend: getBackend(), outcomes }));
  `;
  const { stdout } = await exec(process.execPath, ['--input-type=module', '-e', script], {
    cwd: pkgRoot,
    env: { ...process.env, MDS_BACKEND: backend, MDS_TEST_FILES: JSON.stringify(files) },
    timeout: 60000,
    maxBuffer: 16 * 1024 * 1024,
  });
  const result = JSON.parse(stdout);
  assert.equal(result.backend, backend, 'the forced backend must be the one that ran');
  return result.outcomes;
}

describe('forbidden path characters — Rust↔JS differential (#265)', () => {
  let engines;
  before(async () => {
    engines = await loadEngines();
  });

  test('U-FP1: isForbiddenPathChar holds for exactly the 80 codepoints of the class over every scalar value', () => {
    const found = [];
    for (let cp = 0; cp <= 0x10ffff; cp++) {
      if (cp >= 0xd800 && cp <= 0xdfff) continue; // surrogates are not scalar values
      if (isForbiddenPathChar(cp)) found.push(cp);
    }
    assert.deepEqual(found, [...FORBIDDEN_PATH_CODEPOINTS]);
    // Coverage of the backend differentials below: every member is asked about.
    const asked = new Set(CODEPOINTS);
    assert.deepEqual(FORBIDDEN_PATH_CODEPOINTS.filter((cp) => !asked.has(cp)), []);
  });

  test('U-FP2: entry keys — native, WASM and the JS scanner agree on every codepoint, verdict and message', (t) => {
    if (!requireEngines(t, engines, 'U-FP2')) return;
    const mismatches = [];
    let refused = 0;
    for (const cp of CODEPOINTS) {
      const key = `a${String.fromCodePoint(cp)}b.mds`;
      const modules = { [key]: 'hi\n' };
      const native = rustOutcome(engines.native, modules, key);
      const wasm = rustOutcome(engines.wasm, modules, key);
      const js = jsOutcome(() => normalizeVirtualKey('', key));
      const expectRefused = isForbiddenPathChar(cp);
      const nativeRefused = native !== 'accepted';
      if (nativeRefused) refused++;
      if (
        nativeRefused !== expectRefused ||
        JSON.stringify(wasm) !== JSON.stringify(native) ||
        JSON.stringify(js) !== JSON.stringify(native)
      ) {
        mismatches.push({ cp: uPlus(cp), expectRefused, native, wasm, js });
      }
    }
    assert.deepEqual(mismatches, []);
    // Non-vacuity (PF-013): the engine really refused the whole class, with the
    // entry-key error rather than some other failure.
    assert.equal(refused, 80);
    const escKey = `a${String.fromCodePoint(0x1b)}b.mds`;
    const esc = rustOutcome(engines.native, { [escKey]: 'hi\n' }, escKey);
    assert.deepEqual(esc, {
      code: 'mds::io',
      message: `entry path contains forbidden character U+001B: "a${escapeText(0x1b)}b.mds"`,
      help: null,
      span: null,
    });
  });

  test('U-FP3: import strings — native, WASM and the JS scanner agree on every codepoint, verdict and message', (t) => {
    if (!requireEngines(t, engines, 'U-FP3')) return;
    const mismatches = [];
    let refused = 0;
    // A quoted `@import` cannot carry LF (the lexer ends the line) or `"` (it ends
    // the string), so neither reaches a path check this way. LF stays covered by the
    // entry-key route above and the frontmatter route in U-FP4.
    for (const cp of CODEPOINTS.filter((c) => c !== LF && c !== QUOTE)) {
      const c = String.fromCodePoint(cp);
      const importPath = `./a${c}b.mds`;
      const modules = { 'main.mds': `@import "${importPath}"\nhi\n`, [`a${c}b.mds`]: 'hi\n' };
      const native = rustOutcome(engines.native, modules, 'main.mds');
      const wasm = rustOutcome(engines.wasm, modules, 'main.mds');
      const js = jsOutcome(() => normalizeVirtualKey('main.mds', importPath));
      const expectRefused = isForbiddenPathChar(cp);
      const nativeRefused = native !== 'accepted';
      if (nativeRefused) refused++;
      if (
        nativeRefused !== expectRefused ||
        JSON.stringify(wasm) !== JSON.stringify(native) ||
        JSON.stringify(js) !== JSON.stringify(native)
      ) {
        mismatches.push({ cp: uPlus(cp), expectRefused, native, wasm, js });
      }
    }
    assert.deepEqual(mismatches, []);
    assert.equal(refused, 79, 'every member but LF is refused on this route');
  });

  test('U-FP4: compileFile — the WASM backend (JS pre-scanner) and the native backend throw identical errors', async (t) => {
    if (!requireEngines(t, engines, 'U-FP4')) return;
    const dir = await mkdtemp(path.join(os.tmpdir(), 'mds-265-diff-'));
    try {
      await writeFile(path.join(dir, '.mdsroot'), '');
      // Control: a clean import whose name has a space and non-ASCII letters.
      await writeFile(path.join(dir, 'clean.mds'), '@import "./a b-ünï.mds"\nhi\n');
      await writeFile(path.join(dir, 'a b-ünï.mds'), 'there\n');
      for (const cp of FORBIDDEN_PATH_CODEPOINTS) {
        const c = String.fromCodePoint(cp);
        // Body import (the scanner's route), frontmatter import (YAML escape, so LF
        // is reachable too), and the entry path itself (never created).
        await writeFile(path.join(dir, `imp-${cp}.mds`), `@import "./a${c}b.mds"\nhi\n`);
        await writeFile(
          path.join(dir, `fm-${cp}.mds`),
          `---\nimports:\n  - path: "./a${escapeText(cp)}b.mds"\n---\nhi\n`,
        );
      }

      const cases = { clean: path.join(dir, 'clean.mds') };
      for (const cp of FORBIDDEN_PATH_CODEPOINTS) {
        cases[`imp-${cp}`] = path.join(dir, `imp-${cp}.mds`);
        cases[`fm-${cp}`] = path.join(dir, `fm-${cp}.mds`);
        cases[`entry-${cp}`] = path.join(dir, `a${String.fromCodePoint(cp)}b.mds`);
      }
      const names = Object.keys(cases);
      const byName = (outcomes) => Object.fromEntries(names.map((name, i) => [name, outcomes[i]]));
      const native = byName(await compileFileOutcomes('native', Object.values(cases)));
      const wasm = byName(await compileFileOutcomes('wasm', Object.values(cases)));

      // Non-vacuity (PF-013): the native engine refused every route for the reason
      // under test, and the clean control compiled.
      assert.equal(typeof native.clean.output, 'string', JSON.stringify(native.clean));
      for (const cp of FORBIDDEN_PATH_CODEPOINTS) {
        const label = uPlus(cp);
        const fmReason = cp === 0 ? 'contains null byte' : `contains forbidden character ${label}`;
        assert.equal(native[`fm-${cp}`].code, 'mds::import', label);
        assert.ok(
          native[`fm-${cp}`].message.includes(`invalid path "./a${escapeText(cp)}b.mds": ${fmReason}`),
          `${label}: ${native[`fm-${cp}`].message}`,
        );
        const shownEntry = path.join(dir, `a${escapeText(cp)}b.mds`);
        const entryMessage = cp === 0
          ? `entry path contains null byte: "${shownEntry}"`
          : `entry path contains forbidden character ${label}: "${shownEntry}"`;
        assert.deepEqual(native[`entry-${cp}`], { code: 'mds::io', message: entryMessage, help: null, span: null });
        if (cp !== LF) {
          const importMessage = cp === 0
            ? 'import error: import path contains null byte'
            : `import error: import path contains forbidden character ${label}: "./a${escapeText(cp)}b.mds"`;
          assert.deepEqual(native[`imp-${cp}`], { code: 'mds::import', message: importMessage, help: null, span: null });
        }
        for (const route of ['imp', 'fm', 'entry']) {
          const shape = native[`${route}-${cp}`];
          if (shape.message !== undefined) assertNoForbiddenChars(shape.message, `${route} ${label}`);
        }
      }

      assert.deepEqual(wasm, native);
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  });

  // Windows file names cannot carry C0 controls, so the hostile directory this
  // needs cannot be created there.
  test(
    'U-FP5: compileFile through a symlink into a hostile-named directory — both backends throw the same mds::io error',
    { skip: process.platform === 'win32' && 'C0 controls are not valid in Windows file names' },
    async (t) => {
      if (!requireEngines(t, engines, 'U-FP5')) return;
      const dir = await mkdtemp(path.join(os.tmpdir(), 'mds-265-diff-'));
      try {
        await writeFile(path.join(dir, '.mdsroot'), '');
        const hostileCps = [0x09, 0x0a, 0x1b, 0x7f, 0x85, 0x202e, 0xfeff];
        const files = [];
        for (const cp of hostileCps) {
          const hostile = path.join(dir, `ho${String.fromCodePoint(cp)}stile-${cp}`);
          await mkdir(hostile);
          await writeFile(path.join(hostile, 'main.mds'), 'hi\n');
          await symlink(hostile, path.join(dir, `alias-${cp}`), 'dir');
          // The typed path is clean; only its canonical form carries the codepoint.
          files.push(path.join(dir, `alias-${cp}`, 'main.mds'));
        }
        // Control: the same layout with a clean target directory compiles.
        await mkdir(path.join(dir, 'clean'));
        await writeFile(path.join(dir, 'clean', 'main.mds'), 'hi\n');
        await symlink(path.join(dir, 'clean'), path.join(dir, 'alias-clean'), 'dir');
        files.push(path.join(dir, 'alias-clean', 'main.mds'));

        const native = await compileFileOutcomes('native', files);
        const wasm = await compileFileOutcomes('wasm', files);

        // Non-vacuity (PF-013): native refused each hostile path for this reason,
        // naming the path as typed, and compiled the control.
        hostileCps.forEach((cp, i) => {
          assert.deepEqual(native[i], {
            code: 'mds::io',
            message: `resolved path contains forbidden character ${uPlus(cp)}: "${files[i]}"`,
            help: null,
            span: null,
          });
        });
        assert.deepEqual(native[hostileCps.length], { output: 'hi\n' });

        assert.deepEqual(wasm, native);
      } finally {
        await rm(dir, { recursive: true, force: true });
      }
    },
  );
});
