/**
 * Options-validation tests — assertKnownKeys wrapper-level enforcement.
 * Tests: U-OV-1 through U-OV-10
 *
 * Verifies that the universal @mdscript/mds wrapper rejects unknown option keys
 * with code === 'mds::invalid_options' before dispatching to any backend.
 * Since validation runs inside the wrapper (before backend dispatch) it is
 * backend-agnostic: the same rejection fires on native and WASM paths.
 */
import { test, describe, before } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import {
  compile,
  check,
  compileFile,
  checkFile,
  lint,
  lintFile,
  lintVirtual,
  isMdsError,
  init,
} from '../dist/node.js';
import * as os from 'node:os';
import * as fs from 'node:fs';
import * as path from 'node:path';

const require = createRequire(import.meta.url);

describe('options-validation', () => {
  before(() => init());

  // ── compile: typo'd key ────────────────────────────────────────────────────

  test('U-OV-1: compile rejects unknown key "sourceMaps"', () => {
    assert.throws(
      () => compile('Hello\n', { sourceMaps: true }),
      (err) => {
        assert.ok(isMdsError(err), `expected isMdsError, got: ${err}`);
        assert.equal(err.code, 'mds::invalid_options');
        assert.ok(err.message.includes('"sourceMaps"'), `key name in message: ${err.message}`);
        assert.ok(err.message.includes('recognised keys are:'), `format check: ${err.message}`);
        return true;
      },
    );
  });

  test('U-OV-2: compile rejects unknown key "varsJson"', () => {
    assert.throws(
      () => compile('Hello\n', { varsJson: '{}' }),
      (err) => {
        assert.ok(isMdsError(err));
        assert.equal(err.code, 'mds::invalid_options');
        return true;
      },
    );
  });

  test('U-OV-3: compile rejects snake_case alias "source_map"', () => {
    assert.throws(
      () => compile('Hello\n', { source_map: true }),
      (err) => {
        assert.ok(isMdsError(err));
        assert.equal(err.code, 'mds::invalid_options');
        return true;
      },
    );
  });

  // ── check: typo'd key ─────────────────────────────────────────────────────

  test('U-OV-4: check rejects unknown key "base_path" (snake_case)', () => {
    assert.throws(
      () => check('Hello\n', { base_path: '.' }),
      (err) => {
        assert.ok(isMdsError(err));
        assert.equal(err.code, 'mds::invalid_options');
        assert.ok(err.message.includes('"base_path"'), `key name in message: ${err.message}`);
        return true;
      },
    );
  });

  test('U-OV-5: check rejects sourceMap (not valid for check)', () => {
    assert.throws(
      () => check('Hello\n', { sourceMap: true }),
      (err) => {
        assert.ok(isMdsError(err));
        assert.equal(err.code, 'mds::invalid_options');
        return true;
      },
    );
  });

  // ── lint: typo'd key ──────────────────────────────────────────────────────

  test('U-OV-6: lint rejects unknown key "base_path" (snake_case)', () => {
    assert.throws(
      () => lint('Hello\n', { base_path: '.' }),
      (err) => {
        assert.ok(isMdsError(err));
        assert.equal(err.code, 'mds::invalid_options');
        assert.ok(err.message.includes('"base_path"'), `key name: ${err.message}`);
        return true;
      },
    );
  });

  // ── lintVirtual: basePath not allowed there ────────────────────────────────

  test('U-OV-7: lintVirtual rejects basePath (not in LintFileOptions)', () => {
    const modules = { 'a.mds': 'Hello\n' };
    assert.throws(
      () => lintVirtual(modules, 'a.mds', { basePath: '.' }),
      (err) => {
        assert.ok(isMdsError(err));
        assert.equal(err.code, 'mds::invalid_options');
        assert.ok(err.message.includes('"basePath"'), `key name: ${err.message}`);
        return true;
      },
    );
  });

  // ── valid options pass through ─────────────────────────────────────────────

  test('U-OV-8: compile accepts all valid keys without error', () => {
    assert.doesNotThrow(() => compile('Hello\n', { vars: {}, sourceMap: false, sourcesContent: false }));
  });

  test('U-OV-9: check accepts vars without error', () => {
    assert.doesNotThrow(() => check('Hello\n', { vars: {} }));
  });

  test('U-OV-10: no options does not throw', () => {
    assert.doesNotThrow(() => compile('Hello\n'));
    assert.doesNotThrow(() => compile('Hello\n', undefined));
    assert.doesNotThrow(() => check('Hello\n'));
    assert.doesNotThrow(() => check('Hello\n', undefined));
  });

  // ── multiple unknown keys ──────────────────────────────────────────────────

  test('U-OV-11: compile rejects multiple unknown keys, lists them all', () => {
    assert.throws(
      () => compile('Hello\n', { sourceMaps: true, varsJson: '{}' }),
      (err) => {
        assert.ok(isMdsError(err));
        assert.equal(err.code, 'mds::invalid_options');
        // Plural form: "unknown option keys:"
        assert.ok(err.message.startsWith('unknown option keys:'), `plural form: ${err.message}`);
        assert.ok(err.message.includes('"sourceMaps"') && err.message.includes('"varsJson"'));
        return true;
      },
    );
  });

  // ── async file-ops reject unknown keys synchronously ──────────────────────

  test('U-OV-12: checkFile rejects unknown key (sourceMap not valid for check)', async () => {
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'mds-test-'));
    const file = path.join(tmp, 'ok.mds');
    fs.writeFileSync(file, 'Hello\n', 'utf8');
    await assert.rejects(
      () => checkFile(file, { sourceMap: true }),
      (err) => {
        assert.ok(isMdsError(err));
        assert.equal(err.code, 'mds::invalid_options');
        return true;
      },
    );
    fs.rmSync(tmp, { recursive: true, force: true });
  });

  test('U-OV-13: lintFile rejects unknown key "basePath"', async () => {
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'mds-test-'));
    const file = path.join(tmp, 'ok.mds');
    fs.writeFileSync(file, 'Hello\n', 'utf8');
    await assert.rejects(
      () => lintFile(file, { basePath: '.' }),
      (err) => {
        assert.ok(isMdsError(err));
        assert.equal(err.code, 'mds::invalid_options');
        return true;
      },
    );
    fs.rmSync(tmp, { recursive: true, force: true });
  });

  // ── message-parity: wrapper format matches backend format ─────────────────

  test('U-OV-14: wrapper error message format matches napi backend format', () => {
    let wrapperMsg = '';
    let backendMsg = '';

    try {
      compile('', { sourceMaps: true });
    } catch (err) {
      wrapperMsg = err.message;
    }

    // Load napi directly and trigger its own unknown-key rejection.
    let addon;
    try {
      addon = require('@mdscript/mds-napi');
    } catch {
      // Native addon not available — skip parity check.
      return;
    }
    try {
      addon.compile('', { sourceMaps: true });
    } catch (err) {
      backendMsg = err.message;
    }

    assert.ok(wrapperMsg.length > 0, 'wrapper should have thrown');
    assert.ok(backendMsg.length > 0, 'napi backend should have thrown');
    // Both must use the same phrasing format:
    //   Single key: `unknown option key "X"; recognised keys are: ...`
    assert.ok(
      wrapperMsg.startsWith('unknown option key "sourceMaps"'),
      `wrapper phrasing: ${wrapperMsg}`,
    );
    assert.ok(
      backendMsg.startsWith('unknown option key "sourceMaps"'),
      `backend phrasing: ${backendMsg}`,
    );
    assert.ok(wrapperMsg.includes('recognised keys are:'), `wrapper format: ${wrapperMsg}`);
    assert.ok(backendMsg.includes('recognised keys are:'), `backend format: ${backendMsg}`);
  });
});
