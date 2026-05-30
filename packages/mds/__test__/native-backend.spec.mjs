/**
 * Native backend tests for @mdscript/mds universal package.
 * Tests: U-N1 through U-N6
 *
 * Verifies that the native NAPI backend behaves correctly in isolation.
 */
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { createNativeBackend } from '../dist/backend/native.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);

const FIXTURES = path.join(__dirname, 'fixtures');
// Load the native addon through its loader (crates/mds-napi/index.js), which
// resolves the correct binary for the host. With `napi build --platform` (as CI
// builds it) the file is named mds-napi.<triple>.node, so requiring a bare
// mds-napi.node is not portable across build flags. The loader returns the same
// raw addon exports either way.
const napiAddon = require(path.join(__dirname, '../../..', 'crates/mds-napi/index.js'));
const nativeBackend = createNativeBackend(napiAddon);

describe('native backend', () => {
  test('U-N1: compile plain text matches expected output', () => {
    const result = nativeBackend.compile('Hello World!\n');
    assert.equal(result.output, 'Hello World!\n');
    assert.deepEqual(result.warnings, []);
    assert.deepEqual(result.dependencies, []);
  });

  test('U-N2: compile with frontmatter vars', () => {
    const source = '---\nname: Test\n---\nHello {name}!\n';
    const result = nativeBackend.compile(source);
    assert.ok(result.output.includes('Hello Test!'), `got: ${result.output}`);
  });

  test('U-N3: compile with runtime vars', () => {
    const result = nativeBackend.compile('Hello {name}!\n', { vars: { name: 'World' } });
    assert.equal(result.output, 'Hello World!\n');
  });

  test('U-N4: check returns warnings array', () => {
    const result = nativeBackend.check('Hello!\n');
    assert.ok(Array.isArray(result.warnings));
  });

  test('U-N5: compile syntax error throws', () => {
    assert.throws(() => nativeBackend.compile('Hello {name\n'));
  });

  test('U-N6: compileFile resolves with correct shape', async () => {
    const simpleMds = path.join(FIXTURES, 'simple.mds');
    const result = await nativeBackend.compileFile(simpleMds);
    assert.ok(typeof result.output === 'string');
    assert.ok(Array.isArray(result.warnings));
    assert.ok(Array.isArray(result.dependencies));
  });
});
