/**
 * WASM backend compileFile/checkFile tests for @mdscript/mds universal package.
 * Tests: U-WCF1 through U-WCF11
 *
 * Uses subprocess isolation with MDS_BACKEND=wasm to force the WASM backend
 * for file operations. Each test spawns a separate subprocess to avoid
 * cross-contamination from the module-level backend singleton.
 */
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { SIMPLE_MDS, IMPORT_CONSUMER_MDS, ENTRY_MDS, __dirname } from './helpers.mjs';
import path from 'node:path';

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
});
