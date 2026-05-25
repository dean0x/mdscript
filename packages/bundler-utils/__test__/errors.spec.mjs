/**
 * Tests for error formatting utilities.
 */
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { formatMdsError } from '../dist/index.js';

// ---------------------------------------------------------------------------
// Helper: create a mock MdsError
// ---------------------------------------------------------------------------
function makeMdsError(opts = {}) {
  const err = new Error(opts.message ?? 'Something went wrong');
  err.code = opts.code ?? 'mds::undefined_variable';
  if (opts.help !== undefined) err.help = opts.help;
  if (opts.span !== undefined) err.span = opts.span;
  return err;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
describe('formatMdsError', () => {
  test('formats MdsError with span (line and column)', () => {
    const err = makeMdsError({
      message: 'Undefined variable: foo',
      span: { offset: 10, length: 3, line: 5, column: 2 },
    });
    const result = formatMdsError(err, '/file.mds');

    assert.equal(result.message, 'Undefined variable: foo');
    assert.equal(result.id, '/file.mds');
    assert.equal(result.line, 5);
    assert.equal(result.column, 2);
  });

  test('formats MdsError without span', () => {
    const err = makeMdsError({ message: 'Parse error' });
    const result = formatMdsError(err, '/file.mds');

    assert.equal(result.message, 'Parse error');
    assert.equal(result.id, '/file.mds');
    assert.equal(result.line, undefined);
    assert.equal(result.column, undefined);
  });

  test('appends help text to message', () => {
    const err = makeMdsError({
      message: 'Undefined variable: foo',
      help: 'Did you mean to define foo in frontmatter?',
    });
    const result = formatMdsError(err, '/file.mds');

    assert.ok(
      result.message.includes('help: Did you mean to define foo in frontmatter?'),
      `Expected help in message: ${result.message}`,
    );
  });

  test('formats generic Error', () => {
    const err = new Error('Something went wrong');
    const result = formatMdsError(err, '/file.mds');

    assert.equal(result.message, 'Something went wrong');
    assert.equal(result.id, '/file.mds');
    assert.equal(result.line, undefined);
  });

  test('formats non-Error value', () => {
    const result = formatMdsError('a string error', '/file.mds');

    assert.equal(result.message, 'a string error');
    assert.equal(result.id, '/file.mds');
  });

  test('formats null', () => {
    const result = formatMdsError(null, '/file.mds');
    assert.equal(typeof result.message, 'string');
  });

  test('formats MdsError with span but no line/column', () => {
    const err = makeMdsError({
      message: 'Error without position',
      span: { offset: 5, length: 2 },
    });
    const result = formatMdsError(err, '/file.mds');

    assert.equal(result.line, undefined);
    assert.equal(result.column, undefined);
  });
});
