/**
 * compileFile() tests for @mds/mds universal package.
 * Tests: U-CF1 through U-CF9
 */
import { test, describe, before } from 'node:test';
import assert from 'node:assert/strict';
import { SIMPLE_MDS, IMPORT_CONSUMER_MDS, ENTRY_MDS, EMPTY_MDS, FRONTMATTER_ONLY_MDS, MD_EXTENSION } from './helpers.mjs';
import { compileFile, init } from '../dist/node.js';

describe('compileFile', () => {
  before(() => init());

  test('U-CF1: compile simple file', async () => {
    const result = await compileFile(SIMPLE_MDS);
    assert.ok(typeof result.output === 'string', 'output should be string');
    assert.ok(result.output.length > 0, 'output should not be empty');
    assert.ok(Array.isArray(result.warnings));
    assert.ok(Array.isArray(result.dependencies));
  });

  test('U-CF2: compile file with imports', async () => {
    const result = await compileFile(IMPORT_CONSUMER_MDS);
    assert.ok(result.output.includes('Hello World!'), `expected "Hello World!" in: ${result.output}`);
    // import_consumer imports import_provider
    assert.ok(result.dependencies.length >= 1, 'expected at least 1 dependency for file with imports');
  });

  test('U-CF3: compile file with deep import chain', async () => {
    const result = await compileFile(ENTRY_MDS);
    assert.ok(typeof result.output === 'string');
    assert.ok(result.output.length > 0);
  });

  test('U-CF4: compile empty file returns empty-like output', async () => {
    const result = await compileFile(EMPTY_MDS);
    assert.ok(typeof result.output === 'string');
  });

  test('U-CF5: compile frontmatter-only file', async () => {
    const result = await compileFile(FRONTMATTER_ONLY_MDS);
    assert.ok(typeof result.output === 'string');
  });

  test('U-CF6: compile .md extension file', async () => {
    const result = await compileFile(MD_EXTENSION);
    assert.ok(result.output.includes('Hello World!'), `expected content in: ${result.output}`);
  });

  test('U-CF7: compile nonexistent file rejects with error', async () => {
    await assert.rejects(
      () => compileFile('/nonexistent/path/file.mds'),
      (err) => {
        assert.ok(err instanceof Error, 'should throw Error');
        return true;
      },
    );
  });

  test('U-CF8: compile file with runtime vars', async () => {
    const result = await compileFile(SIMPLE_MDS, { vars: { count: 99 } });
    // vars override frontmatter — count should be overridden
    assert.ok(typeof result.output === 'string');
  });

  test('U-CF9: compile file returns proper shape', async () => {
    const result = await compileFile(SIMPLE_MDS);
    assert.ok('output' in result, 'result should have output');
    assert.ok('warnings' in result, 'result should have warnings');
    assert.ok('dependencies' in result, 'result should have dependencies');
    assert.ok(typeof result.output === 'string');
    assert.ok(Array.isArray(result.warnings));
    assert.ok(Array.isArray(result.dependencies));
  });
});
