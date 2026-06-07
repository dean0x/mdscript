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

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);

// Load the native addon from the built .node file.
const addon = require('../mds-napi.node');
const { compile, compileFile, check, checkFile, compileMessages } = addon;

// Fixture directory.
const FIXTURES = path.join(__dirname, 'fixtures');
const SIMPLE_MDS = path.join(FIXTURES, 'simple.mds');
const VAR_MDS = path.join(FIXTURES, 'var.mds');
const IMPORT_PROVIDER_MDS = path.join(FIXTURES, 'import_provider.mds');
const IMPORT_CONSUMER_MDS = path.join(FIXTURES, 'import_consumer.mds');

// ── Compile tests ─────────────────────────────────────────────────────────────

describe('compile', () => {
  test('F-C1: basic compile, no options', () => {
    const result = compile('Hello World!\n');
    assert.equal(result.output, 'Hello World!\n');
    assert.ok(Array.isArray(result.warnings));
    assert.ok(Array.isArray(result.dependencies));
  });

  test('F-C2: compile with null options', () => {
    const result = compile('Hello World!\n', null);
    assert.equal(result.output, 'Hello World!\n');
  });

  test('F-C3: compile with undefined options', () => {
    const result = compile('Hello World!\n', undefined);
    assert.equal(result.output, 'Hello World!\n');
  });

  test('F-C4: compile with empty options object', () => {
    const result = compile('Hello World!\n', {});
    assert.equal(result.output, 'Hello World!\n');
  });

  test('F-C5: compile with frontmatter vars', () => {
    const source = '---\nname: Alice\n---\nHello {name}!\n';
    const result = compile(source);
    assert.ok(result.output.includes('Hello Alice!'), `expected "Hello Alice!" in: ${result.output}`);
  });

  test('F-C6: compile with runtime vars', () => {
    const source = 'Hello {name}!\n';
    const result = compile(source, { vars: { name: 'Bob' } });
    assert.equal(result.output, 'Hello Bob!\n');
  });

  test('F-C7: runtime vars override frontmatter', () => {
    const source = '---\nname: Alice\n---\nHello {name}!\n';
    const result = compile(source, { vars: { name: 'Override' } });
    assert.ok(result.output.includes('Hello Override!'), `got: ${result.output}`);
  });

  test('F-C8: compile with basePath for import resolution', () => {
    const source = `@import { greet } from "./import_provider.mds"\n\n{greet("Test")}\n`;
    const result = compile(source, { basePath: FIXTURES });
    assert.ok(result.output.includes('Hello Test!'), `got: ${result.output}`);
  });

  test('F-C9: empty source compiles successfully', () => {
    const result = compile('');
    assert.equal(result.output, '');
    assert.deepEqual(result.warnings, []);
    assert.deepEqual(result.dependencies, []);
  });
});

// ── CompileFile tests ─────────────────────────────────────────────────────────

describe('compileFile', () => {
  test('F-CF1: compile file', () => {
    const result = compileFile(SIMPLE_MDS);
    assert.ok(result.output.includes('Hello Alice!'), `got: ${result.output}`);
    assert.ok(result.output.includes('3 items'), `got: ${result.output}`);
  });

  test('F-CF2: compile file with vars', () => {
    const result = compileFile(VAR_MDS, { vars: { name: 'World' } });
    assert.equal(result.output, 'Hello World!\n');
  });

  test('F-CF3: compile file with imports', () => {
    const result = compileFile(IMPORT_CONSUMER_MDS);
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
    // The simple.mds has frontmatter with name: Alice, count: 3.
    assert.ok(result.output.includes('Hello Alice!'), `expected "Hello Alice!" in: ${result.output}`);
    assert.ok(result.output.includes('3 items'), `expected "3 items" in: ${result.output}`);
  });

  test('P-2: compile and compileFile agree on same source + basePath', () => {
    const source = '---\nname: Alice\ncount: 3\n---\n\nHello {name}! You have {count} items.\n';
    const compileResult = compile(source, { basePath: FIXTURES });
    const fileResult = compileFile(SIMPLE_MDS);
    assert.equal(compileResult.output, fileResult.output);
  });
});

// ── compileMessages tests ─────────────────────────────────────────────────────

describe('compileMessages', () => {
  // M-1: basic success path — result shape is correct
  test('M-1: basic compileMessages returns messages/warnings/dependencies arrays', () => {
    const source = '@message system:\nYou are helpful.\n@end\n';
    const result = compileMessages(source);
    assert.ok(Array.isArray(result.messages), 'messages should be an array');
    assert.ok(Array.isArray(result.warnings), 'warnings should be an array');
    assert.ok(Array.isArray(result.dependencies), 'dependencies should be an array');
    assert.equal(result.messages.length, 1);
    assert.equal(result.messages[0].role, 'system');
    assert.ok(typeof result.messages[0].content === 'string', 'content should be a string');
  });

  // M-2: vars marshaling — dynamic role via { vars: { ... } } produces correct role
  test('M-2: vars marshaling — { vars } produces dynamic role correctly', () => {
    const source = '@message {role}:\nHello!\n@end\n';
    const result = compileMessages(source, { vars: { role: 'assistant' } });
    assert.equal(result.messages.length, 1);
    assert.equal(result.messages[0].role, 'assistant');
    assert.equal(result.messages[0].content, 'Hello!');
  });

  // M-3: no @message blocks throws a JS Error with code property
  test('M-3: no @message blocks throws JS Error with code property', () => {
    assert.throws(
      () => compileMessages('Hello World\n'),
      (err) => {
        assert.ok(err instanceof Error, 'should be instanceof Error');
        assert.ok('code' in err, 'should have code property');
        assert.ok(typeof err.code === 'string', `code should be string, got ${typeof err.code}`);
        assert.ok(err.code.startsWith('mds::'), `code should start with mds::, got: ${err.code}`);
        return true;
      },
    );
  });

  // M-4: unknown option key throws mds::invalid_options (parse_compile_opts path)
  test('M-4: unknown option key throws mds::invalid_options', () => {
    assert.throws(
      () => compileMessages('@message user:\nHi\n@end\n', { unknownKey: true }),
      (err) => {
        assert.equal(err.code, 'mds::invalid_options', `got: ${err.code}`);
        return true;
      },
    );
  });

  // M-5: basePath-relative @import resolves in messages mode; dependencies is non-empty
  test('M-5: basePath-relative @import resolves; dependencies is non-empty', () => {
    const source =
      '@import { greet } from "./import_provider.mds"\n\n@message user:\n{greet("World")}\n@end\n';
    const result = compileMessages(source, { basePath: FIXTURES });
    assert.equal(result.messages.length, 1);
    assert.equal(result.messages[0].role, 'user');
    assert.ok(
      result.messages[0].content.includes('Hello World!'),
      `expected "Hello World!" in content: ${result.messages[0].content}`,
    );
    assert.ok(result.dependencies.length > 0, 'dependencies should be non-empty after @import');
    for (const dep of result.dependencies) {
      assert.ok(path.isAbsolute(dep), `dependency should be absolute path: ${dep}`);
    }
  });

  // M-6: check_source_size rejects oversized source with mds::resource_limit (avoids PF-004)
  test('M-6: oversized source throws mds::resource_limit', () => {
    const MAX_SOURCE_SIZE = 10 * 1024 * 1024; // 10 MiB — mirrors R-1/R-2
    const oversized = 'x'.repeat(MAX_SOURCE_SIZE + 1);
    assert.throws(
      () => compileMessages(oversized),
      (err) => {
        assert.equal(err.code, 'mds::resource_limit', `got: ${err.code}`);
        return true;
      },
    );
  });

  // M-7: null and undefined opts are both accepted
  test('M-7: null opts are accepted', () => {
    const result = compileMessages('@message user:\nHi\n@end\n', null);
    assert.equal(result.messages.length, 1);
  });

  test('M-8: undefined opts are accepted', () => {
    const result = compileMessages('@message user:\nHi\n@end\n', undefined);
    assert.equal(result.messages.length, 1);
  });

  // M-9: multiple messages, correct ordering
  test('M-9: multiple @message blocks produce ordered messages array', () => {
    const source =
      '@message system:\nYou are helpful.\n@end\n@message user:\nWhat is 2+2?\n@end\n@message assistant:\n4\n@end\n';
    const result = compileMessages(source);
    assert.equal(result.messages.length, 3);
    assert.equal(result.messages[0].role, 'system');
    assert.equal(result.messages[1].role, 'user');
    assert.equal(result.messages[2].role, 'assistant');
  });
});
