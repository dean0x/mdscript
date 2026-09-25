/**
 * Error shape tests for @mdscript/mds universal package.
 * Tests: U-E1 through U-E10
 */
import { test, describe, before } from 'node:test';
import assert from 'node:assert/strict';
import { compile, check, isMdsError, init, lintVirtual } from '../dist/node.js';
import { assertNoForbiddenChars, errorShape, escapeText, thrownBy } from './helpers.mjs';

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

  test('U-E12: U+0085 (NEL/C1) in an import path is refused with an escaped mds::import error', () => {
    // Route A (the import path / module name). Before #265 the hostile name reached
    // the lint rules and duplicate-import embedded it in a diagnostic; the import
    // string is now refused at the input boundary, so the error itself must carry
    // the escaped form. Message-escaping coverage for NEL lives on in route B below.
    const nel = String.fromCodePoint(0x85);
    const moduleName = `fo${nel}o.mds`;
    const modules = {
      [moduleName]: 'hi\n',
      'main.mds': `@import "./${moduleName}"\n@import "./${moduleName}"\n`,
    };
    const err = thrownBy(() => lintVirtual(modules, 'main.mds'), 'U-E12');
    assert.ok(isMdsError(err), `U-E12: expected MdsError, got: ${err}`);
    assert.equal(err.code, 'mds::import');
    assert.equal(
      err.message,
      `import error: import path contains forbidden character U+0085: "./fo${escapeText(0x85)}o.mds"`,
    );
    assertNoForbiddenChars(err.message, 'U-E12: err.message');
  });

  test('U-E12 (route B): NEL in a frontmatter key is sanitized in unused-variable message', () => {
    // Route B sibling of U-E12: same NEL (U+0085) control character, but carried by
    // an UNUSED FRONTMATTER KEY (unused-variable) rather than an import path /
    // module name (duplicate-import). #265 rejects hostile paths/module names at the
    // input boundary, which retired route-A coverage for NEL — a frontmatter key is
    // not a path, so this route stays reachable and keeps message-escaping coverage
    // alive.
    //
    // Written as a YAML double-quoted key with a YAML `\x85` escape so the .mds
    // source text itself carries no raw control byte (PF-018); serde_yaml_ng
    // decodes the escape into a real NEL codepoint, which unused-variable embeds
    // verbatim in its message before WIRE-sanitization escapes it back out.
    const source = '---\n"a\\x85payload\\x85b": 1\n---\nHello\n';
    assert.ok(
      !source.includes(String.fromCharCode(0x85)),
      'U-E12 (route B): source must carry no raw NEL byte, only the YAML escape',
    );
    const result = lintVirtual({ 'main.mds': source }, 'main.mds');
    assert.equal(result.version, 1, 'U-E12 (route B): version must be 1');
    const allDiags = result.files.flatMap((f) => f.diagnostics);
    assert.ok(
      allDiags.some((d) => d.rule === 'unused-variable'),
      'U-E12 (route B): expected unused-variable; got rules: ' +
        JSON.stringify(allDiags.map((d) => d.rule)),
    );
    for (const diag of allDiags) {
      if (typeof diag.message === 'string') {
        assertNoControlChars(diag.message, `U-E12 (route B): diag[${diag.rule}].message`);
      }
    }
    const hasSanitizedNel = allDiags.some(
      (d) => typeof d.message === 'string' && d.message.includes('\\u0085'),
    );
    assert.ok(
      hasSanitizedNel,
      'U-E12 (route B): expected \\u0085 in at least one diagnostic message; got: ' +
        JSON.stringify(allDiags.map((d) => d.message)),
    );
    assert.ok(
      allDiags.some((d) => typeof d.message === 'string' && d.message.includes('payload')),
      'U-E12 (route B): message body must be preserved verbatim; got: ' +
        JSON.stringify(allDiags.map((d) => d.message)),
    );
  });

  test('U-E13: U+202E (RLO) in an import path is refused with an escaped mds::import error', () => {
    // Trojan Source (CVE-2021-42574): "fo<RLO>gnp.mds" renders as "fopng.mds". Route A
    // (see U-E12): the import string is refused at the input boundary, and the error
    // must name the codepoint and show it escaped — a raw RLO in the message would
    // reverse how the rest of it displays. Route B below keeps the lint-message
    // escaping coverage.
    const rlo = String.fromCodePoint(0x202e);
    const moduleName = `fo${rlo}gnp.mds`;
    const modules = {
      [moduleName]: 'hi\n',
      'main.mds': `@import "./${moduleName}"\n@import "./${moduleName}"\n`,
    };
    const err = thrownBy(() => lintVirtual(modules, 'main.mds'), 'U-E13');
    assert.ok(isMdsError(err), `U-E13: expected MdsError, got: ${err}`);
    assert.equal(err.code, 'mds::import');
    assert.equal(
      err.message,
      `import error: import path contains forbidden character U+202E: "./fo${escapeText(0x202e)}gnp.mds"`,
    );
    assertNoForbiddenChars(err.message, 'U-E13: err.message');
  });

  test('U-E13 (route B): RLO in a frontmatter key is escaped on the wire', () => {
    // Route B sibling of U-E13: same U+202E RIGHT-TO-LEFT OVERRIDE, but carried by
    // an UNUSED FRONTMATTER KEY (unused-variable) rather than an import path /
    // module name (duplicate-import). See U-E12 (route B) above for the full
    // rationale (#265 retires route-A coverage; a frontmatter key is not a path, so
    // this route survives enforcement).
    const rlo = '\\u202E'; // YAML escape text, not a raw RLO byte (PF-018).
    const source = `---\n"a${rlo}payload${rlo}b": 1\n---\nHello\n`;
    assert.ok(
      !source.includes(String.fromCharCode(0x202e)),
      'U-E13 (route B): source must carry no raw RLO byte, only the YAML escape',
    );
    const result = lintVirtual({ 'main.mds': source }, 'main.mds');
    assert.equal(result.version, 1, 'U-E13 (route B): version must be 1');
    const allDiags = result.files.flatMap((f) => f.diagnostics);
    assert.ok(
      allDiags.some((d) => d.rule === 'unused-variable'),
      'U-E13 (route B): expected unused-variable; got rules: ' +
        JSON.stringify(allDiags.map((d) => d.rule)),
    );
    for (const diag of allDiags) {
      if (typeof diag.message === 'string') {
        assertNoControlChars(diag.message, `U-E13 (route B): diag[${diag.rule}].message`);
      }
    }
    assert.ok(
      allDiags.some((d) => typeof d.message === 'string' && d.message.includes('\\u202E')),
      'U-E13 (route B): expected \\u202E in at least one diagnostic message; got: ' +
        JSON.stringify(allDiags.map((d) => d.message)),
    );
    assert.ok(
      allDiags.some((d) => typeof d.message === 'string' && d.message.includes('payload')),
      'U-E13 (route B): message body must be preserved verbatim; got: ' +
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

  test('U-E-DIFF: native and WASM lintVirtual throw identical errors for a hostile import path', async (t) => {
    // Differential assertion: the same hostile import run through the native (napi)
    // and WASM backends must throw deeply equal errors. Before #265 this vector
    // produced a lint result (duplicate-import carried the name); both backends now
    // refuse the import string at the input boundary, so parity is asserted on the
    // thrown error. Lint-RESULT parity for the same escape class lives on in
    // U-E-DIFF (route B) below. Both backends are required in CI; locally a missing
    // one skips visibly.
    let native = null;
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
      native = null;
    }

    let wasm = null;
    try {
      const { initWasmNode, createWasmBackend } = await import('../dist/backend/wasm.js');
      const wasmModule = await initWasmNode();
      wasm = createWasmBackend(wasmModule);
    } catch {
      wasm = null;
    }

    if (native === null || wasm === null) {
      if (process.env.CI) {
        throw new Error('U-E-DIFF: both the native and the WASM backend are required in CI');
      }
      t.skip('native or WASM backend not built');
      return;
    }

    // One vector covering every escape class, carried in the module NAME: C0 (ESC),
    // C1 (NEL), bidi override (RLO), JS line separator, BOM. PF-007: a per-surface
    // golden cannot catch cross-surface divergence, so the whole class goes through
    // the differential.
    const hostile = [0x1b, 0x85, 0x202e, 0x2028, 0xfeff];
    const moduleName = `fo${String.fromCodePoint(...hostile)}o.mds`;
    const modules = {
      [moduleName]: 'hi\n',
      'main.mds': `@import "./${moduleName}"\n@import "./${moduleName}"\n`,
    };

    const nativeErr = thrownBy(() => native.lintVirtual(modules, 'main.mds'), 'U-E-DIFF native');
    const wasmErr = thrownBy(() => wasm.lintVirtual(modules, 'main.mds'), 'U-E-DIFF wasm');

    // Non-vacuity (PF-013): the native error is the forbidden-character refusal —
    // naming the FIRST forbidden codepoint and showing all five escaped — not some
    // unrelated failure the two backends happen to share.
    assert.deepEqual(errorShape(nativeErr), {
      code: 'mds::import',
      message:
        'import error: import path contains forbidden character U+001B: ' +
        `"./fo${hostile.map(escapeText).join('')}o.mds"`,
      help: null,
      span: null,
    });
    assertNoForbiddenChars(nativeErr.message, 'U-E-DIFF: native err.message');

    assert.deepEqual(
      errorShape(wasmErr),
      errorShape(nativeErr),
      'U-E-DIFF: native and WASM lintVirtual must throw identical errors for the same input',
    );
  });

  test('U-E-DIFF (route B): native and WASM lintVirtual produce identical lint results for a frontmatter-key control-char vector', async () => {
    // Route B sibling of U-E-DIFF: U-E-DIFF's vector carries ESC/NEL/RLO/LS/BOM in
    // the *module name* (route A, via duplicate-import) — #265 makes that vector
    // throw instead of returning a lint result, so U-E-DIFF asserts thrown-error
    // parity. This sibling carries the SAME escape class entirely in an UNUSED
    // FRONTMATTER KEY (route B, via unused-variable) instead, so cross-surface
    // lint-RESULT parity for the widened escape class survives the enforcement.
    //
    // Written as a YAML double-quoted key with YAML escapes so the .mds source text
    // itself carries no raw control byte (PF-018); serde_yaml_ng decodes each escape
    // into its real codepoint, which unused-variable embeds verbatim in its message
    // before WIRE-sanitization escapes it back out.
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

    // ESC, DEL, NEL, RLO, LS, BOM — all six via one YAML double-quoted key.
    const source =
      '---\n"a\\x1B\\x7F\\x85\\u202E\\u2028\\uFEFFpayload\\x1B\\x7F\\x85\\u202E\\u2028\\uFEFFb": 1\n' +
      '---\nHello\n';
    for (const raw of [0x1b, 0x7f, 0x85, 0x202e, 0x2028, 0xfeff]) {
      assert.ok(
        !source.includes(String.fromCharCode(raw)),
        `U-E-DIFF (route B): source must carry no raw U+${raw.toString(16).toUpperCase()} byte, only the YAML escape`,
      );
    }
    const modules = { 'main.mds': source };

    const nativeResult = native.lintVirtual(modules, 'main.mds');
    const wasmResult = wasm.lintVirtual(modules, 'main.mds');

    // Non-vacuity (PF-013): the differential is worthless if unused-variable never
    // fired or never carried the escaped forms.
    const nativeMessages = nativeResult.files
      .flatMap((f) => f.diagnostics)
      .map((d) => d.message)
      .filter((m) => typeof m === 'string');
    assert.ok(
      nativeResult.files.some((f) => f.diagnostics.some((d) => d.rule === 'unused-variable')),
      'U-E-DIFF (route B): expected unused-variable to fire; got: ' +
        JSON.stringify(nativeResult.files),
    );
    for (const escaped of ['\\u001B', '\\u007F', '\\u0085', '\\u202E', '\\u2028', '\\uFEFF']) {
      assert.ok(
        nativeMessages.some((m) => m.includes(escaped)),
        `U-E-DIFF (route B): expected ${escaped} in some message; got: ` +
          JSON.stringify(nativeMessages),
      );
    }
    assert.ok(
      nativeMessages.some((m) => m.includes('payload')),
      'U-E-DIFF (route B): message body must be preserved verbatim; got: ' +
        JSON.stringify(nativeMessages),
    );

    // deepEqual of plain-object round-trip proves wire-format parity for the
    // route-B-only vector — the property that must survive #265 enforcement.
    assert.deepEqual(
      JSON.parse(JSON.stringify(nativeResult)),
      JSON.parse(JSON.stringify(wasmResult)),
      'U-E-DIFF (route B): native and WASM lintVirtual must produce identical results',
    );
  });
});
