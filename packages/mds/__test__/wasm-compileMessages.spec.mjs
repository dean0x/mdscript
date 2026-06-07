/**
 * WASM backend compileMessages tests for @mdscript/mds universal package.
 * Tests: U-WCM1 through U-WCM6
 *
 * Uses subprocess isolation with MDS_BACKEND=wasm to force the WASM backend.
 * Each test spawns a separate subprocess to avoid cross-contamination from the
 * module-level backend singleton — mirroring the pattern in wasm-compileFile.spec.mjs.
 *
 * These tests exercise the real WASM FFI adapter (packages/mds/src/backend/wasm.ts
 * createWasmBackend → wasmModule.compileMessages) with actual source compilation,
 * which compile-messages.spec.mjs does NOT do (it runs through the native backend
 * when the native addon is present).
 */
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { __dirname } from './helpers.mjs';
import path from 'node:path';

const exec = promisify(execFile);
const pkgRoot = path.join(__dirname, '..');

/**
 * Spawn a subprocess, execute an inline ESM script that imports from dist/node.js,
 * and return the parsed JSON result written to stdout.
 *
 * Pass `MDS_BACKEND: 'wasm'` in env to force the WASM backend.
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

describe('WASM backend — compileMessages', () => {
  // ── U-WCM1: bare-word role, result shape ────────────────────────────────
  test('U-WCM1: WASM compileMessages with bare-word system role returns correct shape and content', async () => {
    const src = '@message system:\nYou are helpful.\n@end\n';
    const result = await runScript(`
      import { init, compileMessages } from './dist/node.js';
      await init();
      const r = compileMessages(${JSON.stringify(src)});
      process.stdout.write(JSON.stringify({
        messages: r.messages,
        warnings: r.warnings,
        dependencies: r.dependencies,
      }));
    `, wasmEnv());
    assert.ok(Array.isArray(result.messages), 'messages must be an array');
    assert.ok(Array.isArray(result.warnings), 'warnings must be an array');
    assert.ok(Array.isArray(result.dependencies), 'dependencies must be an array');
    assert.equal(result.messages.length, 1, 'must have exactly one message');
    assert.equal(result.messages[0].role, 'system');
    assert.equal(result.messages[0].content, 'You are helpful.');
  });

  // ── U-WCM2: multiple ordered messages ───────────────────────────────────
  test('U-WCM2: WASM compileMessages returns multiple messages in source order', async () => {
    const src = '@message system:\nSys.\n@end\n@message user:\nUsr.\n@end\n@message assistant:\nAst.\n@end\n';
    const result = await runScript(`
      import { init, compileMessages } from './dist/node.js';
      await init();
      const r = compileMessages(${JSON.stringify(src)});
      process.stdout.write(JSON.stringify({ messages: r.messages }));
    `, wasmEnv());
    assert.equal(result.messages.length, 3, `expected 3 messages, got: ${JSON.stringify(result.messages)}`);
    assert.equal(result.messages[0].role, 'system');
    assert.equal(result.messages[1].role, 'user');
    assert.equal(result.messages[2].role, 'assistant');
  });

  // ── U-WCM3: dynamic {role} via vars ─────────────────────────────────────
  test('U-WCM3: WASM compileMessages resolves dynamic role from vars', async () => {
    const src = '@message {r}:\nContent.\n@end\n';
    const result = await runScript(`
      import { init, compileMessages } from './dist/node.js';
      await init();
      const r = compileMessages(${JSON.stringify(src)}, { vars: { r: 'assistant' } });
      process.stdout.write(JSON.stringify({ messages: r.messages }));
    `, wasmEnv());
    assert.equal(result.messages.length, 1);
    assert.equal(result.messages[0].role, 'assistant');
    assert.equal(result.messages[0].content, 'Content.');
  });

  // ── U-WCM4: no @message blocks throws MdsError ───────────────────────────
  test('U-WCM4: WASM compileMessages throws MdsError when no @message blocks present', async () => {
    const src = 'Hello world!\n';
    const result = await runScript(`
      import { init, compileMessages, isMdsError } from './dist/node.js';
      await init();
      try {
        compileMessages(${JSON.stringify(src)});
        process.stdout.write(JSON.stringify({ threw: false }));
      } catch (e) {
        process.stdout.write(JSON.stringify({
          threw: true,
          isMdsError: isMdsError(e),
          message: e.message,
        }));
      }
    `, wasmEnv());
    assert.ok(result.threw, 'must throw when no @message blocks');
    assert.ok(result.isMdsError, `error must be MdsError, message: ${result.message}`);
  });

  // ── U-WCM5: orphan text produces a warning (not an error) ────────────────
  test('U-WCM5: WASM compileMessages emits warning for orphan text outside @message', async () => {
    const src = 'Orphan text.\n@message user:\nQ?\n@end\n';
    const result = await runScript(`
      import { init, compileMessages } from './dist/node.js';
      await init();
      const r = compileMessages(${JSON.stringify(src)});
      process.stdout.write(JSON.stringify({ messages: r.messages, warnings: r.warnings }));
    `, wasmEnv());
    assert.equal(result.messages.length, 1, 'non-orphan message must be present');
    assert.ok(result.warnings.length > 0, 'expected at least one warning for orphan text');
    const hasOrphanWarn = result.warnings.some(
      (w) => w.includes('outside @message') || w.includes('orphan') || w.includes('ignored'),
    );
    assert.ok(hasOrphanWarn, `expected orphan warning; got: ${JSON.stringify(result.warnings)}`);
  });

  // ── U-WCM6: WASM output matches native output (parity) ──────────────────
  test('U-WCM6: WASM compileMessages output matches native compileMessages output (parity)', async () => {
    const src = '---\nname: World\n---\n@message system:\nYou are helpful.\n@end\n@message user:\nHello {name}!\n@end\n';
    const script = `
      import { init, compileMessages } from './dist/node.js';
      await init();
      const r = compileMessages(${JSON.stringify(src)});
      process.stdout.write(JSON.stringify({ messages: r.messages, warnings: r.warnings }));
    `;
    const [wasmResult, nativeResult] = await Promise.all([
      runScript(script, wasmEnv()),
      runScript(script, nativeEnv()),
    ]);
    assert.deepEqual(
      wasmResult.messages,
      nativeResult.messages,
      `WASM and native compileMessages messages must match.\nWASM: ${JSON.stringify(wasmResult.messages)}\nNative: ${JSON.stringify(nativeResult.messages)}`,
    );
    assert.deepEqual(
      wasmResult.warnings,
      nativeResult.warnings,
      'WASM and native compileMessages warnings must match',
    );
  });
});
