/**
 * WASM backend compileFile/checkFile tests for @mdscript/mds universal package.
 * Tests: U-WCF1 through U-WCF12
 *
 * Uses subprocess isolation with MDS_BACKEND=wasm to force the WASM backend
 * for file operations. Each test spawns a separate subprocess to avoid
 * cross-contamination from the module-level backend singleton.
 */
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import {
  SIMPLE_MDS,
  IMPORT_CONSUMER_MDS,
  ENTRY_MDS,
  __dirname,
  caseInsensitive,
  compileFileOutcomes,
  loadEngines,
  requireEngines,
} from './helpers.mjs';
import path from 'node:path';
import os from 'node:os';
import { mkdir, mkdtemp, writeFile, rm } from 'node:fs/promises';

const exec = promisify(execFile);
const pkgRoot = path.join(__dirname, '..');

/**
 * Spawn a subprocess, execute an inline ESM script that imports from dist/node.js,
 * and return the parsed JSON result written to stdout.
 *
 * Pass `MDS_BACKEND: 'wasm'` in env to force the WASM backend. Omit it (or delete
 * it from process.env) to use the default backend selection.
 *
 * @param {string} script - Inline ESM script. Must write JSON to stdout.
 * @param {Record<string,string>} env - Full environment for the subprocess.
 */
async function runScript(script, env) {
  const { stdout } = await exec(
    process.execPath,
    ['--input-type=module', '-e', script],
    { cwd: pkgRoot, env, timeout: 30000 },
  );
  if (!stdout.trim()) throw new Error('subprocess produced no output');
  return JSON.parse(stdout);
}

function wasmEnv() {
  return { ...process.env, MDS_BACKEND: 'wasm' };
}

function nativeEnv() {
  const env = { ...process.env };
  delete env['MDS_BACKEND'];
  return env;
}

describe('WASM backend — compileFile/checkFile', () => {
  test('U-WCF1: WASM compileFile on simple file returns valid CompileResult shape', async () => {
    const result = await runScript(`
      import { init, compileFile } from './dist/node.js';
      await init();
      const r = await compileFile(${JSON.stringify(SIMPLE_MDS)});
      process.stdout.write(JSON.stringify({ output: r.output, warnings: r.warnings, dependencies: r.dependencies }));
    `, wasmEnv());
    assert.ok(typeof result.output === 'string', 'output must be string');
    assert.ok(result.output.length > 0, 'output must not be empty');
    assert.ok(Array.isArray(result.warnings), 'warnings must be array');
    assert.ok(Array.isArray(result.dependencies), 'dependencies must be array');
  });

  test('U-WCF2: WASM compileFile with imports resolves dependencies', async () => {
    const result = await runScript(`
      import { init, compileFile } from './dist/node.js';
      await init();
      const r = await compileFile(${JSON.stringify(IMPORT_CONSUMER_MDS)});
      process.stdout.write(JSON.stringify({ output: r.output, warnings: r.warnings, dependencies: r.dependencies }));
    `, wasmEnv());
    assert.ok(
      result.output.includes('Hello World!'),
      `expected "Hello World!" in output, got: ${result.output}`,
    );
    assert.ok(
      result.dependencies.length >= 1,
      `expected at least 1 dependency for file with imports, got: ${result.dependencies.length}`,
    );
  });

  test('U-WCF3: WASM compileFile with deep import chain succeeds', async () => {
    const result = await runScript(`
      import { init, compileFile } from './dist/node.js';
      await init();
      const r = await compileFile(${JSON.stringify(ENTRY_MDS)});
      process.stdout.write(JSON.stringify({ output: r.output, warnings: r.warnings, dependencies: r.dependencies }));
    `, wasmEnv());
    assert.ok(typeof result.output === 'string', 'output must be string');
    assert.ok(result.output.length > 0, 'output must not be empty');
  });

  test('U-WCF4: WASM compileFile with runtime vars overrides frontmatter', async () => {
    const result = await runScript(`
      import { init, compileFile } from './dist/node.js';
      await init();
      const r = await compileFile(${JSON.stringify(SIMPLE_MDS)}, { vars: { count: 99 } });
      process.stdout.write(JSON.stringify({ output: r.output, warnings: r.warnings, dependencies: r.dependencies }));
    `, wasmEnv());
    assert.ok(typeof result.output === 'string', 'output must be string');
    assert.ok(
      result.output.includes('You have 99 items'),
      `expected runtime var override (count=99) in output, got: ${result.output}`,
    );
  });

  test('U-WCF5: WASM checkFile returns valid CheckResult shape', async () => {
    const result = await runScript(`
      import { init, checkFile } from './dist/node.js';
      await init();
      const r = await checkFile(${JSON.stringify(SIMPLE_MDS)});
      process.stdout.write(JSON.stringify({ warnings: r.warnings }));
    `, wasmEnv());
    assert.ok(Array.isArray(result.warnings), 'warnings must be array');
  });

  test('U-WCF6: WASM compileFile on nonexistent file rejects with error', async () => {
    const result = await runScript(`
      import { init, compileFile } from './dist/node.js';
      await init();
      try {
        await compileFile('/nonexistent/path/file.mds');
        process.stdout.write(JSON.stringify({ threw: false }));
      } catch (e) {
        process.stdout.write(JSON.stringify({ threw: true, message: e.message }));
      }
    `, wasmEnv());
    assert.ok(result.threw, 'compileFile on nonexistent path must throw');
    assert.ok(result.message, 'error message must not be empty');
  });

  test('U-WCF7: WASM compileFile output matches native compileFile output (parity)', async () => {
    const script = `
      import { init, compileFile } from './dist/node.js';
      await init();
      const r = await compileFile(${JSON.stringify(SIMPLE_MDS)});
      process.stdout.write(JSON.stringify({ output: r.output, warnings: r.warnings, dependencies: r.dependencies }));
    `;
    const [wasmResult, nativeResult] = await Promise.all([
      runScript(script, wasmEnv()),
      runScript(script, nativeEnv()),
    ]);
    assert.equal(
      wasmResult.output,
      nativeResult.output,
      `WASM and native compileFile output must match.\nWASM: ${wasmResult.output}\nNative: ${nativeResult.output}`,
    );
    const toBasenames = (deps) => deps.map((d) => path.basename(d)).sort();
    assert.deepEqual(
      toBasenames(wasmResult.dependencies),
      toBasenames(nativeResult.dependencies),
      'WASM and native compileFile dependencies must match (compared by basename — WASM returns relative, native returns absolute)',
    );
  });

  test('U-WCF8: WASM checkFile output matches native checkFile output (parity)', async () => {
    const script = `
      import { init, checkFile } from './dist/node.js';
      await init();
      const r = await checkFile(${JSON.stringify(SIMPLE_MDS)});
      process.stdout.write(JSON.stringify({ warnings: r.warnings }));
    `;
    const [wasmResult, nativeResult] = await Promise.all([
      runScript(script, wasmEnv()),
      runScript(script, nativeEnv()),
    ]);
    assert.deepEqual(
      wasmResult.warnings,
      nativeResult.warnings,
      `WASM and native checkFile warnings must match`,
    );
  });

  test('U-WCF9: WASM compileFile with imports output matches native (parity)', async () => {
    const script = `
      import { init, compileFile } from './dist/node.js';
      await init();
      const r = await compileFile(${JSON.stringify(IMPORT_CONSUMER_MDS)});
      process.stdout.write(JSON.stringify({ output: r.output, warnings: r.warnings, dependencies: r.dependencies }));
    `;
    const [wasmResult, nativeResult] = await Promise.all([
      runScript(script, wasmEnv()),
      runScript(script, nativeEnv()),
    ]);
    assert.equal(
      wasmResult.output,
      nativeResult.output,
      `WASM and native compileFile output must match for file with imports.\nWASM: ${wasmResult.output}\nNative: ${nativeResult.output}`,
    );
    const toBasenames = (deps) => deps.map((d) => path.basename(d)).sort();
    assert.deepEqual(
      toBasenames(wasmResult.dependencies),
      toBasenames(nativeResult.dependencies),
      'WASM and native compileFile dependencies must match (compared by basename — WASM returns relative, native returns absolute)',
    );
  });

  test('U-WCF10: WASM checkFile with imports matches native (parity)', async () => {
    const script = `
      import { init, checkFile } from './dist/node.js';
      await init();
      const r = await checkFile(${JSON.stringify(IMPORT_CONSUMER_MDS)});
      process.stdout.write(JSON.stringify({ warnings: r.warnings }));
    `;
    const [wasmResult, nativeResult] = await Promise.all([
      runScript(script, wasmEnv()),
      runScript(script, nativeEnv()),
    ]);
    assert.deepEqual(
      wasmResult.warnings,
      nativeResult.warnings,
      `WASM and native checkFile warnings must match for file with imports`,
    );
  });

  test('U-WCF11: WASM checkFile on nonexistent file rejects with error', async () => {
    const result = await runScript(`
      import { init, checkFile } from './dist/node.js';
      await init();
      try {
        await checkFile('/nonexistent/path/file.mds');
        process.stdout.write(JSON.stringify({ threw: false }));
      } catch (e) {
        process.stdout.write(JSON.stringify({ threw: true, message: e.message }));
      }
    `, wasmEnv());
    assert.ok(result.threw, 'checkFile on nonexistent path must throw');
    assert.ok(result.message, 'error message must not be empty');
  });

  test('U-WCF12: case-mismatched entry and import spellings compile identically on both backends, never as a symlink (#408)', async () => {
    // On a case-insensitive volume (the macOS and Windows default) the WASM
    // backend's pre-scanner used to compare realpath with the path as written and
    // report `MAIN.mds` for `main.mds` as a possible symlink. The same file is
    // imported under three spellings, once through a second module.
    const dir = await mkdtemp(path.join(os.tmpdir(), 'mds-wcf-408-'));
    try {
      await writeFile(path.join(dir, '.mdsroot'), '');
      await writeFile(
        path.join(dir, 'main.mds'),
        '@import "./Header.mds" as a\n@import "./header.mds" as b\n@import "./footer.mds" as f\n' +
          '{{a.hi()}}{{b.hi()}}{{f.bye()}}\n',
      );
      await writeFile(path.join(dir, 'header.mds'), '@define hi():\nHi\n@end\n');
      await writeFile(
        path.join(dir, 'footer.mds'),
        '@import "./HEADER.mds" as h\n@define bye():\n{{h.hi()}} bye\n@end\n',
      );
      const insensitive = await caseInsensitive(dir);

      const script = `
        import { init, compileFile, getBackend } from './dist/node.js';
        await init();
        let outcome;
        try {
          outcome = { output: (await compileFile(${JSON.stringify(path.join(dir, 'MAIN.mds'))})).output };
        } catch (e) {
          outcome = { error: { code: e.code, message: e.message, help: e.help ?? null, span: e.span ?? null } };
        }
        process.stdout.write(JSON.stringify({ backend: getBackend(), ...outcome }));
      `;
      const [wasmResult, nativeResult] = await Promise.all([
        runScript(script, wasmEnv()),
        runScript(script, { ...process.env, MDS_BACKEND: 'native' }),
      ]);
      assert.equal(wasmResult.backend, 'wasm');
      assert.equal(nativeResult.backend, 'native');

      if (insensitive) {
        assert.equal(nativeResult.error, undefined, JSON.stringify(nativeResult.error));
        assert.match(nativeResult.output, /Hi.*bye/s);
        assert.equal(wasmResult.output, nativeResult.output, JSON.stringify(wasmResult));
      } else {
        // Case-sensitive volume: the spelling names no file — not found, never a symlink.
        for (const result of [wasmResult, nativeResult]) {
          assert.ok(result.error, `${result.backend}: expected a not-found failure`);
          assert.doesNotMatch(result.error.message, /symlink/, result.backend);
        }
        // The same error on both, help included: the entry is what is missing, and an
        // entry error points into no source (#414).
        assert.equal(nativeResult.error.code, 'mds::file_not_found', JSON.stringify(nativeResult.error));
        assert.equal(typeof nativeResult.error.help, 'string', JSON.stringify(nativeResult.error));
        assert.deepEqual(wasmResult.error, nativeResult.error);
      }
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  });

  test('U-WCF13: a non-MDS entry or import throws the same error on both backends, named as typed (#417)', async (t) => {
    const engines = await loadEngines();
    if (!requireEngines(t, engines, 'U-WCF13')) return;
    const dir = await mkdtemp(path.join(os.tmpdir(), 'mds-wcf13-'));
    try {
      await writeFile(path.join(dir, '.mdsroot'), '');
      await mkdir(path.join(dir, 'sub'));
      await writeFile(path.join(dir, 'doc.txt'), 'Hello!\n');
      // Import-like lines in a file that is not an MDS file are never followed: the
      // resolver refuses the file before it parses it (#417).
      await writeFile(path.join(dir, 'lure.txt'), '@import "./missing.mds" as m\nHello!\n');
      await writeFile(path.join(dir, 'plain.md'), 'Hello!\n');
      await writeFile(path.join(dir, 'page.md'), '---\ntype: mds\n---\nHello!\n');
      await writeFile(path.join(dir, 'imp.mds'), '@import "./sub/../lure.txt" as l\nhi\n');
      // Typed absolute, through `sub/..` (joined by hand: path.join would drop it), and
      // relative to compileFileOutcomes' working directory, the package root.
      const dotted = (name) => [dir, 'sub', '..', name].join(path.sep);
      const relative = (name) => path.relative(pkgRoot, path.join(dir, name));
      const notMds = [
        path.join(dir, 'doc.txt'),
        dotted('doc.txt'),
        relative('doc.txt'),
        path.join(dir, 'lure.txt'),
        path.join(dir, 'plain.md'),
      ];
      const imported = path.join(dir, 'imp.mds');
      const controls = [dotted('page.md'), relative('page.md')];
      const files = [...notMds, imported, ...controls];
      const native = await compileFileOutcomes('native', files);
      const wasm = await compileFileOutcomes('wasm', files);

      const help = "use .mds extension or add 'type: mds' to frontmatter";
      notMds.forEach((entry, i) => {
        // The exact message proves the path is named as typed; no substring check, which
        // a relative form containing the canonical path would fool.
        assert.deepEqual(native[i], { code: 'mds::not_mds', message: `not an MDS file: ${entry}`, help, span: null }, entry);
        assert.deepEqual(wasm[i], native[i], entry);
      });
      const i = notMds.length;
      assert.deepEqual(native[i], { code: 'mds::not_mds', message: 'not an MDS file: ./sub/../lure.txt', help, span: null });
      assert.deepEqual(wasm[i], native[i], 'import');
      // Control: the same forms naming a `type: mds` file compile on both.
      controls.forEach((entry, j) => {
        assert.deepEqual(native[i + 1 + j], { output: 'Hello!\n' }, entry);
        assert.deepEqual(wasm[i + 1 + j], native[i + 1 + j], entry);
      });
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  });
});
