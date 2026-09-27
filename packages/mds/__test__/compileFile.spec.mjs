/**
 * compileFile() tests for @mdscript/mds universal package.
 * Tests: U-CF1 through U-CF9, CF-NOTMDS
 */
import { test, describe, before } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, realpath, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {
  SIMPLE_MDS,
  IMPORT_CONSUMER_MDS,
  ENTRY_MDS,
  EMPTY_MDS,
  FRONTMATTER_ONLY_MDS,
  MD_EXTENSION,
  compileFileOutcomes,
  loadEngines,
  pkgRoot,
  requireEngines,
} from './helpers.mjs';
import { compileFile, init } from '../dist/node.js';

/**
 * Whether `message` names `canonical` anywhere but inside an occurrence of `typed`.
 * A bare `includes` is wrong both ways: a typed relative path can hold the canonical
 * path (`../…/tmp/x/doc.txt` for `/tmp/x/doc.txt`), and the canonical path can hold a
 * typed one (`/private/var/…` for `/var/…` on macOS).
 */
function namesCanonicalOutsideTyped(message, typed, canonical) {
  const startsOf = (needle) => {
    assert.ok(needle.length > 0, 'an empty needle occurs everywhere');
    const starts = [];
    // Bounded by message.length: each search starts past the previous hit.
    for (let at = message.indexOf(needle); at !== -1; at = message.indexOf(needle, at + 1)) {
      starts.push(at);
    }
    return starts;
  };
  const spans = startsOf(typed).map((at) => [at, at + typed.length]);
  return startsOf(canonical).some(
    (at) => !spans.some(([from, to]) => from <= at && at + canonical.length <= to),
  );
}

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

  // The native backend only: the WASM backend's pre-scanner names a not-MDS entry by
  // its own route (#417 WASM parity is a separate change).
  test('CF-NOTMDS: a non-MDS entry is named as typed on the native backend (#417)', async (t) => {
    const engines = await loadEngines();
    if (!requireEngines(t, { native: engines.native }, 'CF-NOTMDS')) return;
    const dir = await mkdtemp(path.join(os.tmpdir(), 'mds-u-cf-notmds-'));
    try {
      await mkdir(path.join(dir, 'sub'));
      await writeFile(path.join(dir, 'doc.txt'), 'Hello!\n');
      await writeFile(path.join(dir, 'page.md'), '---\ntype: mds\n---\nHello!\n');
      const canonicalDir = await realpath(dir);
      const canonical = path.join(canonicalDir, 'doc.txt');
      // Typed absolute, through `sub/..` (joined by hand: path.join would drop it), and
      // relative to compileFileOutcomes' working directory, the package root — from the
      // directory as typed and from its canonical form. A relative form holds the
      // canonical path wherever the two share only the filesystem root: the last one
      // on macOS (`../…/private/var/…`), both on Linux (`../…/tmp/…`).
      const dotted = (name) => [dir, 'sub', '..', name].join(path.sep);
      const relative = (name) => path.relative(pkgRoot, path.join(dir, name));
      const viaCanonical = (name) => path.relative(pkgRoot, path.join(canonicalDir, name));
      const typed = [path.join(dir, 'doc.txt'), dotted('doc.txt'), relative('doc.txt'), viaCanonical('doc.txt')];
      const controls = [dotted('page.md'), relative('page.md'), viaCanonical('page.md')];
      const outcomes = await compileFileOutcomes('native', [...typed, ...controls]);
      typed.forEach((entry, i) => {
        assert.deepEqual(
          outcomes[i],
          {
            code: 'mds::not_mds',
            message: `not an MDS file: ${entry}`,
            help: "use .mds extension or add 'type: mds' to frontmatter",
            span: null,
          },
          entry,
        );
        // The canonical path shows only as part of the typed form, if at all (off
        // macOS the plain absolute form IS the canonical path).
        assert.ok(
          !namesCanonicalOutsideTyped(outcomes[i].message, entry, canonical),
          `${entry}: ${outcomes[i].message}`,
        );
        // Control: the check catches the canonical path named in place of the typed
        // form, and beside it.
        if (entry !== canonical) {
          for (const named of [canonical, `${entry} (${canonical})`]) {
            const message = `not an MDS file: ${named}`;
            assert.ok(namesCanonicalOutsideTyped(message, entry, canonical), `control: ${message}`);
          }
        }
      });
      // Control: the same forms naming a `type: mds` file compile.
      controls.forEach((entry, i) => {
        assert.deepEqual(outcomes[typed.length + i], { output: 'Hello!\n' }, entry);
      });
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  });
});
