/**
 * Data-file pre-scanner tests for @mdscript/mds (#405).
 * Tests: TP-20 (with its Windows rows), TP-21, TP-23, TP-26, TP-27, TP-56, TP-112, the data
 * key conflict, a refused data key that a later path resolves to, the data file count and
 * byte bounds, the case-variant data key, and the data keys that the engine checks with no
 * project root.
 *
 * The WASM backend's file pre-scanner reads the local data files of a compile with the
 * data path rules of the native backend. Each rule test compares the WASM backend with
 * the native backend for the same fixture, code and message text alike: a golden of one
 * backend cannot show that the two agree. Each refusal has a control in which the same
 * test, with the hazard taken out of its fixture, reads the file. The scanner itself is
 * never weakened to show that a test can fail.
 */
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs, { closeSync, constants as fsConstants, existsSync, lstatSync, openSync, readFileSync, rmSync } from 'node:fs';
import { syncBuiltinESMExports } from 'node:module';
import { mkdir, mkdtemp, realpath, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {
  __dirname,
  caseInsensitive,
  compileFileOutcomes,
  errorShape,
  loadEngines,
  requireEngines,
  symlinkOrSkip,
} from './helpers.mjs';

const { buildModulesMap, dataPathRefusal } = await import('../dist/util/module-scanner.js');
const engines = await loadEngines();

/** The table of data path vectors that the Rust tests read too. */
const VECTORS = JSON.parse(
  readFileSync(path.join(__dirname, '../../../crates/mds-core/tests/fixtures/data-path-vectors.json'), 'utf8'),
);

/** The scanner's bound on concurrent read-ahead opens; the walk holds one file more. */
const MAX_CONCURRENT_OPENS = 16;
/** How many of a module's records the scanner reads ahead of its walk. */
const MAX_IMPORTS_READ_AHEAD = 2 * MAX_CONCURRENT_OPENS;
/** The per-file cap of a data file (`mds::MAX_DATA_FILE_BYTES`). */
const MAX_DATA_FILE_BYTES = 10 * 1024 * 1024;
const onWindows = process.platform === 'win32';
const noFifo = onWindows && 'Windows has no named pipe in the file system';

/** Run `fn` with a new temporary directory, given by its canonical path, then remove it. */
async function withTemp(prefix, fn) {
  const dir = await realpath(await mkdtemp(path.join(os.tmpdir(), prefix)));
  try {
    return await fn(dir);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
}

/** Write `text` to `name` below `dir`, with its directories; return the file's path. */
async function put(dir, name, text) {
  const file = path.join(dir, name);
  await mkdir(path.dirname(file), { recursive: true });
  await writeFile(file, text);
  return file;
}

/** Make the project root `name` below `dir`, marked with `.mdsroot`. */
async function project(dir, name) {
  const root = path.join(dir, name);
  await put(root, '.mdsroot', '');
  return root;
}

/** An MDS module whose `data:` entries are `entries` (name → path), then `body`. */
function dataModule(entries, body) {
  const lines = Object.entries(entries)
    .map(([name, file]) => `  ${name}: ${JSON.stringify(file)}\n`)
    .join('');
  return `---\ndata:\n${lines}---\n${body}`;
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

/**
 * The outcome of `files` on the native backend, after the WASM backend is shown to give
 * the same outcome for each, code and message alike.
 */
async function nativeEqualsWasm(files, options = {}, root = undefined) {
  const relative = root === undefined ? (outcome) => outcome : belowRoot(root);
  const native = (await compileFileOutcomes('native', files, options)).map(relative);
  const wasm = (await compileFileOutcomes('wasm', files, options)).map(relative);
  assert.deepEqual(wasm, native, 'the WASM backend must give the native outcome for every file');
  return native;
}

/**
 * The WASM engine, with a record of the files whose bytes reach its `preflightData`: the
 * last step of a data read. A file the scanner refuses before that step is not in it.
 */
function recordingEngine() {
  const { wasm } = engines;
  const preflighted = [];
  return {
    preflighted,
    engine: {
      scanImportRecords: (source, asBase) => wasm.scanImportRecords(source, asBase),
      preflightModule: (bytes, display, typed) => wasm.preflightModule(bytes, display, typed),
      preflightData: (bytes, display) => {
        preflighted.push(display);
        return wasm.preflightData(bytes, display);
      },
    },
  };
}

/** Scan `entry` for a compile; return the scan's result and the files it preflighted. */
async function scanData(entry) {
  const { engine, preflighted } = recordingEngine();
  const result = await buildModulesMap(entry, engine, true);
  return { ...result, preflighted };
}

/** Run `fn` while `fs.promises.open` is the function that `wrap` makes of the real one. */
async function withOpen(wrap, fn) {
  const realOpen = fs.promises.open;
  fs.promises.open = wrap(realOpen);
  syncBuiltinESMExports();
  try {
    return await fn();
  } finally {
    fs.promises.open = realOpen;
    syncBuiltinESMExports();
  }
}

/**
 * `promise`, or a failure after `ms`: then an open that waits on the named pipe `fifo` is
 * released by a second open, so the failure ends the test and not the whole run.
 */
async function withinDeadline(promise, fifo, ms) {
  let timer;
  const deadline = new Promise((_, reject) => {
    timer = setTimeout(() => {
      try {
        closeSync(openSync(fifo, fsConstants.O_RDWR | fsConstants.O_NONBLOCK));
      } catch {
        // The pipe is not there: nothing waits on it.
      }
      reject(new Error(`the scan waited on the named pipe ${fifo}`));
    }, ms);
  });
  try {
    return await Promise.race([promise, deadline]);
  } finally {
    clearTimeout(timer);
  }
}

/** The data path of a vector row, with its code point put in. */
function vectorPath(row) {
  return row.codePoint === undefined ? row.path : row.path.replace('@CODEPOINT@', String.fromCodePoint(row.codePoint));
}

/**
 * The rows that pass the rule on the path as written: the resolution refuses them. A path
 * that climbs above the key root is not read by the scanner, and the engine refuses it.
 */
const RESOLVED_STAGE = new Set(['escapes project directory']);

/**
 * The rows that a compile refuses at declaration, before the path rule: the entry's form
 * check (a string that is not dot-relative is no known form) and the reference rule of a
 * file path. The path rule of each still has the listed code (`dataPathRefusal`).
 */
const DECLARATION_STAGE = {
  'must start': 'entry "d": expected a dot-relative path, an HTTPS URL or a request map',
  references: 'entry "d", field "path": file paths cannot contain `{{`',
};

/**
 * Write the shared vector fixture below `tmp`: the files that the table's paths name, so a
 * rule that let a path through would read its file. Return the project root and the
 * entry file of each portable row.
 */
async function vectorProject(tmp) {
  const root = await project(tmp, 'project');
  // Lower case first: on a case-insensitive volume `a.JSON` is then the same file.
  const names = [
    'modules/a.json',
    'modules/a.yaml',
    'modules/a.yml',
    'modules/a.JSON',
    'a.json',
    'modules/a.txt',
    'modules/{{name}}.json',
    '.env/a.json',
    'node_modules/a.json',
    'NoDe_MoDuLeS/a.json',
  ];
  for (const row of VECTORS.portable) {
    if (row.codePoint !== undefined) names.push(vectorPath(row).slice(2));
  }
  for (const name of names) await put(root, name, 'true');
  // Outside the project: a path that escapes the root names this file.
  await put(tmp, 'a.json', 'false');
  const files = [];
  for (const [i, row] of VECTORS.portable.entries()) {
    files.push(await put(path.join(root, row.from ?? ''), `row${i}.mds`, dataModule({ d: vectorPath(row) }, '{{d}}\n')));
  }
  return { root, files };
}

describe('WASM backend data pre-scanner (#405)', () => {
  test('TP-20 TP-26: each row of the shared data path table gives the native outcome on the WASM backend', async (t) => {
    if (!requireEngines(t, engines, 'TP-20')) return;
    await withTemp('mds-tp20-', async (tmp) => {
      const { files } = await vectorProject(tmp);
      const native = await nativeEqualsWasm(files);
      const mismatches = [];
      for (const [i, row] of VECTORS.portable.entries()) {
        const outcome = native[i];
        const label = vectorPath(row);
        if (row.code === null) {
          // Control: a permitted path is read, on both backends (the equality above).
          if (outcome.output !== 'true\n') mismatches.push({ label, outcome });
        } else if (Object.hasOwn(DECLARATION_STAGE, row.reason)) {
          const expected = {
            code: 'mds::data',
            message: `data error in "row${i}.mds", ${DECLARATION_STAGE[row.reason]}`,
            help: row.reason === 'must start' ? '`data` is reserved for data sources; rename the variable' : null,
            span: null,
          };
          if (JSON.stringify(outcome) !== JSON.stringify(expected)) mismatches.push({ label, outcome, expected });
        } else if (outcome.code !== row.code || !outcome.message.includes(row.reason)) {
          mismatches.push({ label, outcome, expected: { code: row.code, reason: row.reason } });
        }
      }
      assert.deepEqual(mismatches, []);
    });
  });

  test('TP-20 TP-26: the TS scanner refuses each table row with its listed code and reads each permitted one', async (t) => {
    if (!requireEngines(t, engines, 'TP-20 scanner')) return;
    await withTemp('mds-tp20s-', async (tmp) => {
      const { files } = await vectorProject(tmp);
      const mismatches = [];
      for (const [i, row] of VECTORS.portable.entries()) {
        const label = vectorPath(row);
        const scan = await scanData(files[i]);
        const dataKeys = Object.keys(scan.modules).filter((key) => !key.endsWith('.mds'));
        if (row.code === null) {
          // Control: the file reaches the engine's preflightData and the module map.
          if (scan.preflighted.length !== 1 || scan.modules[dataKeys[0]] !== 'true') {
            mismatches.push({ label, preflighted: scan.preflighted, dataKeys });
          }
          continue;
        }
        // No byte of a refused file reaches the engine, and the map holds no data file.
        if (scan.preflighted.length !== 0 || dataKeys.length !== 0) {
          mismatches.push({ label, preflighted: scan.preflighted, dataKeys });
        }
        const refusal = dataPathRefusal(label);
        if (RESOLVED_STAGE.has(row.reason)) {
          // The rule as written lets the path through; the scanner leaves it to the engine,
          // whose refusal is native's (the test above).
          if (refusal !== undefined || scan.dataRefusals.size !== 0) {
            mismatches.push({ label, refusal: refusal === undefined ? null : errorShape(refusal) });
          }
        } else if (refusal?.code !== row.code || !refusal.message.includes(row.reason)) {
          mismatches.push({ label, refusal: refusal === undefined ? null : errorShape(refusal) });
        }
      }
      assert.deepEqual(mismatches, []);
    });
  });

  test('TP-26: a node_modules segment in any ASCII case is refused again on the canonical key, as on native', async (t) => {
    if (!requireEngines(t, engines, 'TP-26')) return;
    await withTemp('mds-tp26-', async (tmp) => {
      const root = await project(tmp, 'p');
      await put(root, 'NoDe_MoDuLeS/x/a.json', 'true');
      await put(root, 'modules/a.json', 'true');
      // The paths as written hold no node_modules segment; the canonical keys do or do not.
      if (!(await symlinkOrSkip(t, path.join('NoDe_MoDuLeS', 'x'), path.join(root, 'lnk'), 'dir'))) return;
      // Control: a link to a permitted directory, so only the target's segment differs.
      if (!(await symlinkOrSkip(t, 'modules', path.join(root, 'ok'), 'dir'))) return;
      const refused = await put(root, 'nm.mds', dataModule({ d: './lnk/a.json' }, '{{d}}\n'));
      const control = await put(root, 'ok.mds', dataModule({ d: './ok/a.json' }, '{{d}}\n'));
      const native = await nativeEqualsWasm([refused, control]);
      assert.deepEqual(native, [
        {
          code: 'mds::import',
          message: 'import error: data path "./lnk/a.json": node_modules segments are not allowed',
          help: null,
          span: null,
        },
        { output: 'true\n' },
      ]);
      // The refusal comes before the read: no byte of the file reaches the engine.
      assert.deepEqual((await scanData(refused)).preflighted, []);
      assert.deepEqual((await scanData(control)).preflighted, ['modules/a.json']);
    });
  });

  test('TP-21: a symbolic link as the final component, and a linked directory outside the root, are refused as on native', async (t) => {
    if (!requireEngines(t, engines, 'TP-21')) return;
    await withTemp('mds-tp21-', async (tmp) => {
      await put(tmp, 'outside/a.json', 'false');
      const attack = await project(tmp, 'attack');
      await put(attack, 'real.json', 'true');
      if (!(await symlinkOrSkip(t, 'real.json', path.join(attack, 'link.json'), 'file'))) return;
      if (!(await symlinkOrSkip(t, path.join('..', 'outside'), path.join(attack, 'out'), 'dir'))) return;
      // Control: the same names as a regular file and a real directory in the project.
      const control = await project(tmp, 'control');
      await put(control, 'link.json', 'true');
      await put(control, 'out/a.json', 'true');
      const files = [];
      for (const root of [attack, control]) {
        files.push(await put(root, 'link.mds', dataModule({ d: './link.json' }, '{{d}}\n')));
        files.push(await put(root, 'out.mds', dataModule({ d: './out/a.json' }, '{{d}}\n')));
      }
      const native = await nativeEqualsWasm(files);
      assert.deepEqual(native, [
        { code: 'mds::import', message: 'import error: symlinks are not allowed in imports: ./link.json', help: null, span: null },
        {
          code: 'mds::import',
          message: 'import error: import path escapes project directory: "./out/a.json"',
          help: null,
          span: null,
        },
        { output: 'true\n' },
        { output: 'true\n' },
      ]);
      const preflighted = [];
      for (const file of files) preflighted.push((await scanData(file)).preflighted);
      assert.deepEqual(preflighted, [[], [], ['link.json'], ['out/a.json']]);
    });
  });

  test('TP-23: a named pipe at a data path fails mds::io on both backends without a wait; control: a regular file is read', { skip: noFifo }, async (t) => {
    if (!requireEngines(t, engines, 'TP-23')) return;
    await withTemp('mds-tp23-', async (tmp) => {
      const piped = await project(tmp, 'piped');
      execFileSync('mkfifo', [path.join(piped, 'pipe.json')]);
      const control = await project(tmp, 'control');
      await put(control, 'pipe.json', 'true');
      const files = [piped, control].map((root) => path.join(root, 'main.mds'));
      for (const file of files) await writeFile(file, dataModule({ d: './pipe.json' }, '{{d}}\n'));
      // compileFileOutcomes fails on its subprocess timeout if a backend waits on the pipe.
      const native = await nativeEqualsWasm(files);
      assert.deepEqual(native, [
        { code: 'mds::io', message: 'cannot read pipe.json: not a regular file', help: null, span: null },
        { output: 'true\n' },
      ]);
    });
  });

  test('TP-23: the TS scanner opens a data file without a wait, so a pipe put there after the type check is refused', { skip: noFifo }, async (t) => {
    if (!requireEngines(t, engines, 'TP-23 race')) return;
    await withTemp('mds-tp23r-', async (tmp) => {
      const root = await project(tmp, 'p');
      const target = await put(root, 'pipe.json', 'true');
      const entry = await put(root, 'main.mds', dataModule({ d: './pipe.json' }, '{{d}}\n'));
      let swap = false;
      let swapped = false;
      // The pipe takes the file's place after every check before the open: only the open
      // itself can then keep the scan from a wait on it.
      const scan = () =>
        withOpen(
          (realOpen) =>
            async function swappingOpen(name, ...args) {
              if (swap && String(name) === target) {
                rmSync(target);
                execFileSync('mkfifo', [target]);
                swapped = true;
              }
              return realOpen.call(this, name, ...args);
            },
          () => withinDeadline(scanData(entry), target, 10_000),
        );
      // Control: the same scan with the file in place reads it.
      const read = await scan();
      assert.equal(read.modules['pipe.json'], 'true');
      assert.deepEqual(read.preflighted, ['pipe.json']);
      swap = true;
      const raced = await scan();
      assert.ok(swapped && lstatSync(target).isFIFO(), 'the pipe must take the place of the file before its open');
      assert.equal(Object.hasOwn(raced.modules, 'pipe.json'), false);
      assert.deepEqual(raced.preflighted, []);
      // The refusal that the compile gives back when it binds the entry: the native text
      // for a pipe at the path (the test above).
      assert.deepEqual([...raced.dataRefusals.values()].map(errorShape), [
        { code: 'mds::io', message: 'cannot read pipe.json: not a regular file', help: null, span: null },
      ]);
    });
  });

  test('TP-27: 64 data entries of one module go through the read-ahead window, at most 17 files open, in native order', async (t) => {
    if (!requireEngines(t, engines, 'TP-27')) return;
    await withTemp('mds-tp27-', async (tmp) => {
      const root = await project(tmp, 'p');
      // Declared in an order that is neither the order of the entry names nor of the paths.
      const many = {};
      for (let i = 0; i < 64; i += 1) {
        const file = (i * 37) % 64;
        many[`e${(i * 23) % 64}`] = `./d/f${file}.json`;
        await put(root, `d/f${file}.json`, `${file}`);
      }
      const big = await put(root, 'many.mds', dataModule(many, '{{e0}}\n'));
      const two = await put(root, 'two.mds', dataModule({ b: './d/f1.json', a: './d/f0.json' }, '{{a}}{{b}}\n'));
      const native = await nativeEqualsWasm([big, two], { sourceMap: true }, root);
      assert.equal(native[0].dependencies.length, 64, JSON.stringify(native[0]));
      // Control: a two-entry module, read in its declaration order on native.
      assert.deepEqual(native[1], { output: '01\n', dependencies: ['d/f1.json', 'd/f0.json'], sources: ['two.mds'] });

      let live = 0;
      let peak = 0;
      const opened = [];
      const counted = (realOpen) =>
        async function countedOpen(name, ...args) {
          const handle = await realOpen.call(this, name, ...args);
          opened.push(String(name));
          live += 1;
          peak = Math.max(peak, live);
          const close = handle.close.bind(handle);
          handle.close = async () => {
            live -= 1;
            return close();
          };
          return handle;
        };
      const scans = [];
      for (const entry of [big, two]) {
        scans.push(await withOpen(counted, () => scanData(entry)));
      }
      // The entry and each of its 64 data files are opened once; several at a time, never
      // more than the read-ahead opens and the walk's own file.
      assert.equal(opened.filter((name) => name.endsWith('.json')).length, 64 + 2);
      assert.ok(peak > 1, `the read-ahead window must hold several files open at once (peak ${peak})`);
      assert.ok(peak <= MAX_CONCURRENT_OPENS + 1, `at most ${MAX_CONCURRENT_OPENS + 1} files open, got ${peak}`);
      assert.ok(MAX_CONCURRENT_OPENS + 1 <= MAX_IMPORTS_READ_AHEAD, 'the open bound is within the read-ahead window');
      // The scanner puts the data files in its map in the order native reads them.
      const dataOrder = (scan) => Object.keys(scan.modules).filter((key) => key.endsWith('.json'));
      assert.deepEqual(dataOrder(scans[0]), native[0].dependencies);
      assert.deepEqual(dataOrder(scans[1]), native[1].dependencies);
    });
  });

  test('TP-56: WASM checkFile and lintFile read no data record: a pipe does not hang them and a bad file is not reported', async (t) => {
    if (!requireEngines(t, engines, 'TP-56')) return;
    await withTemp('mds-tp56-', async (tmp) => {
      const root = await project(tmp, 'p');
      await put(root, 'bad.json', '{"name": ');
      const bad = await put(root, 'bad.mds', dataModule({ d: './bad.json' }, '{{d.name}}\n'));
      const files = [bad];
      if (!noFifo) {
        execFileSync('mkfifo', [path.join(root, 'pipe.json')]);
        files.push(await put(root, 'pipe.mds', dataModule({ d: './pipe.json' }, '{{d.name}}\n')));
      }
      // Control: compileFile reads each data file — the bad one fails to parse, the pipe
      // fails the read.
      const compiled = await compileFileOutcomes('wasm', files);
      assert.equal(compiled[0].code, 'mds::yaml', JSON.stringify(compiled[0]));
      if (!noFifo) {
        assert.deepEqual(compiled[1], { code: 'mds::io', message: 'cannot read pipe.json: not a regular file', help: null, span: null });
      }
      // compileFileOutcomes fails on its subprocess timeout if a call waits on the pipe.
      for (const method of ['checkFile', 'lintFile']) {
        const results = await compileFileOutcomes('wasm', files, { method });
        for (const [i, result] of results.entries()) {
          assert.equal(result.code, undefined, `${method} ${files[i]}: ${JSON.stringify(result)}`);
          assert.doesNotMatch(JSON.stringify(result), /mds::yaml/, `${method} ${files[i]}`);
        }
      }
      // The scan of a check or a lint (`false`), and of a caller that leaves the flag out,
      // opens no data file. Control: the scan of a compile opens it.
      const opened = [];
      const recorded = (realOpen) =>
        async function recordedOpen(name, ...args) {
          opened.push(path.basename(String(name)));
          return realOpen.call(this, name, ...args);
        };
      for (const readData of [false, undefined, true]) {
        const { engine, preflighted } = recordingEngine();
        opened.length = 0;
        await withOpen(recorded, () => buildModulesMap(bad, engine, readData));
        const reads = readData === true ? ['bad.json'] : [];
        assert.deepEqual({ opened, preflighted }, { opened: ['bad.mds', ...reads], preflighted: reads }, String(readData));
      }
    });
  });

  test('TP-112: the root, extension, link, size and reference rules refuse before preflightData with the native code and text', async (t) => {
    if (!requireEngines(t, engines, 'TP-112')) return;
    await withTemp('mds-tp112-', async (tmp) => {
      await put(tmp, 'outside.json', 'false');
      const root = await project(tmp, 'p');
      const sub = path.join(root, 'sub');
      // Each control is in `sub/`, under the same name, with the hazard taken out of the
      // fixture: the path stays inside the root, the file has a data extension, the name
      // is a regular file, the file is at the cap, the entry is at the reference cap.
      await put(root, 'outside.json', 'true');
      await put(root, 'a.txt', 'true');
      await put(root, 'a.json', 'true');
      await put(sub, 'a.json', 'true');
      await put(sub, 'link.json', 'true');
      if (!(await symlinkOrSkip(t, 'a.json', path.join(root, 'link.json'), 'file'))) return;
      await put(root, 'big.json', ' '.repeat(MAX_DATA_FILE_BYTES + 1));
      await put(sub, 'big.json', `"${'a'.repeat(MAX_DATA_FILE_BYTES - 2)}"`);
      // An entry with 254 references in its body, one in its URL and one in a header.
      const refs = (extra) =>
        `---\nk: v\ndata:\n  a: ./a.json\n  b:\n    url: 'https://example.com/{{k}}'\n    headers:\n      X-Key: '{{k}}'\n` +
        `    body:\n      text: '${'{{k}}'.repeat(254)}${extra}'\n---\n{{a}}\n`;
      const rules = [
        ['root', dataModule({ d: '../outside.json' }, '{{d}}\n'), dataModule({ d: '../outside.json' }, '{{d}}\n')],
        ['ext', dataModule({ d: './a.txt' }, '{{d}}\n'), dataModule({ d: './a.json' }, '{{d}}\n')],
        ['link', dataModule({ d: './link.json' }, '{{d}}\n'), dataModule({ d: './link.json' }, '{{d}}\n')],
        ['size', dataModule({ d: './big.json' }, 'ok\n'), dataModule({ d: './big.json' }, 'ok\n')],
        ['refs', refs('{{missing}}'), refs('')],
      ];
      const refused = [];
      const controls = [];
      for (const [name, attack, control] of rules) {
        refused.push(await put(root, `${name}.mds`, attack));
        controls.push(await put(sub, `${name}.mds`, control));
      }
      const native = await nativeEqualsWasm([...refused, ...controls]);
      const error = (code, message) => ({ code, message, help: null, span: null });
      assert.deepEqual(native.slice(0, rules.length), [
        error('mds::import', 'import error: import path escapes project directory: "../outside.json"'),
        error('mds::import', 'import error: data path "./a.txt": expected the .json, .yaml or .yml extension'),
        error('mds::import', 'import error: symlinks are not allowed in imports: ./link.json'),
        error(
          'mds::resource_limit',
          `resource limit exceeded: file too large (${MAX_DATA_FILE_BYTES + 1} bytes, max ${MAX_DATA_FILE_BYTES} bytes): big.json`,
        ),
        error('mds::resource_limit', 'resource limit exceeded: "refs.mds", entry "b": too many references (max 256 per entry)'),
      ]);
      // Controls: each file is read; the entry at the reference cap reaches the network denial.
      assert.deepEqual(native.slice(rules.length, -1), [{ output: 'true\n' }, { output: 'true\n' }, { output: 'true\n' }, { output: 'ok\n' }]);
      assert.equal(native.at(-1).code, 'mds::network_denied', JSON.stringify(native.at(-1)));
      // No byte of a refused file reaches the engine's preflightData; each control's does.
      const preflighted = [];
      for (const file of [...refused, ...controls]) preflighted.push((await scanData(file)).preflighted);
      assert.deepEqual(preflighted, [
        [], [], [], [], [],
        ['outside.json'], ['sub/a.json'], ['sub/link.json'], ['sub/big.json'], ['sub/a.json'],
      ]);
    });
  });

  test('two data paths with one key that name two files are refused on the WASM backend, never bound to one file', async (t) => {
    if (!requireEngines(t, engines, 'data key conflict')) return;
    await withTemp('mds-dkey-', async (tmp) => {
      const root = await project(tmp, 'p');
      await put(root, 'z.json', '"ROOT"');
      await put(root, 'deep/z.json', '"DEEP"');
      await mkdir(path.join(root, 'deep', 'other'));
      if (!(await symlinkOrSkip(t, path.join('deep', 'other'), path.join(root, 'sub'), 'dir'))) return;
      // `./sub/../z.json` and `./z.json` have the one key `z.json`. On POSIX the OS applies
      // the `..` after it follows the link, so the first path names deep/z.json. Windows
      // removes `sub\..` first, so there both paths name the root z.json.
      const files = [
        await put(root, 'link-first.mds', dataModule({ a: './sub/../z.json', b: './z.json' }, '{{a}} {{b}}\n')),
        await put(root, 'link-last.mds', dataModule({ a: './z.json', b: './sub/../z.json' }, '{{a}} {{b}}\n')),
        // Control: the second path names the file of the first path by its own key.
        await put(root, 'control.mds', dataModule({ a: './sub/../z.json', b: './deep/z.json' }, '{{a}} {{b}}\n')),
        // Control: one key, two spellings, one file. Both paths name deep/z.json by the key
        // `z.json`, so the scan refuses neither.
        await put(root, 'one-file.mds', dataModule({ a: './sub/../z.json', b: './sub/./../z.json' }, '{{a}} {{b}}\n')),
      ];
      const native = await compileFileOutcomes('native', files);
      const wasm = await compileFileOutcomes('wasm', files);
      if (process.platform === 'win32') {
        assert.deepEqual(native.slice(0, 2), [{ output: 'ROOT ROOT\n' }, { output: 'ROOT ROOT\n' }]);
        assert.deepEqual(native[3], { output: 'ROOT ROOT\n' });
        assert.deepEqual(wasm, native);
        return;
      }
      // Native reads two files. The WASM engine has one file for the key, so the scan
      // refuses the second path and never gives the first file for it.
      assert.deepEqual(native, [
        { output: 'DEEP ROOT\n' },
        { output: 'ROOT DEEP\n' },
        { output: 'DEEP DEEP\n' },
        { output: 'DEEP DEEP\n' },
      ]);
      const conflict = (shown) => ({
        code: 'mds::import',
        message: `import error: two imports name different files by one module key, which the WASM backend cannot resolve: "${shown}"`,
        help: null,
        span: null,
      });
      assert.deepEqual(wasm, [
        conflict('./z.json'),
        conflict('./sub/../z.json'),
        { output: 'DEEP DEEP\n' },
        { output: 'DEEP DEEP\n' },
      ]);
    });
  });

  test('a refused data path is never bound to the file of a later path whose canonical key is its key', async (t) => {
    if (!requireEngines(t, engines, 'refused data key')) return;
    await withTemp('mds-dref-', async (tmp) => {
      // `./out/../a.json` names this file outside the root.
      await put(tmp, 'a.json', '"OUTSIDE"');
      await mkdir(path.join(tmp, 'outside'));
      const root = await project(tmp, 'p');
      await put(root, 'a.json', '"Ada"');
      await put(root, 'node_modules/a.json', '"NM"');
      await mkdir(path.join(root, 'node_modules', 'pkg'));
      // `./lnk/a.json` names a.json by the key `lnk/a.json`. Its canonical key is `a.json`.
      if (!(await symlinkOrSkip(t, '.', path.join(root, 'lnk'), 'dir'))) return;
      if (!(await symlinkOrSkip(t, path.join('..', 'outside'), path.join(root, 'out'), 'dir'))) return;
      if (!(await symlinkOrSkip(t, path.join('node_modules', 'pkg'), path.join(root, 'nm'), 'dir'))) return;
      // Each first path has the key `a.json`. On POSIX it is refused: its directory is
      // missing, or a link takes it out of the root, or into node_modules.
      const firsts = ['./missing/../a.json', './out/../a.json', './nm/../a.json'];
      const files = [];
      for (const [i, first] of firsts.entries()) {
        files.push(await put(root, `first${i}.mds`, dataModule({ a: first, b: './lnk/a.json' }, '{{a}} {{b}}\n')));
        // Control: the same two entries in the opposite order.
        files.push(await put(root, `last${i}.mds`, dataModule({ b: './lnk/a.json', a: first }, '{{a}} {{b}}\n')));
      }
      const native = await nativeEqualsWasm(files);
      if (process.platform === 'win32') {
        // Windows applies `..` before it follows a link, so each first path names a.json.
        assert.deepEqual(native, files.map(() => ({ output: 'Ada Ada\n' })));
        return;
      }
      const error = (code, message, help = null) => ({ code, message, help, span: null });
      const refusals = [
        error('mds::file_not_found', 'file not found: ./missing/../a.json', 'check the file path and ensure the file exists'),
        error('mds::import', 'import error: import path escapes project directory: "./out/../a.json"'),
        error('mds::import', 'import error: data path "./nm/../a.json": node_modules segments are not allowed'),
      ];
      assert.deepEqual(native, refusals.flatMap((refusal) => [refusal, refusal]));
      // A runtime variable replaces the first entry, so native reads only the second path.
      // The WASM engine cannot have the key `a.json` both refused and bound: the scan keeps
      // a refusal for the second path, and never binds a.json to the first.
      const replaced = files.filter((_, i) => i % 2 === 0);
      const vars = { a: 'X' };
      assert.deepEqual(await compileFileOutcomes('native', replaced, { vars }), replaced.map(() => ({ output: 'X Ada\n' })));
      const conflict = error(
        'mds::import',
        'import error: two imports name different files by one module key, which the WASM backend cannot resolve: "./lnk/a.json"',
      );
      assert.deepEqual(await compileFileOutcomes('wasm', replaced, { vars }), replaced.map(() => conflict));
      // A child entry replaces the first entry of its base: the same outcomes.
      await put(root, 'c.json', '"Kid"');
      await put(root, 'base.mds', dataModule({ a: './missing/../a.json', b: './lnk/a.json' }, '{{a}} {{b}}\n@block body:\n@end\n'));
      const child = await put(root, 'child.mds', dataModule({ a: './c.json' }, '@extends "./base.mds"\n@block body:\nchild\n@end\n'));
      assert.deepEqual(await compileFileOutcomes('native', [child]), [{ output: 'Kid Ada\nchild\n' }]);
      assert.deepEqual(await compileFileOutcomes('wasm', [child]), [conflict]);
      // The file is not in the map under the refused key.
      for (const file of [...replaced, child]) {
        assert.equal(Object.hasOwn((await scanData(file)).modules, 'a.json'), false, file);
      }
    });
  });

  test('on a case-insensitive volume, a refused data path is never bound to the file of a later case variant of its key', async (t) => {
    if (!requireEngines(t, engines, 'refused data key, case variant')) return;
    await withTemp('mds-drefcase-', async (tmp) => {
      if (!(await caseInsensitive(tmp))) {
        t.skip('needs a case-insensitive volume, where ./A.json can name a.json (the macOS and Windows default)');
        return;
      }
      const root = await project(tmp, 'p');
      await put(root, 'a.json', '"Ada"');
      // `./A.json` has the key `A.json`. Its canonical key is the on-disk spelling `a.json`.
      const files = [
        await put(root, 'first.mds', dataModule({ a: './missing/../a.json', b: './A.json' }, '{{a}} {{b}}\n')),
        // Control: the same two entries in the opposite order.
        await put(root, 'last.mds', dataModule({ b: './A.json', a: './missing/../a.json' }, '{{a}} {{b}}\n')),
      ];
      const native = await nativeEqualsWasm(files);
      if (process.platform === 'win32') {
        // Windows applies `..` first, so `./missing/../a.json` names a.json.
        assert.deepEqual(native, [{ output: 'Ada Ada\n' }, { output: 'Ada Ada\n' }]);
        return;
      }
      const missing = {
        code: 'mds::file_not_found',
        message: 'file not found: ./missing/../a.json',
        help: 'check the file path and ensure the file exists',
        span: null,
      };
      assert.deepEqual(native, [missing, missing]);
    });
  });

  test('TP-20 TP-26: each Windows row of the shared data path table gives the native outcome on the WASM backend', { skip: !onWindows && 'the Windows rows name forms that only Windows resolves' }, async (t) => {
    if (!requireEngines(t, engines, 'TP-20 Windows')) return;
    await withTemp('mds-tp20w-', async (tmp) => {
      const root = await project(tmp, 'project');
      await put(root, 'modules/a.json', 'true');
      await put(root, 'node_modules/a.json', 'true');
      // If this volume has no 8.3 name, give the short spelling a directory junction, as the
      // Rust test does. Either form resolves to node_modules before the segment check.
      const short = path.join(root, 'NODE_M~1');
      if (!existsSync(short) && !(await symlinkOrSkip(t, path.join(root, 'node_modules'), short, 'junction'))) return;
      const rows = [
        ...VECTORS.windows,
        // A trailing space hides a node_modules segment as a trailing dot does.
        { path: './node_modules /a.json', code: 'mds::import', reason: 'node_modules' },
        // A `\` separator: with a trailing dot, and as a control that reads the file.
        { path: './node_modules.\\a.json', code: 'mds::import', reason: 'node_modules' },
        { path: './modules\\a.json', code: null },
      ];
      const files = [];
      for (const [i, row] of rows.entries()) {
        files.push(await put(root, `row${i}.mds`, dataModule({ d: row.path }, '{{d}}\n')));
      }
      const native = await nativeEqualsWasm(files);
      const mismatches = [];
      for (const [i, row] of rows.entries()) {
        const outcome = native[i];
        const fits = row.code === null
          ? outcome.output === 'true\n'
          : outcome.code === row.code && outcome.message.includes(row.reason);
        if (!fits) mismatches.push({ path: row.path, outcome });
      }
      assert.deepEqual(mismatches, []);
      // No byte of a refused file reaches the engine. Control: the file of the last row does.
      const preflighted = [];
      for (const file of files) preflighted.push((await scanData(file)).preflighted);
      assert.deepEqual(preflighted, [...rows.slice(0, -1).map(() => []), ['modules/a.json']]);
    });
  });

  test('a data key that the WASM engine checks with no project root: a missing file gets the native refusal', async (t) => {
    if (!requireEngines(t, engines, 'root-less data key')) return;
    await withTemp('mds-drootless-', async (tmp) => {
      const root = await project(tmp, 'p');
      await put(root, 'a.json', '"Ada"');
      // The engine splits the key `a\..\missing.json` at `\` and sees a `..` segment.
      const missing = await put(root, 'missing.mds', dataModule({ d: './a\\..\\missing.json' }, '{{d}}\n'));
      // The same module reached through an import: native names the import.
      const imported = await put(root, 'imports.mds', '@import "./missing.mds" as m\nHi\n');
      // Control: the same form names a file that is there on Windows.
      const control = await put(root, 'control.mds', dataModule({ d: './a\\..\\a.json' }, '{{d}}\n'));
      const native = await nativeEqualsWasm([missing, imported, control]);
      const notFound = (shown) => ({
        code: 'mds::file_not_found',
        message: `file not found: ${shown}`,
        help: 'check the file path and ensure the file exists',
        span: null,
      });
      assert.deepEqual(native[0], notFound('./a\\..\\missing.json'));
      assert.deepEqual({ ...native[1], span: null }, notFound('./missing.mds'));
      assert.notEqual(native[1].span, null, 'native gives the import its span');
      assert.deepEqual(native[2], onWindows ? { output: 'Ada\n' } : notFound('./a\\..\\a.json'));
    });
  });

  test('known limit: the WASM backend refuses the POSIX data files named \\x.json and a:b.json, which native reads', { skip: onWindows && 'Windows does not allow these file names' }, async (t) => {
    if (!requireEngines(t, engines, 'root-less data key names')) return;
    await withTemp('mds-drootname-', async (tmp) => {
      const root = await project(tmp, 'p');
      await put(root, '\\x.json', '"BACK"');
      await put(root, 'a:b.json', '"COLON"');
      const shown = ['./\\x.json', './a:b.json'];
      const files = [];
      for (const [i, file] of shown.entries()) files.push(await put(root, `name${i}.mds`, dataModule({ d: file }, '{{d}}\n')));
      assert.deepEqual(await compileFileOutcomes('native', files), [{ output: 'BACK\n' }, { output: 'COLON\n' }]);
      // The key of each is its name, with no alias. The engine checks it with no project
      // root and refuses a key that starts with `\` or has `:` as its second byte.
      const escapes = (path) => ({
        code: 'mds::import',
        message: `import error: data path "${path}": escapes project directory`,
        help: null,
        span: null,
      });
      assert.deepEqual(await compileFileOutcomes('wasm', files), shown.map(escapes));
    });
  });

  test('the scan puts at most 256 data files in the map; past that, both backends give the native refusal', async (t) => {
    if (!requireEngines(t, engines, 'data file count')) return;
    await withTemp('mds-dcount-', async (tmp) => {
      const root = await project(tmp, 'p');
      // Four modules of 64 entries each, the cap per module, name 256 distinct files.
      const imports = [];
      for (let m = 0; m < 4; m += 1) {
        const entries = {};
        for (let i = 0; i < 64; i += 1) {
          const n = m * 64 + i;
          entries[`e${i}`] = `./d/f${n}.json`;
          await put(root, `d/f${n}.json`, `${n}`);
        }
        await put(root, `m${m}.mds`, dataModule(entries, '{{e0}}\n'));
        imports.push(`@import "./m${m}.mds" as m${m}\n`);
      }
      await put(root, 'd/f256.json', '256');
      const body = `${imports.join('')}@include m0\n`;
      // The entry module names a 257th file, after the files of its imports.
      const over = await put(root, 'over.mds', dataModule({ x: './d/f256.json' }, body));
      // Control: the same imports, with no data entry in the entry module.
      const control = await put(root, 'control.mds', body);
      const native = await nativeEqualsWasm([over, control]);
      assert.deepEqual(native, [
        {
          code: 'mds::resource_limit',
          message: 'resource limit exceeded: too many data entries (max 256 per compile)',
          help: null,
          span: null,
        },
        { output: '0\n' },
      ]);
      const dataKeysOf = (scan) => Object.keys(scan.modules).filter((key) => key.endsWith('.json'));
      const scan = await scanData(over);
      assert.equal(dataKeysOf(scan).length, 256);
      assert.equal(Object.hasOwn(scan.modules, 'd/f256.json'), false);
      assert.deepEqual([...scan.dataRefusals.values()].map(errorShape), [
        {
          code: 'mds::resource_limit',
          message: 'resource limit exceeded: too many data files (max 256 per compile)',
          help: null,
          span: null,
        },
      ]);
      const controlScan = await scanData(control);
      assert.equal(dataKeysOf(controlScan).length, 256);
      assert.equal(controlScan.dataRefusals.size, 0);
    });
  });

  test('data text over 32 MiB in one compile gets the native refusal on both backends; no file is admitted after it', async (t) => {
    if (!requireEngines(t, engines, 'data bytes')) return;
    await withTemp('mds-dbytes-', async (tmp) => {
      const root = await project(tmp, 'p');
      // Four files of 8.5 MiB each: three fit in the 32 MiB bound, the fourth does not.
      const size = 8.5 * 1024 * 1024;
      for (const name of ['a', 'b', 'c', 'd']) await put(root, `${name}.json`, `"${'x'.repeat(size - 2)}"`);
      await put(root, 'small.json', 'true');
      const three = { a: './a.json', b: './b.json', c: './c.json' };
      const over = await put(root, 'over.mds', dataModule({ ...three, d: './d.json', e: './small.json' }, 'ok\n'));
      // Control: the fourth entry names a small file.
      const control = await put(root, 'control.mds', dataModule({ ...three, d: './small.json' }, 'ok\n'));
      const native = await nativeEqualsWasm([over, control]);
      const bound = {
        code: 'mds::resource_limit',
        message: `resource limit exceeded: data files exceed ${32 * 1024 * 1024} bytes per compile`,
        help: null,
        span: null,
      };
      assert.deepEqual(native, [bound, { output: 'ok\n' }]);
      // The fourth file and every file after it stay out of the map, with the bound kept.
      const scan = await scanData(over);
      const dataKeysOf = (result) => Object.keys(result.modules).filter((key) => key.endsWith('.json'));
      assert.deepEqual(dataKeysOf(scan), ['a.json', 'b.json', 'c.json']);
      assert.deepEqual([...scan.dataRefusals.values()].map(errorShape), [bound, bound]);
      const controlScan = await scanData(control);
      assert.deepEqual(dataKeysOf(controlScan), ['a.json', 'b.json', 'c.json', 'small.json']);
      assert.equal(controlScan.dataRefusals.size, 0);
    });
  });

  test('a data path whose canonical key has no data extension gives the native mds::data on the WASM backend', async (t) => {
    if (!requireEngines(t, engines, 'case-variant data key')) return;
    await withTemp('mds-casekey-', async (tmp) => {
      if (!(await caseInsensitive(tmp))) {
        t.skip('needs a case-insensitive volume, where ./cfg.json can name CFG.JSON (the macOS and Windows default)');
        return;
      }
      const upper = await project(tmp, 'upper');
      await put(upper, 'CFG.JSON', '{"name": "Ada"}');
      // Control: the same fixture with the file spelled as the path spells it.
      const lower = await project(tmp, 'lower');
      await put(lower, 'cfg.json', '{"name": "Ada"}');
      const files = [upper, lower].map((root) => path.join(root, 'main.mds'));
      for (const file of files) await writeFile(file, dataModule({ cfg: './cfg.json' }, 'Hi {{cfg.name}}\n'));
      const native = await nativeEqualsWasm(files);
      assert.deepEqual(native, [
        {
          code: 'mds::data',
          message: 'data error in "main.mds", entry "cfg": resolved data file must have a .json, .yaml or .yml extension',
          help: null,
          span: null,
        },
        { output: 'Hi Ada\n' },
      ]);
      // A runtime variable replaces the entry: native binds no file and refuses none.
      const replayed = await nativeEqualsWasm(files.slice(0, 1), { vars: { cfg: { name: 'Bob' } } });
      assert.deepEqual(replayed, [{ output: 'Hi Bob\n' }]);
      // The scan reads no byte of the refused file; the control's file is read.
      assert.deepEqual((await scanData(files[0])).preflighted, []);
      assert.deepEqual((await scanData(files[1])).preflighted, ['cfg.json']);
    });
  });
});
