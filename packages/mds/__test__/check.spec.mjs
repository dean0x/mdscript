/**
 * check() and checkFile() tests for @mds/mds universal package.
 * Tests: U-K1 through U-KF3
 */
import { test, describe, before } from 'node:test';
import assert from 'node:assert/strict';
import { SIMPLE_MDS, IMPORT_CONSUMER_MDS } from './helpers.mjs';
import { check, checkFile, isMdsError, init } from '../dist/node.js';

describe('check', () => {
  before(() => init());

  test('U-K1: check valid source returns empty warnings', () => {
    const result = check('---\nname: World\n---\nHello {name}!\n');
    assert.deepEqual(result.warnings, []);
  });

  test('U-K2: check plain text returns warnings array', () => {
    const result = check('Hello World!\n');
    assert.ok(Array.isArray(result.warnings));
  });

  test('U-K3: check syntax error throws MdsError', () => {
    assert.throws(
      () => check('Hello {name\n'),
      (err) => {
        assert.ok(isMdsError(err), `expected MdsError, got: ${err}`);
        return true;
      },
    );
  });

  test('U-K4: check with runtime vars succeeds', () => {
    const result = check('Hello {name}!\n', { vars: { name: 'Test' } });
    assert.ok(Array.isArray(result.warnings));
  });

  test('U-K5: check has no output field', () => {
    const result = check('Hello!\n');
    assert.ok(!('output' in result), 'check result should not have output field');
    assert.ok('warnings' in result, 'check result should have warnings field');
  });
});

describe('checkFile', () => {
  before(() => init());

  test('U-KF1: checkFile valid file succeeds', async () => {
    const result = await checkFile(SIMPLE_MDS);
    assert.ok(Array.isArray(result.warnings));
  });

  test('U-KF2: checkFile with imports succeeds', async () => {
    const result = await checkFile(IMPORT_CONSUMER_MDS);
    assert.ok(Array.isArray(result.warnings));
  });

  test('U-KF3: checkFile nonexistent file rejects', async () => {
    await assert.rejects(
      () => checkFile('/nonexistent/path/file.mds'),
      (err) => {
        assert.ok(err instanceof Error);
        return true;
      },
    );
  });
});
