/**
 * Error shape tests for @mdscript/mds universal package.
 * Tests: U-E1 through U-E10
 */
import { test, describe, before } from 'node:test';
import assert from 'node:assert/strict';
import { compile, check, isMdsError, init, lintVirtual } from '../dist/node.js';

describe('error shape', () => {
  before(() => init());

  test('U-E1: compile syntax error is an Error instance', () => {
    try {
      compile('Hello {{name\n');
      assert.fail('expected error to be thrown');
    } catch (err) {
      assert.ok(err instanceof Error, `expected Error instance, got: ${typeof err}`);
    }
  });

  test('U-E2: compile syntax error has code property', () => {
    try {
      compile('Hello {{name\n');
      assert.fail('expected error to be thrown');
    } catch (err) {
      assert.ok(typeof (err).code === 'string', `expected code string, got: ${(err).code}`);
    }
  });

  test('U-E3: isMdsError returns true for MDS errors', () => {
    try {
      compile('Hello {{name\n');
      assert.fail('expected error to be thrown');
    } catch (err) {
      assert.ok(isMdsError(err), 'isMdsError should return true');
    }
  });

  test('U-E4: isMdsError returns false for regular errors', () => {
    const regularError = new Error('regular error');
    assert.equal(isMdsError(regularError), false);
  });

  test('U-E5: isMdsError returns false for non-errors', () => {
    assert.equal(isMdsError(null), false);
    assert.equal(isMdsError(undefined), false);
    assert.equal(isMdsError('string error'), false);
    assert.equal(isMdsError(42), false);
  });

  test('U-E9: isMdsError returns false for errors with non-mds:: code', () => {
    // isMdsError requires code.startsWith('mds::'); a system error code like
    // 'ENOENT' must not be mistaken for an MDS compiler error.
    const err = new Error('file not found');
    err.code = 'ENOENT';
    assert.equal(isMdsError(err), false);
  });

  test('U-E6: check syntax error has code property', () => {
    try {
      check('Hello {{name\n');
      assert.fail('expected error to be thrown');
    } catch (err) {
      assert.ok(isMdsError(err), 'should be MdsError');
      assert.ok(typeof err.code === 'string');
    }
  });

  test('U-E7: undefined variable error has syntax-related code', () => {
    try {
      // Using an undefined variable should error.
      compile('{{undefinedVar}}\n');
      assert.fail('expected error');
    } catch (err) {
      assert.ok(isMdsError(err), 'should be MdsError');
      assert.ok(typeof err.code === 'string', 'error code should be present');
    }
  });

  test('U-E8: error message is a non-empty string', () => {
    try {
      compile('Hello {{name\n');
      assert.fail('expected error');
    } catch (err) {
      assert.ok(err instanceof Error);
      assert.ok(typeof err.message === 'string');
      assert.ok(err.message.length > 0, 'error message should not be empty');
    }
  });

  // T-11 / U-E10..U-E-DIFF [AC-F3, AC-F4]: ESC-injection hardening (issue #176 / CWE-150).
  // `@include fo<ESC>o` — alias contains a raw ESC byte (U+001B) mid-token so
  // trim() cannot strip it.  The parser rejects the alias as an invalid identifier
  // and produces a MdsError::Syntax whose message interpolates the raw alias.
  // After the fix, err.message must carry the sanitized 6-char \u001B literal and
  // must contain no raw C0/DEL/C1 bytes.
  // Helper: assert no raw C0 (excl. \t \n), DEL, or C1 chars in a string.
  // Uses charCodeAt (UTF-16 code units); all C0/DEL/C1 codepoints are in BMP so
  // charCodeAt correctly identifies them without surrogate pair handling.
  function assertNoControlChars(s, label) {
    for (let i = 0; i < s.length; i++) {
      const code = s.charCodeAt(i);
      const isC0 = code < 0x20 && code !== 0x09 && code !== 0x0a;
      const isDel = code === 0x7f;
      const isC1 = code >= 0x80 && code <= 0x9f;
      // Bidi controls (Trojan Source, CVE-2021-42574), U+2028/U+2029 (JS string
      // literal terminators), and U+FEFF (invisible BOM). `\n` is allowed here;
      // wire-mode newline escaping is asserted explicitly by U-E14.
      const isFormatHazard =
        code === 0x200e || code === 0x200f ||
        code === 0x2028 || code === 0x2029 ||
        (code >= 0x202a && code <= 0x202e) ||
        (code >= 0x2066 && code <= 0x2069) ||
        code === 0xfeff;
      assert.ok(
        !isC0 && !isDel && !isC1 && !isFormatHazard,
        `${label}: raw hostile char U+${code.toString(16).toUpperCase().padStart(4,'0')} ` +
        `at index ${i} must not appear; got: ${JSON.stringify(s)}`
      );
    }
  }

  test('U-E10: control chars in error message are escaped to \\uXXXX literals', () => {
    // Build source string with raw ESC (0x1B) mid-alias at runtime to avoid any
    // editor/tool stripping the control byte.
    const esc = String.fromCharCode(0x1b);
    const source = `@include fo${esc}o\n`;
    try {
      compile(source);
      assert.fail('expected error to be thrown');
    } catch (err) {
      assert.ok(isMdsError(err), `expected MdsError, got: ${err}`);
      const msg = err.message;
      assert.ok(typeof msg === 'string' && msg.length > 0,
        'message must be a non-empty string');
      assertNoControlChars(msg, 'U-E10: err.message');
      // Sanitized literal \u001B must be present.
      assert.ok(
        msg.includes('\\u001B'),
        `sanitized \\u001B literal must appear in err.message; got: ${JSON.stringify(msg)}`
      );
    }
  });

  test('U-E11: DEL (U+007F) in error message is escaped to \\u007F literal', () => {
    // DEL (U+007F) in @include alias — serde_json does NOT escape DEL by default,
    // so this is a distinct load-bearing vector from U-E10 (ESC).
    const del = String.fromCharCode(0x7f);
    const source = `@include fo${del}o\n`;
    try {
      compile(source);
      assert.fail('expected error to be thrown');
    } catch (err) {
      assert.ok(isMdsError(err), `U-E11: expected MdsError, got: ${err}`);
      const msg = err.message;
      assert.ok(typeof msg === 'string' && msg.length > 0,
        'U-E11: message must be a non-empty string');
      assertNoControlChars(msg, 'U-E11: err.message');
      assert.ok(
        msg.includes('\\u007F'),
        `U-E11: sanitized \\u007F literal must appear in err.message; got: ${JSON.stringify(msg)}`
      );
    }
  });

  test('U-E12: U+0085 (NEL/C1) in lintVirtual module name is sanitized in diagnostic message', () => {
    // U+0085 (NEL) is a C1 control char that passes serde_yaml_ng YAML parsing
    // (unlike ESC/DEL), making it a reachable C1 ESC-injection vector for lintVirtual.
    // The duplicate-import rule fires and embeds the raw module name in its message.
    const nel = String.fromCharCode(0x85);
    const moduleName = `fo${nel}o.mds`;
    const modules = {
      [moduleName]: 'hi\n',
      'main.mds': `@import "./${moduleName}"\n@import "./${moduleName}"\n`,
    };
    const result = lintVirtual(modules, 'main.mds');
    assert.equal(result.version, 1, 'U-E12: version must be 1');
    const allDiags = result.files.flatMap((f) => f.diagnostics);
    assert.ok(
      allDiags.length > 0,
      'U-E12: expected at least one diagnostic; got: ' + JSON.stringify(allDiags),
    );
    for (const diag of allDiags) {
      if (typeof diag.message === 'string') {
        assertNoControlChars(diag.message, `U-E12: diag[${diag.rule}].message`);
      }
    }
    const hasSanitizedNel = allDiags.some(
      (d) => typeof d.message === 'string' && d.message.includes('\\u0085'),
    );
    assert.ok(
      hasSanitizedNel,
      'U-E12: expected \\u0085 in at least one diagnostic message; got: ' +
        JSON.stringify(allDiags.map((d) => d.message)),
    );
  });

  test('U-E13: U+202E (RLO) in lintVirtual module name is escaped on the wire', () => {
    // Trojan Source (CVE-2021-42574). U+202E is outside C0/DEL/C1, so it used to
    // reach the wire untouched and reverse how the rest of the line displays.
    const rlo = String.fromCharCode(0x202e);
    const moduleName = `fo${rlo}gnp.mds`;
    const modules = {
      [moduleName]: 'hi\n',
      'main.mds': `@import "./${moduleName}"\n@import "./${moduleName}"\n`,
    };
    const result = lintVirtual(modules, 'main.mds');
    assert.equal(result.version, 1, 'U-E13: version must be 1');
    const allDiags = result.files.flatMap((f) => f.diagnostics);
    assert.ok(
      allDiags.some((d) => d.rule === 'duplicate-import'),
      'U-E13: expected duplicate-import; got rules: ' +
        JSON.stringify(allDiags.map((d) => d.rule)),
    );
    for (const diag of allDiags) {
      if (typeof diag.message === 'string') {
        assertNoControlChars(diag.message, `U-E13: diag[${diag.rule}].message`);
      }
    }
    // Cheap invariant check only — NOT coverage of the `file`-key escape. The
    // hostile RLO is in the *imported* module's name, but this key is the *entry*
    // filename ("main.mds"), so no hostile byte reaches it and this cannot fail via
    // this vector (PF-013). Real `file`-key coverage: mds-core
    // `to_canonical_json_escapes_bidi_override`.
    for (const f of result.files) {
      assertNoControlChars(f.file, 'U-E13: files[].file');
    }
    assert.ok(
      allDiags.some((d) => typeof d.message === 'string' && d.message.includes('\\u202E')),
      'U-E13: expected \\u202E in at least one diagnostic message; got: ' +
        JSON.stringify(allDiags.map((d) => d.message)),
    );
  });

  test('U-E14: newline in a frontmatter key is escaped to \\u000A on the wire', () => {
    // Log-forging guard: a raw newline in a diagnostic message lets an attacker
    // forge what reads as a second, independent finding in any line-oriented
    // consumer of the JSON string value.
    //
    // Reachability: a newline inside an `@import "..."` path is rejected by the
    // lexer (vacuous route). A YAML double-quoted frontmatter key is not — the
    // \n escape decodes to a real newline that unused-variable embeds verbatim.
    const source =
      '---\n"a\\nerror[mds::forged]: FAKE\\nb": 1\n---\nHello\n';
    const result = lintVirtual({ 'main.mds': source }, 'main.mds');
    assert.equal(result.version, 1, 'U-E14: version must be 1');
    const allDiags = result.files.flatMap((f) => f.diagnostics);
    assert.ok(
      allDiags.some((d) => d.rule === 'unused-variable'),
      'U-E14: expected unused-variable; got rules: ' +
        JSON.stringify(allDiags.map((d) => d.rule)),
    );
    for (const diag of allDiags) {
      assert.ok(
        !diag.message.includes('\n'),
        `U-E14: raw newline must not survive into the wire message; got: ${JSON.stringify(diag.message)}`,
      );
    }
    assert.ok(
      allDiags.some((d) => d.message.includes('\\u000A')),
      'U-E14: expected \\u000A in at least one diagnostic message; got: ' +
        JSON.stringify(allDiags.map((d) => d.message)),
    );
    // Escaped, not stripped.
    assert.ok(
      allDiags.some((d) => d.message.includes('error[mds::forged]')),
      'U-E14: message body must be preserved verbatim; got: ' +
        JSON.stringify(allDiags.map((d) => d.message)),
    );
  });

  test('U-E-DIFF: native and WASM lintVirtual produce identical results for ESC-injection input', async () => {
    // Differential assertion: the same ESC-injection input run through both the
    // native (napi) and WASM backends must produce deeply equal results.
    // Skips gracefully when either backend is unavailable locally; must run in CI
    // where both backends are built.
    let native;
    try {
      const { createNativeBackend } = await import('../dist/backend/native.js');
      const { createRequire } = await import('node:module');
      const { fileURLToPath } = await import('node:url');
      const { join, dirname } = await import('node:path');
      const testDir = dirname(fileURLToPath(import.meta.url));
      const require = createRequire(import.meta.url);
      const napiAddon = require(join(testDir, '../../../crates/mds-napi/index.js'));
      native = createNativeBackend(napiAddon);
    } catch {
      return; // native backend not available — skip
    }

    let wasm;
    try {
      const { initWasmNode, createWasmBackend } = await import('../dist/backend/wasm.js');
      const wasmModule = await initWasmNode();
      wasm = createWasmBackend(wasmModule);
    } catch {
      return; // WASM backend not available — skip
    }

    // One vector covering every escape class: C0 (ESC), C1 (NEL), bidi override
    // (RLO), JS line separator, BOM — carried in the module NAME — plus the
    // wire-mode newline, carried in a YAML double-quoted frontmatter key (a
    // newline inside an `@import "..."` path is rejected by the lexer, so the
    // module-name route is unreachable for that one character).
    // PF-007: a per-surface golden cannot catch cross-surface divergence, so the
    // widened class has to be exercised through the differential too.
    const esc = String.fromCharCode(0x1b);
    const nel = String.fromCharCode(0x85);
    const rlo = String.fromCharCode(0x202e);
    const ls = String.fromCharCode(0x2028);
    const bom = String.fromCharCode(0xfeff);
    const moduleName = `fo${esc}${nel}${rlo}${ls}${bom}o.mds`;
    const mainSource =
      '---\n"a\\nerror[mds::forged]: FAKE\\nb": 1\n---\n' +
      `@import "./${moduleName}"\n@import "./${moduleName}"\n`;
    const modules = {
      [moduleName]: 'hi\n',
      'main.mds': mainSource,
    };

    const nativeResult = native.lintVirtual(modules, 'main.mds');
    const wasmResult = wasm.lintVirtual(modules, 'main.mds');

    // Non-vacuity: the differential is worthless if neither backend produced
    // diagnostics carrying the escaped forms. duplicate-import carries the module
    // name (ESC/NEL/RLO/LS/BOM); unused-variable carries the frontmatter key (\n).
    const nativeMessages = nativeResult.files
      .flatMap((f) => f.diagnostics)
      .map((d) => d.message)
      .filter((m) => typeof m === 'string');
    for (const escaped of ['\\u001B', '\\u0085', '\\u202E', '\\u2028', '\\uFEFF', '\\u000A']) {
      assert.ok(
        nativeMessages.some((m) => m.includes(escaped)),
        `U-E-DIFF: expected ${escaped} in some message; got: ` +
          JSON.stringify(nativeMessages),
      );
    }

    // deepEqual of plain-object round-trip proves wire-format parity.
    assert.deepEqual(
      JSON.parse(JSON.stringify(nativeResult)),
      JSON.parse(JSON.stringify(wasmResult)),
      'U-E-DIFF: native and WASM lintVirtual must produce identical results for the same input',
    );
  });
});
