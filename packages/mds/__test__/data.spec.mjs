/**
 * Local data files through the public JS API of @mdscript/mds, on both backends (#405).
 * Tests: TP-15, TP-19, TP-52 (the JS arms).
 *
 * Each test runs the same calls on the native and the WASM backend, each in its own
 * process, and compares the two: a golden of one backend cannot show that they agree.
 */
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdir, mkdtemp, realpath, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { compileFileOutcomes, loadEngines, pkgRoot, requireEngines } from './helpers.mjs';

const { buildModulesMap } = await import('../dist/util/module-scanner.js');
const exec = promisify(execFile);
const engines = await loadEngines();

/** Run `fn` with a new temporary project root, given by its canonical path, then remove it. */
async function withProject(prefix, fn) {
  const dir = await realpath(await mkdtemp(path.join(os.tmpdir(), prefix)));
  try {
    await writeFile(path.join(dir, '.mdsroot'), '');
    return await fn(dir);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
}

/** Write `text` to `name` below `dir`; return the file's path. */
async function put(dir, name, text) {
  const file = path.join(dir, name);
  await mkdir(path.dirname(file), { recursive: true });
  await writeFile(file, text);
  return file;
}

/** Native names a dependency by its absolute path, the WASM backend by its key below the root. */
function belowRoot(root) {
  return (outcome) =>
    outcome.dependencies === undefined
      ? outcome
      : {
        ...outcome,
        dependencies: outcome.dependencies.map((dep) =>
          path.isAbsolute(dep) ? path.relative(root, dep).split(path.sep).join('/') : dep,
        ),
      };
}

/** The public calls that `backendCalls` may make. */
const METHODS = new Set(['compile', 'check', 'lint', 'compileFile', 'checkFile', 'lintFile']);

/**
 * Make each call `[method, ...args]` of `calls` through the public API with `backend`, in
 * a fresh process. Return `{ result }` or `{ error }` (its binding-visible fields) per call.
 */
async function backendCalls(backend, calls) {
  for (const [method] of calls) assert.ok(METHODS.has(method), `unknown method ${method}`);
  const script = `
    import * as mds from './dist/node.js';
    const calls = JSON.parse(process.env.MDS_TEST_CALLS);
    await mds.init();
    const outcomes = [];
    for (const [method, ...args] of calls) {
      try {
        outcomes.push({ result: await mds[method](...args) });
      } catch (e) {
        outcomes.push({ error: { code: e.code, message: e.message, help: e.help ?? null, span: e.span ?? null } });
      }
    }
    process.stdout.write(JSON.stringify({ backend: mds.getBackend(), outcomes }));
  `;
  const { stdout } = await exec(process.execPath, ['--input-type=module', '-e', script], {
    cwd: pkgRoot,
    env: { ...process.env, MDS_BACKEND: backend, MDS_TEST_CALLS: JSON.stringify(calls) },
    timeout: 60000,
  });
  const result = JSON.parse(stdout);
  assert.equal(result.backend, backend, 'the forced backend must be the one that ran');
  return result.outcomes;
}

/** A source whose `data:` entry `cfg` names `./bad.json`, a file that is not valid JSON. */
const BAD_ENTRY = '---\ndata:\n  cfg: ./bad.json\n---\nHi {{cfg.name}}\n';
const BAD_JSON = '{"name": ';

describe('local data files through the public JS API (#405)', () => {
  test('TP-19: compileFile binds a JSON entry and a YAML entry to the same output on both backends', async (t) => {
    if (!requireEngines(t, engines, 'TP-19')) return;
    await withProject('mds-tp19-', async (root) => {
      await put(root, 'cfg.json', '{"name": "Ada", "n": 2}\n');
      await put(root, 'list.yaml', 'title: Grace\ncount: 3\n');
      const files = [
        await put(root, 'json.mds', '---\ndata:\n  cfg: ./cfg.json\n---\nHi {{cfg.name}}, {{cfg.n}}\n'),
        await put(root, 'yaml.mds', '---\ndata:\n  list: ./list.yaml\n---\n{{list.title}}: {{list.count}}\n'),
      ];
      const options = { sourceMap: true };
      const native = (await compileFileOutcomes('native', files, options)).map(belowRoot(root));
      assert.deepEqual(native, [
        { output: 'Hi Ada, 2\n', dependencies: ['cfg.json'], sources: ['json.mds'] },
        { output: 'Grace: 3\n', dependencies: ['list.yaml'], sources: ['yaml.mds'] },
      ]);
      assert.deepEqual((await compileFileOutcomes('wasm', files, options)).map(belowRoot(root)), native);
    });
  });

  test('TP-15: a runtime variable named data holds an entry map, defines no entry and reads no file', async (t) => {
    if (!requireEngines(t, engines, 'TP-15')) return;
    await withProject('mds-tp15-', async (root) => {
      await put(root, 'bad.json', BAD_JSON);
      const plain = await put(root, 'plain.mds', 'Path {{data.cfg}}\n');
      const entry = await put(root, 'entry.mds', BAD_ENTRY);
      const vars = { data: { cfg: './bad.json' } };
      const outcomes = {};
      for (const backend of ['native', 'wasm']) {
        outcomes[backend] = [
          ...(await compileFileOutcomes(backend, [plain, entry], { vars })),
          // A runtime variable with the entry's name replaces the entry: native reads no file.
          ...(await compileFileOutcomes(backend, [entry], { vars: { cfg: { name: 'Bob' } } })),
        ];
      }
      assert.deepEqual(outcomes.native.map((o) => o.output ?? o.code), ['Path ./bad.json\n', 'mds::yaml', 'Hi Bob\n']);
      // Control: the same entry map in the frontmatter defines the entry, and the compile
      // reads the file: one parse error, on both backends.
      assert.match(outcomes.native[1].message, /bad\.json/, JSON.stringify(outcomes.native[1]));
      assert.deepEqual(outcomes.wasm, outcomes.native);
      // The WASM scan of the template lists no data file, so it opens none.
      const { modules } = await buildModulesMap(plain, engines.wasm, true);
      assert.deepEqual(Object.keys(modules), ['plain.mds']);
    });
  });

  test('TP-52: check, checkFile, lint and lintFile never report mds::yaml for a bad data file; compileFile does', async (t) => {
    if (!requireEngines(t, engines, 'TP-52')) return;
    await withProject('mds-tp52-', async (root) => {
      await put(root, 'bad.json', BAD_JSON);
      const entry = await put(root, 'main.mds', BAD_ENTRY);
      // The WASM backend takes no base directory: it has no file system for a string.
      const base = { native: { basePath: root }, wasm: undefined };
      const outcomes = {};
      for (const backend of ['native', 'wasm']) {
        outcomes[backend] = await backendCalls(backend, [
          ['compileFile', entry],
          ['checkFile', entry],
          ['lintFile', entry],
          ['check', BAD_ENTRY, base[backend]],
          ['lint', BAD_ENTRY, base[backend]],
        ]);
      }
      // Control: compileFile reads the file and reports its parse error, on both backends.
      const [compiled] = outcomes.native;
      assert.equal(compiled.error?.code, 'mds::yaml', JSON.stringify(compiled));
      assert.deepEqual(outcomes.wasm[0], compiled);
      // Control for the string calls: a native compile from that base directory reads it.
      const [stringCompile] = await backendCalls('native', [['compile', BAD_ENTRY, base.native]]);
      assert.deepEqual(stringCompile.error, compiled.error);
      for (const backend of ['native', 'wasm']) {
        for (const [i, method] of ['checkFile', 'lintFile', 'check', 'lint'].entries()) {
          const outcome = outcomes[backend][i + 1];
          assert.equal(outcome.error, undefined, `${backend} ${method}: ${JSON.stringify(outcome)}`);
          assert.doesNotMatch(JSON.stringify(outcome.result), /mds::yaml/, `${backend} ${method}`);
        }
      }
      // A check warns once that the entry is not read, the same on both backends.
      assert.deepEqual(outcomes.native[1].result.warnings, [
        'data pending; output was not evaluated: "main.mds", entry "cfg"',
      ]);
      assert.deepEqual(outcomes.wasm[1], outcomes.native[1]);
      // A string source has another name on each backend: native names it `<source>`, the
      // WASM engine by its default file name. The name is the only difference.
      assert.deepEqual(outcomes.native[3].result.warnings, [
        'data pending; output was not evaluated: "<source>", entry "cfg"',
      ]);
      assert.deepEqual(outcomes.wasm[3].result.warnings, [
        'data pending; output was not evaluated: "input.mds", entry "cfg"',
      ]);
    });
  });
});
