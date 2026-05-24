/**
 * WASM backend compileFile/checkFile tests for @mds/mds universal package.
 * Tests: U-WCF1 through U-WCF8
 *
 * Uses subprocess isolation with MDS_BACKEND=wasm to force the WASM backend
 * for file operations. Each test spawns a separate subprocess to avoid
 * cross-contamination from the module-level backend singleton.
 */
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const exec = promisify(execFile);
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const pkgRoot = path.join(__dirname, '..');

const SIMPLE_MDS = path.join(__dirname, 'fixtures', 'simple.mds');
const IMPORT_CONSUMER_MDS = path.join(__dirname, 'fixtures', 'import_consumer.mds');
const ENTRY_MDS = path.join(__dirname, 'fixtures', 'imports', 'entry.mds');

/**
 * Spawn a subprocess with MDS_BACKEND=wasm, execute an inline ESM script that
 * imports from dist/node.js, and return the parsed JSON result written to stdout.
 *
 * @param {string} script - Inline ESM script. Must write JSON to stdout.
 * @param {Record<string,string>} [extraEnv] - Additional env vars (merged over process.env).
 */
async function runWasm(script, extraEnv = {}) {
  const { stdout } = await exec(
    process.execPath,
    ['--input-type=module', '-e', script],
    {
      cwd: pkgRoot,
      env: { ...process.env, MDS_BACKEND: 'wasm', ...extraEnv },
      timeout: 30000,
    },
  );
  return JSON.parse(stdout);
}

/**
 * Spawn a subprocess WITHOUT MDS_BACKEND to use the default (native) backend.
 */
async function runNative(script) {
  // Remove MDS_BACKEND from env to allow native detection.
  const env = { ...process.env };
  delete env['MDS_BACKEND'];
  const { stdout } = await exec(
    process.execPath,
    ['--input-type=module', '-e', script],
    {
      cwd: pkgRoot,
      env,
      timeout: 30000,
    },
  );
  return JSON.parse(stdout);
}

describe('WASM backend — compileFile/checkFile', () => {
  test('U-WCF1: WASM compileFile on simple file returns valid CompileResult shape', async () => {
    const result = await runWasm(`
      import { init, compileFile } from './dist/node.js';
      await init();
      const r = await compileFile(${JSON.stringify(SIMPLE_MDS)});
      process.stdout.write(JSON.stringify({ output: r.output, warnings: r.warnings, dependencies: r.dependencies }));
    `);
    assert.equal(typeof result.output, 'string', 'output must be string');
    assert.ok(result.output.length > 0, 'output must not be empty');
    assert.ok(Array.isArray(result.warnings), 'warnings must be array');
    assert.ok(Array.isArray(result.dependencies), 'dependencies must be array');
  });

  test('U-WCF2: WASM compileFile with imports resolves dependencies', async () => {
    const result = await runWasm(`
      import { init, compileFile } from './dist/node.js';
      await init();
      const r = await compileFile(${JSON.stringify(IMPORT_CONSUMER_MDS)});
      process.stdout.write(JSON.stringify({ output: r.output, warnings: r.warnings, dependencies: r.dependencies }));
    `);
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
    const result = await runWasm(`
      import { init, compileFile } from './dist/node.js';
      await init();
      const r = await compileFile(${JSON.stringify(ENTRY_MDS)});
      process.stdout.write(JSON.stringify({ output: r.output, warnings: r.warnings, dependencies: r.dependencies }));
    `);
    assert.equal(typeof result.output, 'string', 'output must be string');
    assert.ok(result.output.length > 0, 'output must not be empty');
  });

  test('U-WCF4: WASM compileFile with runtime vars overrides frontmatter', async () => {
    const result = await runWasm(`
      import { init, compileFile } from './dist/node.js';
      await init();
      const r = await compileFile(${JSON.stringify(SIMPLE_MDS)}, { vars: { count: 99 } });
      process.stdout.write(JSON.stringify({ output: r.output, warnings: r.warnings, dependencies: r.dependencies }));
    `);
    assert.equal(typeof result.output, 'string', 'output must be string');
    assert.ok(
      result.output.includes('99'),
      `expected runtime var override (count=99) in output, got: ${result.output}`,
    );
  });

  test('U-WCF5: WASM checkFile returns valid CheckResult shape', async () => {
    const result = await runWasm(`
      import { init, checkFile } from './dist/node.js';
      await init();
      const r = await checkFile(${JSON.stringify(SIMPLE_MDS)});
      process.stdout.write(JSON.stringify({ warnings: r.warnings, dependencies: r.dependencies }));
    `);
    assert.ok(Array.isArray(result.warnings), 'warnings must be array');
    assert.ok(Array.isArray(result.dependencies), 'dependencies must be array');
  });

  test('U-WCF6: WASM compileFile on nonexistent file rejects with error', async () => {
    const script = `
      import { init, compileFile } from './dist/node.js';
      await init();
      try {
        await compileFile('/nonexistent/path/file.mds');
        process.stdout.write(JSON.stringify({ threw: false }));
      } catch (e) {
        process.stdout.write(JSON.stringify({ threw: true, message: e.message }));
      }
    `;
    const { stdout } = await exec(
      process.execPath,
      ['--input-type=module', '-e', script],
      {
        cwd: pkgRoot,
        env: { ...process.env, MDS_BACKEND: 'wasm' },
        timeout: 30000,
      },
    );
    const result = JSON.parse(stdout);
    assert.ok(result.threw, 'compileFile on nonexistent path must throw');
  });

  test('U-WCF7: WASM compileFile output matches native compileFile output (parity)', async () => {
    const script = `
      import { init, compileFile } from './dist/node.js';
      await init();
      const r = await compileFile(${JSON.stringify(SIMPLE_MDS)});
      process.stdout.write(JSON.stringify({ output: r.output, warnings: r.warnings, dependencies: r.dependencies }));
    `;
    const [wasmResult, nativeResult] = await Promise.all([
      runWasm(script),
      runNative(script),
    ]);
    assert.equal(
      wasmResult.output,
      nativeResult.output,
      `WASM and native compileFile output must match.\nWASM: ${wasmResult.output}\nNative: ${nativeResult.output}`,
    );
  });

  test('U-WCF8: WASM checkFile output matches native checkFile output (parity)', async () => {
    const script = `
      import { init, checkFile } from './dist/node.js';
      await init();
      const r = await checkFile(${JSON.stringify(SIMPLE_MDS)});
      process.stdout.write(JSON.stringify({ warnings: r.warnings, dependencies: r.dependencies }));
    `;
    const [wasmResult, nativeResult] = await Promise.all([
      runWasm(script),
      runNative(script),
    ]);
    assert.deepEqual(
      wasmResult.warnings,
      nativeResult.warnings,
      `WASM and native checkFile warnings must match`,
    );
    assert.deepEqual(
      wasmResult.dependencies,
      nativeResult.dependencies,
      `WASM and native checkFile dependencies must match`,
    );
  });
});
