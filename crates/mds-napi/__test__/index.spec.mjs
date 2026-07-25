/**
 * Integration tests for the mds-napi native Node.js addon.
 *
 * Run with: node --test __test__/index.spec.mjs
 * (requires Node.js 22+ for node:test runner)
 */

import { createRequire } from 'node:module';
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import os from 'node:os';
import fs from 'node:fs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);

// Load the native addon from the built .node file.
const addon = require('../mds-napi.node');
const { compile, compileFile, check, checkFile } = addon;

// Fixture directory.
const FIXTURES = path.join(__dirname, 'fixtures');
const SIMPLE_MDS = path.join(FIXTURES, 'simple.mds');
const VAR_MDS = path.join(FIXTURES, 'var.mds');
const IMPORT_PROVIDER_MDS = path.join(FIXTURES, 'import_provider.mds');
const IMPORT_CONSUMER_MDS = path.join(FIXTURES, 'import_consumer.mds');
const MESSAGES_MDS = path.join(FIXTURES, 'messages.mds');
const MIXED_MDS = path.join(FIXTURES, 'mixed.mds');

// ── Compile tests ─────────────────────────────────────────────────────────────

describe('compile', () => {
  test('F-C1: basic compile, no options', () => {
    const result = compile('Hello World!\n');
    assert.equal(result.kind, 'markdown');
    assert.equal(result.output, 'Hello World!\n');
    assert.ok(Array.isArray(result.warnings));
    assert.ok(Array.isArray(result.dependencies));
  });

  test('F-C2: compile with null options', () => {
    const result = compile('Hello World!\n', null);
    assert.equal(result.kind, 'markdown');
    assert.equal(result.output, 'Hello World!\n');
  });

  test('F-C3: compile with undefined options', () => {
    const result = compile('Hello World!\n', undefined);
    assert.equal(result.kind, 'markdown');
    assert.equal(result.output, 'Hello World!\n');
  });

  test('F-C4: compile with empty options object', () => {
    const result = compile('Hello World!\n', {});
    assert.equal(result.kind, 'markdown');
    assert.equal(result.output, 'Hello World!\n');
  });

  test('F-C5: compile with frontmatter vars', () => {
    const source = '---\nname: Alice\n---\nHello {{name}}!\n';
    const result = compile(source);
    assert.equal(result.kind, 'markdown');
    assert.ok(result.output.includes('Hello Alice!'), `expected "Hello Alice!" in: ${result.output}`);
  });

  test('F-C6: compile with runtime vars', () => {
    const source = 'Hello {{name}}!\n';
    const result = compile(source, { vars: { name: 'Bob' } });
    assert.equal(result.kind, 'markdown');
    assert.equal(result.output, 'Hello Bob!\n');
  });

  test('F-C7: runtime vars override frontmatter', () => {
    const source = '---\nname: Alice\n---\nHello {{name}}!\n';
    const result = compile(source, { vars: { name: 'Override' } });
    assert.equal(result.kind, 'markdown');
    assert.ok(result.output.includes('Hello Override!'), `got: ${result.output}`);
  });

  test('F-C8: compile with basePath for import resolution', () => {
    const source = `@import { greet } from "./import_provider.mds"\n\n{{greet("Test")}}\n`;
    const result = compile(source, { basePath: FIXTURES });
    assert.equal(result.kind, 'markdown');
    assert.ok(result.output.includes('Hello Test!'), `got: ${result.output}`);
  });

  test('F-C9: empty source compiles successfully', () => {
    const result = compile('');
    assert.equal(result.kind, 'markdown');
    assert.equal(result.output, '');
    assert.deepEqual(result.warnings, []);
    assert.deepEqual(result.dependencies, []);
  });
});

// ── CompileFile tests ─────────────────────────────────────────────────────────

describe('compileFile', () => {
  test('F-CF1: compile file', () => {
    const result = compileFile(SIMPLE_MDS);
    assert.equal(result.kind, 'markdown');
    assert.ok(result.output.includes('Hello Alice!'), `got: ${result.output}`);
    assert.ok(result.output.includes('3 items'), `got: ${result.output}`);
  });

  test('F-CF2: compile file with vars', () => {
    const result = compileFile(VAR_MDS, { vars: { name: 'World' } });
    assert.equal(result.kind, 'markdown');
    assert.equal(result.output, 'Hello World!\n');
  });

  test('F-CF3: compile file with imports', () => {
    const result = compileFile(IMPORT_CONSUMER_MDS);
    assert.equal(result.kind, 'markdown');
    assert.ok(result.output.includes('Hello World!'), `got: ${result.output}`);
  });

  test('F-CF4: dependencies are absolute paths', () => {
    const result = compileFile(IMPORT_CONSUMER_MDS);
    assert.ok(result.dependencies.length > 0, 'should have dependencies');
    for (const dep of result.dependencies) {
      assert.ok(path.isAbsolute(dep), `dependency should be absolute: ${dep}`);
    }
  });

  test('F-CF5: nonexistent file throws with code mds::file_not_found', () => {
    assert.throws(
      () => compileFile('/nonexistent/path/file.mds'),
      (err) => {
        assert.ok(err instanceof Error, 'should be an Error');
        assert.equal(err.code, 'mds::file_not_found', `got code: ${err.code}`);
        return true;
      },
    );
  });

  test('F-CF6: relative path resolves from cwd', () => {
    // Derive a relative path from the known absolute fixtures directory so this
    // test is deterministic regardless of which directory the test runner uses.
    const cwd = process.cwd();
    const relativePath = path.relative(cwd, SIMPLE_MDS);
    // If the relative path escapes cwd with "../" segments, the fixture is
    // not reachable as a relative path — assert deterministically instead.
    if (relativePath.startsWith('..')) {
      // The fixture is not reachable as a relative path from this cwd.  The
      // file_not_found error must be thrown (not silently swallowed).
      assert.throws(
        () => compileFile(relativePath),
        (err) => {
          assert.ok(
            err.code === 'mds::file_not_found' || err.code === 'mds::io',
            `expected file_not_found or io, got: ${err.code}`,
          );
          return true;
        },
      );
    } else {
      const result = compileFile(relativePath);
      assert.equal(result.kind, 'markdown');
      assert.ok(result.output.includes('Hello Alice!'), `got: ${result.output}`);
    }
  });

  test('F-CF7: bare filename (no separator, no ./ prefix) resolves from cwd (avoids PF-006)', () => {
    // PF-006: Path::parent() returns Some("") for a bare name like "simple.mds".
    // "".canonicalize() fails with file_not_found unless effective_parent maps it
    // to ".".  This test exercises exactly that path — a bare filename with NO
    // separator character and NO "./" prefix — by chdir-ing into the fixtures
    // directory so the bare name is resolvable.
    // F-CF6 uses path.relative() which always produces a SEPARATOR-CONTAINING
    // relative path (e.g. "../../fixtures/simple.mds"), bypassing the bug entirely.
    // This test is the genuine bare-filename gate.
    const originalCwd = process.cwd();
    try {
      process.chdir(FIXTURES);
      const result = compileFile('simple.mds'); // no separator, no './' prefix
      assert.equal(result.kind, 'markdown', 'F-CF7: kind must be markdown');
      assert.ok(
        result.output.includes('Hello Alice!'),
        `F-CF7: bare-filename compileFile must produce compiled content; got: ${result.output}`,
      );
      // Dependencies must be absolute paths even when the entry was a bare filename.
      for (const dep of result.dependencies) {
        assert.ok(
          path.isAbsolute(dep),
          `F-CF7: dependency must be absolute; got: ${dep}`,
        );
      }
    } finally {
      process.chdir(originalCwd);
    }
  });
});

// ── Check tests ───────────────────────────────────────────────────────────────

describe('check', () => {
  test('F-K1: check valid source returns warnings array', () => {
    const result = check('Hello World!\n');
    assert.ok(Array.isArray(result.warnings));
  });

  test('F-K2: check with null options', () => {
    const result = check('Hello World!\n', null);
    assert.ok(Array.isArray(result.warnings));
  });

  test('F-K3: check with undefined options', () => {
    const result = check('Hello World!\n', undefined);
    assert.ok(Array.isArray(result.warnings));
  });

  test('F-K4: check with frontmatter vars is valid', () => {
    const source = '---\nname: Test\n---\nHello {{name}}!\n';
    const result = check(source);
    assert.deepEqual(result.warnings, []);
  });

  test('F-K5: check with runtime vars', () => {
    const source = 'Hello {{name}}!\n';
    const result = check(source, { vars: { name: 'Test' } });
    assert.deepEqual(result.warnings, []);
  });

  test('F-K6: check undefined variable throws', () => {
    assert.throws(
      () => check('Hello {{undefined_var}}!\n'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.equal(err.code, 'mds::undefined_var', `got: ${err.code}`);
        return true;
      },
    );
  });

  test('F-K7: check with basePath', () => {
    const source = `@import { greet } from "./import_provider.mds"\n\n{{greet("Test")}}\n`;
    const result = check(source, { basePath: FIXTURES });
    assert.ok(Array.isArray(result.warnings));
  });

  test('F-K8: checkFile valid file', () => {
    const result = checkFile(SIMPLE_MDS);
    assert.ok(Array.isArray(result.warnings));
    assert.deepEqual(result.warnings, []);
  });

  test('F-K9: checkFile nonexistent throws', () => {
    assert.throws(
      () => checkFile('/nonexistent/path/file.mds'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.equal(err.code, 'mds::file_not_found', `got: ${err.code}`);
        return true;
      },
    );
  });

  test('F-K10: checkFile with vars', () => {
    const result = checkFile(VAR_MDS, { vars: { name: 'World' } });
    assert.deepEqual(result.warnings, []);
  });

  test('F-K11: check result has only warnings property (no output or dependencies)', () => {
    const result = check('Hello World!\n');
    assert.equal(result.output, undefined, 'check result must not have output');
    assert.equal(result.dependencies, undefined, 'check result must not have dependencies');
  });
});

// ── Error shape tests ─────────────────────────────────────────────────────────

describe('error shape', () => {
  test('E-1: error is instanceof Error', () => {
    assert.throws(
      () => compile('Hello {{undefined_var}}!\n'),
      (err) => {
        assert.ok(err instanceof Error, 'should be instanceof Error');
        return true;
      },
    );
  });

  test('E-2: error has code property', () => {
    assert.throws(
      () => compile('Hello {{undefined_var}}!\n'),
      (err) => {
        assert.ok('code' in err, 'should have code property');
        assert.ok(typeof err.code === 'string', `code should be string, got ${typeof err.code}`);
        return true;
      },
    );
  });

  test('E-3: error has message property', () => {
    assert.throws(
      () => compile('Hello {{undefined_var}}!\n'),
      (err) => {
        assert.ok(typeof err.message === 'string', 'should have message');
        assert.ok(err.message.length > 0, 'message should not be empty');
        return true;
      },
    );
  });

  test('E-4: undefined var error has correct code', () => {
    assert.throws(
      () => compile('Hello {{undefined_var}}!\n'),
      (err) => {
        assert.equal(err.code, 'mds::undefined_var', `got: ${err.code}`);
        return true;
      },
    );
  });

  test('E-5: undefined var error has help property', () => {
    assert.throws(
      () => compile('Hello {{undefined_var}}!\n'),
      (err) => {
        // undefined_var always carries a help message from the diagnostic annotation.
        assert.ok('help' in err, 'undefined_var errors should include a help property');
        assert.ok(typeof err.help === 'string', `help should be string, got: ${typeof err.help}`);
        assert.ok(err.help.length > 0, 'help should not be empty');
        return true;
      },
    );
  });

  test('E-6: syntax error has code mds::syntax', () => {
    assert.throws(
      () => compile('@import\n'),
      (err) => {
        assert.equal(err.code, 'mds::syntax', `got: ${err.code}`);
        return true;
      },
    );
  });

  test('E-7: file not found has code mds::file_not_found', () => {
    assert.throws(
      () => compileFile('/no/such/file.mds'),
      (err) => {
        assert.equal(err.code, 'mds::file_not_found', `got: ${err.code}`);
        return true;
      },
    );
  });

  test('E-8: span is object when present', () => {
    assert.throws(
      () => compile('Hello {{undefined_var}}!\n'),
      (err) => {
        // undefined_var errors produced via compile() go through the validator which
        // always attaches a source span (offset + length of the variable name).
        assert.ok(err.span !== undefined, 'undefined_var errors should have a span');
        assert.ok(typeof err.span === 'object' && err.span !== null, 'span should be object');
        assert.ok(typeof err.span.offset === 'number', 'span.offset should be number');
        assert.ok(typeof err.span.length === 'number', 'span.length should be number');
        return true;
      },
    );
  });

  test('E-9: options error has code mds::invalid_options', () => {
    assert.throws(
      () => compile('Hello!\n', { unknownKey: true }),
      (err) => {
        assert.equal(err.code, 'mds::invalid_options', `got: ${err.code}`);
        return true;
      },
    );
  });

  // D2: type_mismatch_at — cross-type @if comparison now carries a source span
  test('D2: type_mismatch from @if cross-type comparison carries a non-null span', () => {
    const src = '---\nx: 3\n---\n@if x == "3":\nyes\n@end\n';
    assert.throws(
      () => compile(src),
      (err) => {
        assert.equal(err.code, 'mds::type_mismatch', `D2: expected type_mismatch, got: ${err.code}`);
        assert.ok(err.span !== undefined && err.span !== null, 'D2: type_mismatch must carry a span');
        assert.ok(typeof err.span.offset === 'number', 'D2: span.offset must be a number');
        assert.ok(err.span.length > 0, 'D2: span.length must be > 0');
        assert.ok(typeof err.span.line === 'number', `D2: span.line must be a number, got: ${typeof err.span.line}`);
        assert.ok(typeof err.span.column === 'number', `D2: span.column must be a number, got: ${typeof err.span.column}`);
        return true;
      },
    );
  });
});

// ── Options validation tests ──────────────────────────────────────────────────

describe('options validation', () => {
  test('V-1: unknown option key throws mds::invalid_options', () => {
    assert.throws(
      () => compile('Hello!\n', { unknownOption: 'value' }),
      (err) => {
        assert.equal(err.code, 'mds::invalid_options', `got: ${err.code}`);
        return true;
      },
    );
  });

  test('V-2: vars as string throws mds::invalid_options', () => {
    assert.throws(
      () => compile('Hello!\n', { vars: 'not-an-object' }),
      (err) => {
        assert.equal(err.code, 'mds::invalid_options', `got: ${err.code}`);
        return true;
      },
    );
  });

  test('V-3: empty basePath throws mds::invalid_options', () => {
    assert.throws(
      () => compile('Hello!\n', { basePath: '' }),
      (err) => {
        assert.equal(err.code, 'mds::invalid_options', `got: ${err.code}`);
        return true;
      },
    );
  });

  test('V-4: basePath on compileFile throws mds::invalid_options', () => {
    assert.throws(
      () => compileFile(SIMPLE_MDS, { basePath: '/some/path' }),
      (err) => {
        assert.equal(err.code, 'mds::invalid_options', `got: ${err.code}`);
        return true;
      },
    );
  });

  test('V-5: basePath on checkFile throws mds::invalid_options', () => {
    assert.throws(
      () => checkFile(SIMPLE_MDS, { basePath: '/some/path' }),
      (err) => {
        assert.equal(err.code, 'mds::invalid_options', `got: ${err.code}`);
        return true;
      },
    );
  });

  test('V-6: unknown key on compileFile throws mds::invalid_options', () => {
    assert.throws(
      () => compileFile(SIMPLE_MDS, { unknownKey: true }),
      (err) => {
        assert.equal(err.code, 'mds::invalid_options', `got: ${err.code}`);
        return true;
      },
    );
  });

  test('V-7: basePath as number throws mds::invalid_options', () => {
    assert.throws(
      () => compile('Hello!\n', { basePath: 42 }),
      (err) => {
        assert.equal(err.code, 'mds::invalid_options', `got: ${err.code}`);
        return true;
      },
    );
  });

  test('V-8: vars as array throws mds::invalid_options', () => {
    assert.throws(
      () => compile('Hello!\n', { vars: ['not', 'an', 'object'] }),
      (err) => {
        assert.equal(err.code, 'mds::invalid_options', `got: ${err.code}`);
        assert.ok(err.message.includes('array'), `expected "array" in message: ${err.message}`);
        return true;
      },
    );
  });

  test('V-9: unknown key on check throws mds::invalid_options', () => {
    assert.throws(
      () => check('Hello!\n', { unknownKey: true }),
      (err) => {
        assert.equal(err.code, 'mds::invalid_options', `got: ${err.code}`);
        return true;
      },
    );
  });

  test('V-10: unknown key on checkFile throws mds::invalid_options', () => {
    assert.throws(
      () => checkFile(SIMPLE_MDS, { unknownKey: true }),
      (err) => {
        assert.equal(err.code, 'mds::invalid_options', `got: ${err.code}`);
        return true;
      },
    );
  });

  // V-11: source-map options are not accepted on the check/checkFile path (ARCH-5).
  // check() does not generate maps; passing sourceMap/sourcesContent is rejected as
  // an unknown key (aligns with Python's check() which has no such parameters).
  test('V-11: sourceMap on check throws mds::invalid_options (not a valid check option)', () => {
    assert.throws(
      () => check('Hello!\n', { sourceMap: true }),
      (err) => {
        assert.equal(err.code, 'mds::invalid_options', `got: ${err.code}`);
        return true;
      },
    );
    assert.throws(
      () => check('Hello!\n', { sourcesContent: true }),
      (err) => {
        assert.equal(err.code, 'mds::invalid_options', `got: ${err.code}`);
        return true;
      },
    );
    assert.throws(
      () => checkFile(SIMPLE_MDS, { sourceMap: true }),
      (err) => {
        assert.equal(err.code, 'mds::invalid_options', `got: ${err.code}`);
        return true;
      },
    );
  });
});

// ── Resource limit tests ──────────────────────────────────────────────────────

describe('resource limits', () => {
  const MAX_SOURCE_SIZE = 10 * 1024 * 1024; // 10 MiB

  test('R-1: oversized source throws mds::resource_limit', () => {
    const oversized = 'x'.repeat(MAX_SOURCE_SIZE + 1);
    assert.throws(
      () => compile(oversized),
      (err) => {
        assert.equal(err.code, 'mds::resource_limit', `got: ${err.code}`);
        return true;
      },
    );
  });

  test('R-2: oversized source throws mds::resource_limit for check', () => {
    const oversized = 'x'.repeat(MAX_SOURCE_SIZE + 1);
    assert.throws(
      () => check(oversized),
      (err) => {
        assert.equal(err.code, 'mds::resource_limit', `got: ${err.code}`);
        return true;
      },
    );
  });

  test('R-3: source at exactly max size is accepted (not rejected by size guard)', () => {
    // Exactly at the limit must not trigger the resource_limit guard — only
    // sources strictly larger than MAX_SOURCE_SIZE are rejected.
    const atLimit = ' '.repeat(MAX_SOURCE_SIZE);
    try {
      const result = compile(atLimit);
      assert.ok(typeof result.output === 'string', 'output should be a string');
    } catch (e) {
      assert.notEqual(e.code, 'mds::resource_limit', 'size guard must not fire at exactly the limit');
    }
  });
});

// ── Compilation parity tests ──────────────────────────────────────────────────

describe('compilation parity', () => {
  test('P-1: simple.mds compileFile output matches expected', () => {
    const result = compileFile(SIMPLE_MDS);
    assert.equal(result.kind, 'markdown');
    // The simple.mds has frontmatter with name: Alice, count: 3.
    assert.ok(result.output.includes('Hello Alice!'), `expected "Hello Alice!" in: ${result.output}`);
    assert.ok(result.output.includes('3 items'), `expected "3 items" in: ${result.output}`);
  });

  test('P-2: compile and compileFile agree on same source + basePath', () => {
    const source = '---\nname: Alice\ncount: 3\n---\n\nHello {{name}}! You have {{count}} items.\n';
    const compileResult = compile(source, { basePath: FIXTURES });
    const fileResult = compileFile(SIMPLE_MDS);
    assert.equal(compileResult.kind, 'markdown');
    assert.equal(fileResult.kind, 'markdown');
    assert.equal(compileResult.output, fileResult.output);
  });
});

// ── Template inheritance tests (@extends / @block) ────────────────────────────

describe('template inheritance', () => {
  // Helper: create a temp directory with base.mds + child.mds; compile child via compileFile.
  function withInheritanceFixtures(baseContent, childContent, fn) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'mds-inh-'));
    try {
      fs.writeFileSync(path.join(dir, 'base.mds'), baseContent);
      const childPath = path.join(dir, 'child.mds');
      fs.writeFileSync(childPath, childContent);
      return fn(childPath, dir);
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  }

  // INH-1: compileFile round-trip — skeleton + override, base in dependencies
  test('INH-1: compileFile inherits base skeleton and applies child overrides', () => {
    const baseContent =
      '---\nrole: general\n---\nYou are a {{role}} assistant.\n@block body:\nDefault body.\n@end\n';
    const childContent =
      '---\nrole: specialist\n---\n@extends "./base.mds"\n@block body:\nOverridden body.\n@end\n';

    withInheritanceFixtures(baseContent, childContent, (childPath, dir) => {
      const result = compileFile(childPath);

      assert.equal(result.kind, 'markdown', 'INH-1: kind must be "markdown"');

      // Output must contain the overridden block and the base skeleton text.
      assert.ok(
        result.output.includes('You are a specialist assistant.'),
        `INH-1: base skeleton text with child role should render; got: ${result.output}`,
      );
      assert.ok(
        result.output.includes('Overridden body.'),
        `INH-1: overridden block body should render; got: ${result.output}`,
      );
      assert.ok(
        !result.output.includes('Default body.'),
        `INH-1: base default body must not appear when overridden; got: ${result.output}`,
      );

      // Base must appear in the dependencies list.
      assert.ok(Array.isArray(result.dependencies), 'INH-1: dependencies must be an array');
      assert.ok(result.dependencies.length > 0, 'INH-1: dependencies must be non-empty');
      const hasBase = result.dependencies.some((d) => d.includes('base.mds'));
      assert.ok(hasBase, `INH-1: base.mds must be in dependencies; got: ${JSON.stringify(result.dependencies)}`);

      assert.ok(typeof result.output === 'string', 'INH-1: output must be a string');
      assert.ok(Array.isArray(result.warnings), 'INH-1: warnings must be an array');
      // messages field must be absent for markdown results (AC-API-13).
      assert.equal(result.messages, undefined, 'INH-1: messages field must be absent for markdown result');
    });
  });

  // INH-2: error code for extends error is mds::extends
  test('INH-2: stray @extends produces mds::extends error code', () => {
    assert.throws(
      () => compile('Some text.\n@extends "./base.mds"\n'),
      (err) => {
        assert.ok(err instanceof Error, 'should be an Error');
        assert.equal(err.code, 'mds::extends', `expected mds::extends, got: ${err.code}`);
        return true;
      },
    );
  });

  // INH-3: C4 regression — undefined var in base default block carries a real span
  // (line/column) rather than undefined when the child does not override the block.
  test('INH-3: undefined var in inherited base default block has code mds::undefined_var and a real span', () => {
    const baseContent = '@block greeting:\nHello {{customer_name}}, welcome.\n@end\n';
    const childContent = '@extends "./base.mds"\n';

    withInheritanceFixtures(baseContent, childContent, (childPath) => {
      assert.throws(
        () => compileFile(childPath),
        (err) => {
          assert.ok(err instanceof Error, 'INH-3: error should be instanceof Error');
          assert.equal(
            err.code,
            'mds::undefined_var',
            `INH-3: expected mds::undefined_var, got: ${err.code}`,
          );
          assert.ok(
            err.span !== undefined && err.span !== null,
            'INH-3: err.span must be present for inherited undefined_var',
          );
          assert.equal(
            typeof err.span.line,
            'number',
            `INH-3: span.line must be a number, got: ${typeof err.span.line}`,
          );
          assert.equal(
            typeof err.span.column,
            'number',
            `INH-3: span.column must be a number, got: ${typeof err.span.column}`,
          );
          return true;
        },
      );
    });
  });
});

// ── Intrinsic output shape tests (AC-API-04, AC-API-05, AC-API-13) ────────────
//
// These tests verify the canonical { kind, <active-payload>, warnings, dependencies }
// discriminated-union object that compile/compileFile now return.

describe('intrinsic output shape', () => {
  // K-MD-1: markdown fixture → kind:"markdown", output present, messages absent
  test('K-MD-1: compile on markdown source returns kind:"markdown" with output, no messages', () => {
    const result = compile('Hello World!\n');
    assert.equal(result.kind, 'markdown', 'kind must be "markdown"');
    assert.ok(typeof result.output === 'string', 'output must be a string');
    assert.equal(result.messages, undefined, 'messages key must be absent');
    assert.ok(Array.isArray(result.warnings), 'warnings must be an array');
    assert.ok(Array.isArray(result.dependencies), 'dependencies must be an array');
  });

  // K-MD-2: markdown fixture file → kind:"markdown"
  test('K-MD-2: compileFile on markdown fixture returns kind:"markdown"', () => {
    const result = compileFile(SIMPLE_MDS);
    assert.equal(result.kind, 'markdown');
    assert.ok(typeof result.output === 'string');
    assert.equal(result.messages, undefined, 'messages key must be absent');
  });

  // K-MSG-1: @message fixture string → kind:"messages", messages present, output absent
  test('K-MSG-1: compile on @message source returns kind:"messages" with messages, no output', () => {
    const source = '@message system:\nYou are helpful.\n@end\n@message user:\nHello!\n@end\n';
    const result = compile(source);
    assert.equal(result.kind, 'messages', 'kind must be "messages"');
    assert.ok(Array.isArray(result.messages), 'messages must be an array');
    assert.equal(result.output, undefined, 'output key must be absent');
    assert.ok(Array.isArray(result.warnings), 'warnings must be an array');
    assert.ok(Array.isArray(result.dependencies), 'dependencies must be an array');
    assert.equal(result.messages.length, 2);
    assert.equal(result.messages[0].role, 'system');
    assert.ok(typeof result.messages[0].content === 'string', 'content must be string');
    assert.equal(result.messages[1].role, 'user');
  });

  // K-MSG-2: @message fixture file → kind:"messages"
  test('K-MSG-2: compileFile on @message fixture returns kind:"messages"', () => {
    const result = compileFile(MESSAGES_MDS);
    assert.equal(result.kind, 'messages');
    assert.ok(Array.isArray(result.messages));
    assert.equal(result.output, undefined, 'output key must be absent');
    assert.equal(result.messages.length, 2);
    assert.equal(result.messages[0].role, 'system');
    assert.equal(result.messages[1].role, 'user');
  });

  // K-MSG-3: message objects have role and content, nothing else injected by binding
  test('K-MSG-3: each message object has role and content as strings', () => {
    const source = '@message assistant:\nHello there!\n@end\n';
    const result = compile(source);
    assert.equal(result.kind, 'messages');
    assert.equal(result.messages.length, 1);
    const msg = result.messages[0];
    assert.equal(typeof msg.role, 'string');
    assert.equal(typeof msg.content, 'string');
    assert.equal(msg.role, 'assistant');
    assert.ok(msg.content.includes('Hello there'), `got: ${msg.content}`);
  });

  // K-MIXED-1: mixed content (prose + @message) → mds::mixed_content from compile
  test('K-MIXED-1: compile on mixed-content fixture throws mds::mixed_content', () => {
    assert.throws(
      () => compileFile(MIXED_MDS),
      (err) => {
        assert.ok(err instanceof Error, 'should be an Error');
        assert.equal(err.code, 'mds::mixed_content', `got: ${err.code}`);
        return true;
      },
    );
  });

  // K-MIXED-2: compile (string) on mixed content → mds::mixed_content
  test('K-MIXED-2: compile (string) on mixed content throws mds::mixed_content', () => {
    const source = 'Some prose text.\n\n@message user:\nA message.\n@end\n';
    assert.throws(
      () => compile(source),
      (err) => {
        assert.ok(err instanceof Error, 'should be an Error');
        assert.equal(err.code, 'mds::mixed_content', `got: ${err.code}`);
        return true;
      },
    );
  });

  // K-MIXED-3: check on mixed content → mds::mixed_content
  test('K-MIXED-3: check on mixed-content fixture throws mds::mixed_content', () => {
    assert.throws(
      () => checkFile(MIXED_MDS),
      (err) => {
        assert.ok(err instanceof Error, 'should be an Error');
        assert.equal(err.code, 'mds::mixed_content', `got: ${err.code}`);
        return true;
      },
    );
  });

  // AC-API-05 (negative): compileMessages and compileMessagesFile must not exist
  test('AC-API-05: compileMessages is absent from addon exports', () => {
    assert.equal(
      typeof addon.compileMessages,
      'undefined',
      'addon.compileMessages must not be exported',
    );
  });

  test('AC-API-05: compileMessagesFile is absent from addon exports', () => {
    assert.equal(
      typeof addon.compileMessagesFile,
      'undefined',
      'addon.compileMessagesFile must not be exported',
    );
  });

  // K-VARS-1: vars round-trip through @message content survives FFI
  test('K-VARS-1: vars with special chars survive the FFI round-trip in messages mode', () => {
    const source = '@message user:\n{{data}}\n@end\n';
    const specialValue = 'say "hello"\\ here\nnewline\u{1F600}';
    const result = compile(source, { vars: { data: specialValue } });
    assert.equal(result.kind, 'messages');
    assert.equal(result.messages.length, 1);
    assert.equal(
      result.messages[0].content,
      specialValue,
      `content must be byte-identical after FFI round-trip; got: ${JSON.stringify(result.messages[0].content)}`,
    );
  });

  // K-VARS-2: empty dynamic role via FFI throws (evaluator rejects it)
  test('K-VARS-2: empty dynamic role via FFI throws mds:: error', () => {
    const source = '@message {{role}}:\nHello!\n@end\n';
    assert.throws(
      () => compile(source, { vars: { role: '' } }),
      (err) => {
        assert.ok(err instanceof Error, 'should be Error');
        assert.ok(err.code.startsWith('mds::'), `error code must start with mds::; got: ${err.code}`);
        return true;
      },
    );
  });
});

// ── Lint tests ────────────────────────────────────────────────────────────────

const { lint, lintFile, lintVirtual } = addon;

describe('lint', () => {
  // L-N-1: clean source returns canonical shape
  test('L-N-1: lint on clean source returns version:1, empty files, truncated:false', () => {
    const result = lint('Hello World!\n');
    assert.equal(result.version, 1, 'version must be 1');
    assert.ok(Array.isArray(result.files), 'files must be an array');
    assert.equal(result.files.length, 0, 'clean source should have no file entries');
    assert.equal(result.truncated, false, 'truncated must be false');
  });

  // L-N-2: null/undefined options accepted
  test('L-N-2: lint accepts null options', () => {
    const result = lint('Hello World!\n', null);
    assert.equal(result.version, 1);
  });

  test('L-N-3: lint accepts undefined options', () => {
    const result = lint('Hello World!\n', undefined);
    assert.equal(result.version, 1);
  });

  // L-N-4: unused-variable finding on frontmatter key
  test('L-N-4: lint detects unused frontmatter variable', () => {
    const source = '---\nunused_key: value\n---\nHello!\n';
    const result = lint(source);
    assert.equal(result.version, 1);
    // There should be at least one file entry with a diagnostic for unused-variable.
    const allDiags = result.files.flatMap((f) => f.diagnostics);
    const hasUnused = allDiags.some((d) => d.rule === 'unused-variable');
    assert.ok(hasUnused, `expected unused-variable diagnostic; got: ${JSON.stringify(allDiags)}`);
  });

  // L-N-5: rules option silences a rule
  test('L-N-5: rules option can silence a rule', () => {
    const source = '---\nunused_key: value\n---\nHello!\n';
    const result = lint(source, { rules: { 'unused-variable': 'off' } });
    const allDiags = result.files.flatMap((f) => f.diagnostics);
    const hasUnused = allDiags.some((d) => d.rule === 'unused-variable');
    assert.ok(!hasUnused, `unused-variable should be silenced; got: ${JSON.stringify(allDiags)}`);
  });

  // L-N-6: unknown severity value in rules throws mds::invalid_options
  test('L-N-6: unknown severity value in rules throws mds::invalid_options', () => {
    assert.throws(
      () => lint('Hello!\n', { rules: { 'unused-variable': 'verbose' } }),
      (err) => {
        assert.equal(err.code, 'mds::invalid_options', `got: ${err.code}`);
        return true;
      },
    );
  });

  // L-N-7: unknown option key throws mds::invalid_options
  test('L-N-7: unknown option key throws mds::invalid_options', () => {
    assert.throws(
      () => lint('Hello!\n', { unknownKey: true }),
      (err) => {
        assert.equal(err.code, 'mds::invalid_options', `got: ${err.code}`);
        return true;
      },
    );
  });

  // L-N-8: parse error throws (check gate enforced)
  test('L-N-8: lint on invalid source throws with mds:: error code', () => {
    assert.throws(
      () => lint('Hello {{undefined_var}}!\n'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(err.code.startsWith('mds::'), `expected mds:: code, got: ${err.code}`);
        return true;
      },
    );
  });
});

describe('lintFile', () => {
  // L-NF-1: lint a valid file
  test('L-NF-1: lintFile on clean file returns canonical shape', () => {
    const result = lintFile(SIMPLE_MDS);
    assert.equal(result.version, 1);
    assert.ok(Array.isArray(result.files));
    assert.equal(result.truncated, false);
  });

  // L-NF-2: nonexistent file throws mds::file_not_found
  test('L-NF-2: lintFile on nonexistent file throws mds::file_not_found', () => {
    assert.throws(
      () => lintFile('/nonexistent/path/file.mds'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.equal(err.code, 'mds::file_not_found', `got: ${err.code}`);
        return true;
      },
    );
  });

  // L-NF-3: basePath option rejected
  test('L-NF-3: lintFile rejects basePath option', () => {
    assert.throws(
      () => lintFile(SIMPLE_MDS, { basePath: '/some/path' }),
      (err) => {
        assert.equal(err.code, 'mds::invalid_options', `got: ${err.code}`);
        return true;
      },
    );
  });

  // L-NF-4: rules option accepted
  test('L-NF-4: lintFile accepts rules option', () => {
    const result = lintFile(SIMPLE_MDS, { rules: { 'unused-variable': 'off' } });
    assert.equal(result.version, 1);
  });
});

describe('lintVirtual', () => {
  // L-NV-1: clean virtual module
  test('L-NV-1: lintVirtual on clean module returns canonical shape', () => {
    const modules = { 'main.mds': 'Hello World!\n' };
    const result = lintVirtual(modules, 'main.mds');
    assert.equal(result.version, 1);
    assert.ok(Array.isArray(result.files));
    assert.equal(result.truncated, false);
  });

  // L-NV-2: finding in virtual module
  test('L-NV-2: lintVirtual detects lint findings', () => {
    const modules = { 'main.mds': '---\nunused_key: value\n---\nHello!\n' };
    const result = lintVirtual(modules, 'main.mds');
    assert.equal(result.version, 1);
    const allDiags = result.files.flatMap((f) => f.diagnostics);
    const hasUnused = allDiags.some((d) => d.rule === 'unused-variable');
    assert.ok(hasUnused, `expected unused-variable; got: ${JSON.stringify(allDiags)}`);
  });

  // L-NV-3: rules option accepted
  test('L-NV-3: lintVirtual accepts rules option', () => {
    const modules = { 'main.mds': '---\nunused_key: value\n---\nHello!\n' };
    const result = lintVirtual(modules, 'main.mds', { rules: { 'unused-variable': 'off' } });
    const allDiags = result.files.flatMap((f) => f.diagnostics);
    const hasUnused = allDiags.some((d) => d.rule === 'unused-variable');
    assert.ok(!hasUnused, 'unused-variable should be silenced');
  });

  // L-NV-4: basePath option rejected with correct function name
  test('L-NV-4: lintVirtual rejects basePath option', () => {
    const modules = { 'main.mds': 'Hello World!\n' };
    assert.throws(
      () => lintVirtual(modules, 'main.mds', { basePath: '/some/path' }),
      (err) => {
        assert.equal(err.code, 'mds::invalid_options', `got: ${err.code}`);
        assert.ok(
          err.message.includes('lintVirtual'),
          `expected message to name lintVirtual; got: ${err.message}`,
        );
        assert.ok(
          !err.message.includes('lintFile'),
          `expected message NOT to name lintFile; got: ${err.message}`,
        );
        return true;
      },
    );
  });
});

// ── Lint parity guard: lint + lintFile produce canonical JSON ─────────────────

describe('lint parity (AC-API-06 guard)', () => {
  // P-L-1: lint and lintFile produce identical JSON for same source
  test('P-L-1: lint and lintFile produce identical JSON for the same file', () => {
    const fileResult = lintFile(SIMPLE_MDS);
    const source = fs.readFileSync(SIMPLE_MDS, 'utf8');
    // lint(source, {basePath: dir}) should produce the same canonical JSON.
    const dir = path.dirname(SIMPLE_MDS);
    const strResult = lint(source, { basePath: dir });
    // Compare JSON serializations — they must be byte-identical.
    assert.equal(
      JSON.stringify(strResult),
      JSON.stringify(fileResult),
      'lint and lintFile must produce identical canonical JSON for the same source',
    );
  });

  // P-L-2: lintVirtual matches checked-in canonical JSON goldens (AC-API-06)
  //
  // These goldens are derived from the Rust core serializer and checked in — not
  // regenerated from the napi binding, so the comparison is non-circular. A drift
  // in the serializer would fail this test before it ships to consumers.
  //
  // Keys are in BTreeMap (alphabetical) order:
  //   {"files":[...],"truncated":false,"version":1}
  test('P-L-2: lintVirtual clean source matches canonical golden', () => {
    const CLEAN_GOLDEN = '{"files":[],"truncated":false,"version":1}';
    const result = lintVirtual({ 'main.mds': 'Hello World!\n' }, 'main.mds');
    assert.equal(
      JSON.stringify(result),
      CLEAN_GOLDEN,
      `napi lintVirtual clean golden mismatch:\n  got:    ${JSON.stringify(result)}\n  expect: ${CLEAN_GOLDEN}`,
    );
  });

  test('P-L-3: lintVirtual unused-variable source matches canonical golden', () => {
    const UNUSED_GOLDEN =
      '{"files":[{"diagnostics":[{"fix_edits":null,"fixable":false,"help":"Remove the frontmatter key or reference it in the template body.",' +
      '"message":"Variable \'unused_key\' is defined in frontmatter but never referenced in the body.",' +
      '"rule":"unused-variable","severity":"warn","span":{"length":10,"offset":4}}],"file":"main.mds"}],"truncated":false,"version":1}';
    const result = lintVirtual(
      { 'main.mds': '---\nunused_key: value\n---\nHello!\n' },
      'main.mds',
    );
    assert.equal(
      JSON.stringify(result),
      UNUSED_GOLDEN,
      `napi lintVirtual unused-variable golden mismatch:\n  got:    ${JSON.stringify(result)}\n  expect: ${UNUSED_GOLDEN}`,
    );
  });
});

// ── Source map tests ──────────────────────────────────────────────────────────

describe('source maps (F-SM)', () => {
  // F-SM1: sourceMap present in markdown result when sourceMap: true
  test('F-SM1: sourceMap is present in result when sourceMap: true', () => {
    const result = compile('Hello World!\n', { sourceMap: true });
    assert.equal(result.kind, 'markdown');
    assert.ok('sourceMap' in result, 'sourceMap must be present in result');
    const sm = result.sourceMap;
    assert.equal(sm.version, 3, 'sourceMap.version must be 3');
    assert.ok(Array.isArray(sm.sources), 'sourceMap.sources must be an array');
    assert.ok(sm.sources.length > 0, 'sourceMap.sources must be non-empty');
    // String-source uses "input.mds" (STRING_SOURCE_MAP_LABEL) — unified with WASM.
    assert.equal(sm.sources[0], 'input.mds',
      `sources[0] must be "input.mds" after map_source_label fix; got: ${JSON.stringify(sm.sources[0])}`);
    assert.ok(Array.isArray(sm.names), 'sourceMap.names must be an array');
    assert.equal(sm.names.length, 0, 'sourceMap.names must be empty');
    assert.equal(typeof sm.mappings, 'string', 'sourceMap.mappings must be a string');
    assert.ok(sm.mappings.length > 0, 'sourceMap.mappings must be non-empty');
    // file is absent (bindings set file: None)
    assert.ok(!('file' in sm), 'sourceMap.file must be absent for string compilation');
    // sourcesContent absent (not requested)
    assert.ok(!('sourcesContent' in sm), 'sourcesContent must be absent when not requested');
  });

  // F-SM2: sourceMap absent when sourceMap not requested (default)
  test('F-SM2: sourceMap is absent in result by default', () => {
    const result = compile('Hello World!\n');
    assert.ok(!('sourceMap' in result), 'sourceMap must be absent by default');
  });

  // F-SM3: sourcesContent present and index-aligned when requested
  test('F-SM3: sourcesContent present and index-aligned when sourcesContent: true', () => {
    const result = compile('Hello World!\n', { sourceMap: true, sourcesContent: true });
    const sm = result.sourceMap;
    assert.ok('sourcesContent' in sm, 'sourcesContent must be present when requested');
    assert.ok(Array.isArray(sm.sourcesContent), 'sourcesContent must be an array');
    // sourcesContent must be index-aligned with sources
    assert.equal(
      sm.sourcesContent.length,
      sm.sources.length,
      'sourcesContent must have same length as sources',
    );
    for (const sc of sm.sourcesContent) {
      assert.equal(typeof sc, 'string', 'each sourcesContent element must be a string');
    }
  });

  // F-SM4: sourceMap absent for messages-mode (degradation per AC-FUNC-07)
  test('F-SM4: sourceMap absent for messages-mode compilation (degradation)', () => {
    const result = compile('@message user:\nHello!\n@end\n', { sourceMap: true });
    assert.equal(result.kind, 'messages');
    // Messages-mode degrades to source_map: None + a warning
    assert.ok(!('sourceMap' in result), 'sourceMap must be absent for messages result');
    // Warning text may use "source map" (spaces) or "source_map" (underscore)
    assert.ok(
      result.warnings.some((w) => /source.?map/i.test(w)),
      `expected a source-map degradation warning; got: ${JSON.stringify(result.warnings)}`,
    );
  });

  // F-SM5: sourcesContent without sourceMap rejects with mds::invalid_options
  test('F-SM5: sourcesContent without sourceMap rejects with mds::invalid_options', () => {
    assert.throws(
      () => compile('Hello!\n', { sourcesContent: true }),
      (err) => {
        assert.equal(err.code, 'mds::invalid_options', `got code: ${err.code}`);
        assert.ok(
          err.message.includes('sourcesContent'),
          `expected message to mention sourcesContent; got: ${err.message}`,
        );
        return true;
      },
    );
    // compileFile also rejects
    assert.throws(
      () => compileFile(SIMPLE_MDS, { sourcesContent: true }),
      (err) => {
        assert.equal(err.code, 'mds::invalid_options', `got code: ${err.code}`);
        return true;
      },
    );
  });

  // F-SM6: unknown-key rejection; new keys are valid and not rejected
  test('F-SM6: unknown key rejected; sourceMap and sourcesContent are valid', () => {
    // Unknown key is still rejected
    assert.throws(
      () => compile('Hello!\n', { unknownKey: true }),
      (err) => {
        assert.equal(err.code, 'mds::invalid_options', `got code: ${err.code}`);
        return true;
      },
    );
    // New keys are valid
    assert.doesNotThrow(() => compile('Hello!\n', { sourceMap: false }));
    assert.doesNotThrow(() => compile('Hello!\n', { sourceMap: true, sourcesContent: false }));
  });

  // F-SM7: structural validity of sourceMap from compileFile
  //
  // After the ADR-005 Phase A choke-point fix (source_path.rs::relativize_source),
  // compileFile emits ROOT-RELATIVE paths in sources[] — NOT absolute filesystem
  // paths.  The value is slash-separated relative to the project root (found via
  // .mdsroot / .git walk-up).  The assertion below checks the path ends with the
  // fixture basename; for a stronger cross-surface parity assertion see CF-SM1 in
  // packages/mds/__test__/source-map.spec.mjs.
  test('F-SM7: compileFile produces structurally valid sourceMap with root-relative sources[]', () => {
    const result = compileFile(SIMPLE_MDS, { sourceMap: true });
    assert.equal(result.kind, 'markdown');
    const sm = result.sourceMap;
    assert.equal(sm.version, 3);
    assert.ok(Array.isArray(sm.sources), 'sources must be array');
    assert.ok(sm.sources.length > 0, 'sources must be non-empty');
    assert.equal(typeof sm.mappings, 'string', 'mappings must be string');
    assert.ok(sm.mappings.length > 0, 'mappings must be non-empty');
    // file is absent (bindings do not set file)
    assert.ok(!('file' in sm), 'file must be absent (bindings do not set file)');
    // sources[0] must be root-relative (not an absolute path).
    assert.ok(
      !sm.sources[0].startsWith('/') && !sm.sources[0].match(/^[A-Za-z]:\\/),
      `sources[0] must be root-relative, not an absolute path; got: ${JSON.stringify(sm.sources[0])}`,
    );
    assert.ok(
      sm.sources[0].endsWith('.mds'),
      `sources[0] must end with .mds extension; got: ${JSON.stringify(sm.sources[0])}`,
    );
    // Validate mappings contains only valid Base64-VLQ chars
    assert.ok(
      /^[A-Za-z0-9+/,;]*$/.test(sm.mappings),
      `mappings contains invalid chars: ${sm.mappings}`,
    );
  });
});

// ── T-12/T-13 (E-10/E-11): ESC-injection hardening — napi direct (issue #176 / CWE-150) ────────
//
// Four vectors (E-10..E-13):
//  (E-10) error path: `@include fo<ESC>o` — parser rejects invalid alias, message
//      embeds the raw alias. After fix: err.message has no raw C0/DEL/C1 chars.
//  (E-11) lint path: lintVirtual with module name containing U+001B, imported twice —
//      duplicate-import fires; diagnostic message has no raw ESC char.
//  (E-12) error path with DEL (U+007F) — same as E-10 with a different control char.
//  (E-13) lint path with U+0085 (NEL/C1) — passes serde_yaml_ng, provides C1 coverage.

describe('ESC-injection hardening (issue #176 / CWE-150)', () => {
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
      // literal terminators), and U+FEFF (invisible BOM) — all escaped by the
      // sanitizers. `\n` is allowed here; wire-mode newline escaping is asserted
      // explicitly by E-15.
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

  test('T-12 / E-10: error path — compile error message sanitized for ESC-in-alias', () => {
    const esc = String.fromCharCode(0x1b);
    const source = `@include fo${esc}o\n`;
    try {
      compile(source);
      assert.fail('expected compile to throw');
    } catch (err) {
      const msg = err.message;
      assert.ok(typeof msg === 'string' && msg.length > 0, 'message must be non-empty');
      assertNoControlChars(msg, 'err.message');
      assert.ok(
        msg.includes('\\u001B'),
        `sanitized \\u001B must appear in err.message; got: ${JSON.stringify(msg)}`
      );
    }
  });

  test('T-13 / E-11: lint path — lintVirtual with ESC in module name sanitizes duplicate-import message', () => {
    // Use lintVirtual with a module whose NAME contains a raw ESC byte (U+001B),
    // imported twice so duplicate-import fires and embeds the raw path in its message.
    // Mirrors Python E12 (test_e12_lint_virtual_esc_in_import_path_message_sanitized).
    // Verifies:
    //   (1) No raw C0/DEL/C1 bytes in any diagnostic message.
    //   (2) Sanitized \u001B literal IS present (positive evidence, non-vacuous).
    //   (3) Result shape: version 1, duplicate-import rule present.
    const esc = String.fromCharCode(0x1b);
    const moduleName = `fo${esc}o.mds`;
    const modules = {
      [moduleName]: 'hi\n',
      'main.mds': `@import "./${moduleName}"\n@import "./${moduleName}"\n`,
    };
    const result = lintVirtual(modules, 'main.mds');

    // (3) Result shape: version 1.
    assert.equal(result.version, 1, 'T-13/E-11: version must be 1');
    assert.ok(Array.isArray(result.files), 'T-13/E-11: files must be an array');

    const allDiags = result.files.flatMap((f) => f.diagnostics);
    assert.ok(
      allDiags.length > 0,
      'T-13/E-11: expected at least one diagnostic (duplicate-import should fire); got: ' +
        JSON.stringify(allDiags),
    );

    // (1) No raw control bytes in any diagnostic message.
    for (const diag of allDiags) {
      if (typeof diag.message === 'string') {
        assertNoControlChars(diag.message, `T-13/E-11: diag[${diag.rule}].message`);
      }
    }

    // (2) At least one message carries the sanitized \u001B literal (positive evidence).
    const hasSanitizedEsc = allDiags.some(
      (d) =>
        typeof d.message === 'string' &&
        d.message.includes('\\u001B'),
    );
    assert.ok(
      hasSanitizedEsc,
      'T-13/E-11: expected \\u001B in at least one diagnostic message; got: ' +
        JSON.stringify(allDiags.map((d) => d.message)),
    );

    // (3) Expected rule: duplicate-import must be among the diagnostics.
    const hasDupImport = allDiags.some((d) => d.rule === 'duplicate-import');
    assert.ok(
      hasDupImport,
      'T-13/E-11: expected duplicate-import diagnostic; got rules: ' +
        JSON.stringify(allDiags.map((d) => d.rule)),
    );
  });

  test('E-12: error path — compile error message sanitized for DEL (U+007F) in alias', () => {
    // DEL (U+007F) in @include alias; parser rejects the invalid alias and embeds
    // the raw bytes in the error message. After fix, serialize() sanitizes DEL to \u007F.
    const del = String.fromCharCode(0x7f);
    const source = `@include fo${del}o\n`;
    try {
      compile(source);
      assert.fail('expected compile to throw');
    } catch (err) {
      const msg = err.message;
      assert.ok(typeof msg === 'string' && msg.length > 0, 'E-12: message must be non-empty');
      assertNoControlChars(msg, 'E-12: err.message');
      assert.ok(
        msg.includes('\\u007F'),
        `E-12: sanitized \\u007F must appear in err.message; got: ${JSON.stringify(msg)}`
      );
    }
  });

  test('E-13: lint path — lintVirtual with U+0085 (NEL/C1) in module name sanitizes message', () => {
    // U+0085 (NEL) is a C1 control char that passes serde_yaml_ng YAML parsing
    // (unlike ESC/DEL), making it a reachable C1 ESC-injection vector for lintVirtual.
    // The duplicate-import rule fires and embeds the raw module name in its message;
    // after sanitization the message must carry  and no raw C1 chars.
    const nel = String.fromCharCode(0x85);
    const moduleName = `fo${nel}o.mds`;
    const modules = {
      [moduleName]: 'hi\n',
      'main.mds': `@import "./${moduleName}"\n@import "./${moduleName}"\n`,
    };
    const result = lintVirtual(modules, 'main.mds');
    assert.equal(result.version, 1, 'E-13: version must be 1');
    const allDiags = result.files.flatMap((f) => f.diagnostics);
    assert.ok(
      allDiags.length > 0,
      'E-13: expected at least one diagnostic; got: ' + JSON.stringify(allDiags),
    );
    for (const diag of allDiags) {
      if (typeof diag.message === 'string') {
        assertNoControlChars(diag.message, `E-13: diag[${diag.rule}].message`);
      }
    }
    const hasSanitizedNel = allDiags.some(
      (d) => typeof d.message === 'string' && d.message.includes('\\u0085'),
    );
    assert.ok(
      hasSanitizedNel,
      'E-13: expected \\u0085 in at least one diagnostic message; got: ' +
        JSON.stringify(allDiags.map((d) => d.message)),
    );
  });

  test('E-14: lint path — U+202E (RLO) in module name is escaped on the wire', () => {
    // Trojan Source (CVE-2021-42574): U+202E is not a C0/DEL/C1 control char, so it
    // used to travel the wire untouched and reverse the display order of everything
    // after it in any bidi-aware renderer (terminal, IDE, code-review UI).
    // "fo<RLO>gnp.mds" renders as "fopng.mds".
    const rlo = String.fromCharCode(0x202e);
    const moduleName = `fo${rlo}gnp.mds`;
    const modules = {
      [moduleName]: 'hi\n',
      'main.mds': `@import "./${moduleName}"\n@import "./${moduleName}"\n`,
    };
    const result = lintVirtual(modules, 'main.mds');
    assert.equal(result.version, 1, 'E-14: version must be 1');
    const allDiags = result.files.flatMap((f) => f.diagnostics);
    assert.ok(
      allDiags.length > 0,
      'E-14: expected at least one diagnostic; got: ' + JSON.stringify(allDiags),
    );
    assert.ok(
      allDiags.some((d) => d.rule === 'duplicate-import'),
      'E-14: expected duplicate-import; got rules: ' +
        JSON.stringify(allDiags.map((d) => d.rule)),
    );
    for (const diag of allDiags) {
      if (typeof diag.message === 'string') {
        assertNoControlChars(diag.message, `E-14: diag[${diag.rule}].message`);
      }
    }
    // Cheap invariant check only — NOT coverage of the `file`-key escape. The
    // hostile RLO is in the *imported* module's name, but this key is the *entry*
    // filename ("main.mds"), so no hostile byte reaches it and this cannot fail via
    // this vector (PF-013). Real `file`-key coverage: mds-core
    // `to_canonical_json_escapes_bidi_override`.
    for (const f of result.files) {
      assertNoControlChars(f.file, 'E-14: files[].file');
    }
    assert.ok(
      allDiags.some((d) => typeof d.message === 'string' && d.message.includes('\\u202E')),
      'E-14: expected \\u202E in at least one diagnostic message; got: ' +
        JSON.stringify(allDiags.map((d) => d.message)),
    );
  });

  test('E-15: lint path — newline in a frontmatter key is escaped to \\u000A on the wire', () => {
    // Log/YAML-key forging: a raw newline inside a diagnostic message lets an
    // attacker forge what reads as a second, independent finding in any
    // line-oriented consumer of the JSON string value. WIRE mode escapes it.
    //
    // Reachability: a newline inside an `@import "..."` path is rejected by the
    // lexer, so that route is vacuous. A YAML *double-quoted* frontmatter key is
    // not: serde_yaml_ng decodes the \n escape into a real newline and
    // unused-variable embeds the decoded key verbatim in its message.
    const source =
      '---\n"a\\nerror[mds::forged]: FAKE\\nb": 1\n---\nHello\n';
    const result = lintVirtual({ 'main.mds': source }, 'main.mds');
    assert.equal(result.version, 1, 'E-15: version must be 1');
    const allDiags = result.files.flatMap((f) => f.diagnostics);
    assert.ok(
      allDiags.some((d) => d.rule === 'unused-variable'),
      'E-15: expected unused-variable; got rules: ' +
        JSON.stringify(allDiags.map((d) => d.rule)),
    );
    for (const diag of allDiags) {
      assert.ok(
        !diag.message.includes('\n'),
        `E-15: raw newline must not survive into the wire message; got: ${JSON.stringify(diag.message)}`,
      );
    }
    assert.ok(
      allDiags.some((d) => d.message.includes('\\u000A')),
      'E-15: expected \\u000A in at least one diagnostic message; got: ' +
        JSON.stringify(allDiags.map((d) => d.message)),
    );
    // Escaped, not stripped — the payload text itself is preserved verbatim.
    assert.ok(
      allDiags.some((d) => d.message.includes('error[mds::forged]')),
      'E-15: message body must be preserved verbatim; got: ' +
        JSON.stringify(allDiags.map((d) => d.message)),
    );
  });
});
