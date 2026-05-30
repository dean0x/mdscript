/**
 * Core compile() tests for @mdscript/mds universal package.
 * Tests: U-C1 through U-C9
 */
import { test, describe, before } from 'node:test';
import assert from 'node:assert/strict';
import { compile, isMdsError, init } from '../dist/node.js';

describe('compile', () => {
  before(() => init());

  test('U-C1: compile plain text with no options', () => {
    const result = compile('Hello World!\n');
    assert.equal(result.output, 'Hello World!\n');
    assert.ok(Array.isArray(result.warnings), 'warnings should be array');
    assert.ok(Array.isArray(result.dependencies), 'dependencies should be array');
    assert.equal(result.warnings.length, 0);
    assert.equal(result.dependencies.length, 0);
  });

  test('U-C2: compile with frontmatter variables', () => {
    const source = '---\nname: Alice\n---\nHello {name}!\n';
    const result = compile(source);
    assert.ok(result.output.includes('Hello Alice!'), `expected "Hello Alice!" in: ${result.output}`);
  });

  test('U-C3: compile with runtime vars', () => {
    const source = 'Hello {name}!\n';
    const result = compile(source, { vars: { name: 'World' } });
    assert.equal(result.output, 'Hello World!\n');
  });

  test('U-C4: compile with frontmatter returns string output and array warnings', () => {
    // Verify the result shape when source contains a frontmatter block.
    const source = '---\nname: Test\n---\nHello!\n';
    const result = compile(source);
    assert.equal(typeof result.output, 'string');
    assert.ok(Array.isArray(result.warnings));
    assert.ok(Array.isArray(result.dependencies));
  });

  test('U-C5: compile syntax error throws MdsError with code', () => {
    assert.throws(
      () => compile('Hello {name\n'),
      (err) => {
        assert.ok(isMdsError(err), `expected MdsError, got: ${err}`);
        assert.ok(typeof err.code === 'string', 'code should be string');
        return true;
      },
    );
  });

  test('U-C6: compile returns empty dependencies when no imports', () => {
    const result = compile('Hello World!\n');
    assert.deepEqual(result.dependencies, []);
  });

  test('U-C7: compile with null vars produces identical output to no-vars compile', () => {
    // null vars must be treated as absent — varsOpt uses != null so both null
    // and undefined are omitted from the options passed to the backend.
    const source = 'Hello World!\n';
    const withNull = compile(source, { vars: null });
    const withoutVars = compile(source);
    assert.equal(withNull.output, withoutVars.output, 'null vars should produce same output as no vars');
  });

  test('U-C8: compile with undefined vars does not throw', () => {
    const result = compile('Hello World!\n', { vars: undefined });
    assert.equal(result.output, 'Hello World!\n');
  });

  test('U-C9: compile with empty vars object does not throw', () => {
    const result = compile('Hello World!\n', { vars: {} });
    assert.equal(result.output, 'Hello World!\n');
  });
});
