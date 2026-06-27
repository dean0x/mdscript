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
    const source = '---\nname: Alice\n---\nHello {name}!\n';
    const result = compile(source);
    assert.equal(result.kind, 'markdown');
    assert.ok(result.output.includes('Hello Alice!'), `expected "Hello Alice!" in: ${result.output}`);
  });

  test('F-C6: compile with runtime vars', () => {
    const source = 'Hello {name}!\n';
    const result = compile(source, { vars: { name: 'Bob' } });
    assert.equal(result.kind, 'markdown');
    assert.equal(result.output, 'Hello Bob!\n');
  });

  test('F-C7: runtime vars override frontmatter', () => {
    const source = '---\nname: Alice\n---\nHello {name}!\n';
    const result = compile(source, { vars: { name: 'Override' } });
    assert.equal(result.kind, 'markdown');
    assert.ok(result.output.includes('Hello Override!'), `got: ${result.output}`);
  });

  test('F-C8: compile with basePath for import resolution', () => {
    const source = `@import { greet } from "./import_provider.mds"\n\n{greet("Test")}\n`;
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
    const source = '---\nname: Test\n---\nHello {name}!\n';
    const result = check(source);
    assert.deepEqual(result.warnings, []);
  });

  test('F-K5: check with runtime vars', () => {
    const source = 'Hello {name}!\n';
    const result = check(source, { vars: { name: 'Test' } });
    assert.deepEqual(result.warnings, []);
  });

  test('F-K6: check undefined variable throws', () => {
    assert.throws(
      () => check('Hello {undefined_var}!\n'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.equal(err.code, 'mds::undefined_var', `got: ${err.code}`);
        return true;
      },
    );
  });

  test('F-K7: check with basePath', () => {
    const source = `@import { greet } from "./import_provider.mds"\n\n{greet("Test")}\n`;
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
      () => compile('Hello {undefined_var}!\n'),
      (err) => {
        assert.ok(err instanceof Error, 'should be instanceof Error');
        return true;
      },
    );
  });

  test('E-2: error has code property', () => {
    assert.throws(
      () => compile('Hello {undefined_var}!\n'),
      (err) => {
        assert.ok('code' in err, 'should have code property');
        assert.ok(typeof err.code === 'string', `code should be string, got ${typeof err.code}`);
        return true;
      },
    );
  });

  test('E-3: error has message property', () => {
    assert.throws(
      () => compile('Hello {undefined_var}!\n'),
      (err) => {
        assert.ok(typeof err.message === 'string', 'should have message');
        assert.ok(err.message.length > 0, 'message should not be empty');
        return true;
      },
    );
  });

  test('E-4: undefined var error has correct code', () => {
    assert.throws(
      () => compile('Hello {undefined_var}!\n'),
      (err) => {
        assert.equal(err.code, 'mds::undefined_var', `got: ${err.code}`);
        return true;
      },
    );
  });

  test('E-5: undefined var error has help property', () => {
    assert.throws(
      () => compile('Hello {undefined_var}!\n'),
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
      () => compile('Hello {undefined_var}!\n'),
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
    const source = '---\nname: Alice\ncount: 3\n---\n\nHello {name}! You have {count} items.\n';
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
      '---\nrole: general\n---\nYou are a {role} assistant.\n@block body:\nDefault body.\n@end\n';
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
    const baseContent = '@block greeting:\nHello {customer_name}, welcome.\n@end\n';
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
    const source = '@message user:\n{data}\n@end\n';
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
    const source = '@message {role}:\nHello!\n@end\n';
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
