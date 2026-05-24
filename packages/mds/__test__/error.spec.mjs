/**
 * Error shape tests for @mds/mds universal package.
 * Tests: U-E1 through U-E9
 */
import { test, describe, before } from 'node:test';
import assert from 'node:assert/strict';
import { compile, check, isMdsError, init } from '../dist/node.js';

describe('error shape', () => {
  before(() => init());

  test('U-E1: compile syntax error is an Error instance', () => {
    try {
      compile('Hello {name\n');
      assert.fail('expected error to be thrown');
    } catch (err) {
      assert.ok(err instanceof Error, `expected Error instance, got: ${typeof err}`);
    }
  });

  test('U-E2: compile syntax error has code property', () => {
    try {
      compile('Hello {name\n');
      assert.fail('expected error to be thrown');
    } catch (err) {
      assert.ok(typeof (err).code === 'string', `expected code string, got: ${(err).code}`);
    }
  });

  test('U-E3: isMdsError returns true for MDS errors', () => {
    try {
      compile('Hello {name\n');
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
      check('Hello {name\n');
      assert.fail('expected error to be thrown');
    } catch (err) {
      assert.ok(isMdsError(err), 'should be MdsError');
      assert.ok(typeof err.code === 'string');
    }
  });

  test('U-E7: undefined variable error has syntax-related code', () => {
    try {
      // Using an undefined variable in strict mode should error.
      compile('{undefinedVar}\n');
      assert.fail('expected error');
    } catch (err) {
      assert.ok(isMdsError(err), 'should be MdsError');
      assert.ok(typeof err.code === 'string', 'error code should be present');
    }
  });

  test('U-E8: error message is a non-empty string', () => {
    try {
      compile('Hello {name\n');
      assert.fail('expected error');
    } catch (err) {
      assert.ok(err instanceof Error);
      assert.ok(typeof err.message === 'string');
      assert.ok(err.message.length > 0, 'error message should not be empty');
    }
  });
});
