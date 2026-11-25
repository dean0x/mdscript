/**
 * WASM backend compileFile/checkFile tests for @mdscript/mds universal package.
 * Tests: U-WCF1 through U-WCF14
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
  symlinkOrSkip,
} from './helpers.mjs';
import path from 'node:path';
import os from 'node:os';
import { mkdir, mkdtemp, realpath, writeFile, rm } from 'node:fs/promises';

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
      // The import error points at the importer's directive; entry errors stay spanless.
      assert.deepEqual(native[i], {
        code: 'mds::not_mds',
        message: 'not an MDS file: ./sub/../lure.txt',
        help,
        span: { offset: 0, length: 32, line: 1, column: 1 },
      });
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

  test('U-WCF14: case-variant spellings give native sources, dependencies and cycle text on the WASM backend (#414)', async (t) => {
    const engines = await loadEngines();
    if (!requireEngines(t, engines, 'U-WCF14')) return;
    const dir = await mkdtemp(path.join(os.tmpdir(), 'mds-wcf14-'));
    try {
      await writeFile(path.join(dir, '.mdsroot'), '');
      await mkdir(path.join(dir, 'sub'));
      // One file reached under three spellings, one of them through a case-variant
      // directory; every name here differs from the file's on-disk spelling.
      await writeFile(
        path.join(dir, 'main.mds'),
        '@import "./Header.mds" as a\n@import "./SUB/Footer.mds" as f\n{{a.hi()}}{{f.bye()}}\n',
      );
      await writeFile(path.join(dir, 'header.mds'), '@define hi():\nHi\n@end\n');
      await writeFile(path.join(dir, 'sub', 'footer.mds'), '@import "../HEADER.mds" as h\n@define bye():\n{{h.hi()}} bye\n@end\n');
      // A cycle closed through a case variant of the entry.
      await writeFile(path.join(dir, 'cyc.mds'), '@import "./CYC-B.mds" as b\nhi\n');
      await writeFile(path.join(dir, 'cyc-b.mds'), '@import "./Cyc.mds" as a\nB\n');
      const insensitive = await caseInsensitive(dir);
      const root = await realpath(dir);

      const script = `
        import { init, compileFile, getBackend } from './dist/node.js';
        const files = JSON.parse(process.env.MDS_TEST_FILES);
        await init();
        const outcomes = [];
        for (const file of files) {
          try {
            const r = await compileFile(file, { sourceMap: true });
            outcomes.push({ output: r.output, dependencies: r.dependencies, sources: r.sourceMap.sources });
          } catch (e) {
            outcomes.push({ error: { code: e.code, message: e.message, help: e.help ?? null, span: e.span ?? null } });
          }
        }
        process.stdout.write(JSON.stringify({ backend: getBackend(), outcomes }));
      `;
      const entries = [path.join(dir, 'MAIN.mds'), path.join(dir, 'CYC.mds'), path.join(dir, 'main.mds')];
      const run = (backend) =>
        runScript(script, { ...process.env, MDS_BACKEND: backend, MDS_TEST_FILES: JSON.stringify(entries) });
      const [wasmResult, nativeResult] = await Promise.all([run('wasm'), run('native')]);
      assert.equal(wasmResult.backend, 'wasm');
      assert.equal(nativeResult.backend, 'native');
      const [wasm, native] = [wasmResult.outcomes, nativeResult.outcomes];
      // Native names a dependency by its absolute path; the WASM backend by its path
      // below the project root. The spelling is what is compared.
      const belowRoot = (outcome) =>
        outcome.dependencies === undefined
          ? outcome
          : {
            ...outcome,
            dependencies: outcome.dependencies.map((d) =>
              path.isAbsolute(d) ? path.relative(root, d).split(path.sep).join('/') : d,
            ),
          };

      if (insensitive) {
        // Every module is named by its on-disk spelling on both backends.
        assert.deepEqual(belowRoot(native[0]), {
          output: 'HiHi bye\n',
          dependencies: ['header.mds', 'sub/footer.mds'],
          sources: ['main.mds', 'header.mds', 'sub/footer.mds'],
        }, JSON.stringify(native[0]));
        assert.deepEqual(belowRoot(wasm[0]), belowRoot(native[0]));
        assert.equal(native[1].error?.code, 'mds::circular_import', JSON.stringify(native[1]));
        assert.equal(native[1].error.message, 'circular import detected: cyc.mds → cyc-b.mds → cyc.mds');
        assert.deepEqual(wasm[1], native[1]);
      } else {
        // Case-sensitive volume: the spellings name no file — the same error on both.
        assert.equal(native[0].error?.code, 'mds::file_not_found', JSON.stringify(native[0]));
        assert.deepEqual(wasm.slice(0, 2), native.slice(0, 2));
      }
      // Control: the entry spelled as on disk compiles on both. Off a case-insensitive
      // volume its import's spelling names no file, the same on both.
      assert.deepEqual(belowRoot(wasm[2]), belowRoot(native[2]));
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  });

  test('U-WCF15: the WASM backend reads data files as the native backend does (#405)', async (t) => {
    const engines = await loadEngines();
    if (!requireEngines(t, engines, 'U-WCF15')) return;
    const dir = await mkdtemp(path.join(os.tmpdir(), 'mds-wcf15-'));
    try {
      const root = await realpath(dir);
      const write = (name, text) => writeFile(path.join(root, name), text);
      await write('.mdsroot', '');
      await write('cfg.json', '{"name": "Ada"}\n');
      await write('main.mds', '---\ndata:\n  cfg: ./cfg.json\n---\nHi {{cfg.name}}\n');
      await write('missing.mds', '---\ndata:\n  cfg: ./missing.json\n---\nHi {{cfg.name}}\n');
      // A missing data file of an imported module: both backends name the data file and
      // point at the import (#531). Control: a missing module names the import as written.
      await write('m.mds', '---\ndata:\n  d: ./missing.json\n---\nM {{d}}\n');
      await write('imports.mds', '@import "./m.mds" as m\nHi\n');
      await write('gone.mds', '@import "./gone-module.mds" as g\nHi\n');
      // A child entry replaces the base entry, so the base file is never needed.
      await write('base.mds', '---\ndata:\n  d: ./missing.json\n---\nbase {{d.name}}\n@block body:\n@end\n');
      await write('child.mds', '---\ndata:\n  d: ./cfg.json\n---\n@extends "./base.mds"\n@block body:\nchild\n@end\n');
      await mkdir(path.join(root, 'node_modules', 'x'), { recursive: true });
      await write(path.join('node_modules', 'x', 'a.json'), '{"a": 1}\n');
      const linked = await symlinkOrSkip(t, path.join('node_modules', 'x'), path.join(root, 'nmlink'), 'dir');
      if (!linked) return;
      await write('nm.mds', '---\ndata:\n  d: ./nmlink/a.json\n---\n{{d.a}}\n');
      // No child entry replaces the base entry. Native binds the base data in the scope of
      // the child, so its error has no `@extends` context: through one base, a chain of two
      // and an import of the child. `child.mds` above is the control.
      await write('lone-base.mds', '---\ndata:\n  d: ./missing.json\n---\nbase {{d}}\n@block body:\n@end\n');
      await write('lone-child.mds', '@extends "./lone-base.mds"\n@block body:\nchild\n@end\n');
      await write('middle.mds', '@extends "./lone-base.mds"\n@block body:\nmiddle\n@end\n');
      await write('chain.mds', '@extends "./middle.mds"\n@block body:\nchain\n@end\n');
      await write('import-child.mds', '@import "./lone-child.mds" as c\nHi\n');
      // A base path whose key a base entry before it read: the scan refuses it at once.
      await write(
        'known-base.mds',
        '---\ndata:\n  a: ./cfg.json\n  b: ./gone/../cfg.json\n---\nbase {{a.name}} {{b.name}}\n@block body:\n@end\n',
      );
      await write('known-child.mds', '@extends "./known-base.mds"\n@block body:\nchild\n@end\n');
      const names = [
        'main.mds', 'missing.mds', 'imports.mds', 'child.mds', 'nm.mds',
        'lone-child.mds', 'chain.mds', 'import-child.mds', 'known-child.mds', 'gone.mds',
      ];
      const files = names.map((name) => path.join(root, name));
      // Native names a dependency by its absolute path, the WASM backend by its key.
      const belowRoot = (outcome) =>
        outcome.dependencies === undefined
          ? outcome
          : {
            ...outcome,
            dependencies: outcome.dependencies.map((d) =>
              path.isAbsolute(d) ? path.relative(root, d).split(path.sep).join('/') : d,
            ),
          };
      const native = (await compileFileOutcomes('native', files, { sourceMap: true })).map(belowRoot);
      // Control: the native outcomes are the ones each fixture is for, so the equality
      // below compares reads and refusals, not two identical failures.
      assert.deepEqual(native[0], { output: 'Hi Ada\n', dependencies: ['cfg.json'], sources: ['main.mds'] });
      assert.deepEqual(native[1], {
        code: 'mds::file_not_found',
        message: 'file not found: ./missing.json',
        help: 'check the file path and ensure the file exists',
        span: null,
      });
      assert.equal(native[2].code, 'mds::file_not_found', JSON.stringify(native[2]));
      assert.equal(native[2].message, 'file not found: ./missing.json');
      assert.equal(native[2].span?.offset, 0, JSON.stringify(native[2]));
      assert.equal(native[3].output, 'base Ada\nchild\n', JSON.stringify(native[3]));
      assert.equal(native[4].code, 'mds::import', JSON.stringify(native[4]));
      assert.equal(native[4].message, 'import error: data path "./nmlink/a.json": node_modules segments are not allowed');
      assert.deepEqual(native.slice(5, 7), [native[1], native[1]]);
      assert.equal(native[7].code, 'mds::file_not_found', JSON.stringify(native[7]));
      assert.equal(native[7].message, 'file not found: ./missing.json');
      assert.equal(native[7].span?.offset, 0, JSON.stringify(native[7]));
      if (process.platform === 'win32') {
        // Windows applies `..` first, so `./gone/../cfg.json` names cfg.json.
        assert.equal(native[8].output, 'base Ada Ada\nchild\n', JSON.stringify(native[8]));
      } else {
        assert.deepEqual(native[8], { ...native[1], message: 'file not found: ./gone/../cfg.json' });
      }
      assert.equal(native[9].code, 'mds::file_not_found', JSON.stringify(native[9]));
      assert.equal(native[9].message, 'file not found: ./gone-module.mds');
      assert.equal(native[9].span?.offset, 0, JSON.stringify(native[9]));
      assert.deepEqual((await compileFileOutcomes('wasm', files, { sourceMap: true })).map(belowRoot), native);
      // A check reads no data file, so both backends pass the same entry.
      const checks = await compileFileOutcomes('native', files.slice(0, 1), { method: 'checkFile' });
      assert.deepEqual(await compileFileOutcomes('wasm', files.slice(0, 1), { method: 'checkFile' }), checks);
      assert.ok(!('code' in checks[0]), JSON.stringify(checks));
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  });
});
