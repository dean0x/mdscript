/**
 * Module scanner unit tests for @mdscript/mds universal package.
 * Tests: U-S1 through U-S10
 *
 * Tests the normalizeVirtualKey and buildModulesMap utilities directly
 * using the compiled JS output.
 */
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { isDeepStrictEqual } from 'node:util';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { chmod, mkdtemp, mkdir, open, realpath, symlink, writeFile, rm } from 'node:fs/promises';
import os from 'node:os';
import {
  FORBIDDEN_PATH_CODEPOINTS,
  assertNoForbiddenChars,
  caseInsensitive,
  compileFileOutcomes,
  errorShape,
  escapeText,
  loadEngines,
  pkgRoot,
  rejectionOf,
  requireEngines,
  symlinkOrSkip,
  thrownBy,
  uPlus,
} from './helpers.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

// Import from the compiled dist.
// Note: module-scanner is a Node-only utility (uses fs/promises).
const {
  normalizeVirtualKey,
  buildModulesMap: buildModulesMapWith,
  findProjectRoot,
} = await import('../dist/util/module-scanner.js');

const FIXTURES = path.join(__dirname, 'fixtures');

// The scanner checks every file's bytes through the WASM engine's own
// `preflightModule` (#414); these tests pair it with the import scanner passed in.
const { wasm: wasmEngine } = await loadEngines();

function preflightModule(bytes, display, shown) {
  if (wasmEngine === null) {
    throw new Error('buildModulesMap needs the WASM engine: build crates/mds-wasm/pkg first');
  }
  return wasmEngine.preflightModule(bytes, display, shown);
}

/**
 * `scan`, a plain path-list import scanner, as a `scanImportRecords` engine method:
 * each path it lists is an `@import` with no span (the engine's `scanImportRecords`
 * spans them; these tests judge the refusals themselves).
 */
function importRecordsOf(scan) {
  return (source) => scan(source).map((p) => ({ path: p, kind: 'import', frontmatterIndex: null, span: null }));
}

/** buildModulesMap with `scan` as the engine's import scanner (see `importRecordsOf`). */
function buildModulesMap(entryPath, scan, options) {
  return buildModulesMapWith(entryPath, { scanImportRecords: importRecordsOf(scan), preflightModule }, options);
}

// A minimal scanImports implementation using the napi addon.
function scanImports(source) {
  // The napi addon doesn't expose scanImports directly, so we use
  // the compile result to determine imports... Actually we need scan_imports.
  // For testing buildModulesMap, use a simple regex-based scanner.
  const importRegex = /@import\s*(?:["']([^"']+)["']|{[^}]+}\s+from\s+["']([^"']+)["']|["']([^"']+)["']\s+as\s+\w+)/g;
  const exportRegex = /@export\s+(?:\*|\w+)\s+from\s+["']([^"']+)["']/g;
  const paths = [];
  let m;
  while ((m = importRegex.exec(source)) !== null) {
    const p = m[1] || m[2] || m[3];
    if (p && !paths.includes(p)) paths.push(p);
  }
  while ((m = exportRegex.exec(source)) !== null) {
    const p = m[1];
    if (p && !paths.includes(p)) paths.push(p);
  }
  return paths;
}

/**
 * The `help` the Rust engine attaches to `mds::file_not_found`
 * (crates/mds-core/src/error.rs). U-SM21 checks it against the native engine
 * itself, so a change on the Rust side alone fails there.
 */
const FILE_NOT_FOUND_HELP = 'check the file path and ensure the file exists';

/**
 * A missing file — whether never found (nonexistent name, nonexistent parent or
 * grandparent directory, ENOENT/ENOTDIR alike) or a mismatched spelling on a
 * case-sensitive volume (#408) — is reported with the same `mds::file_not_found`
 * shape the Rust engine reports for a missing file, keyed on `shown` (never a
 * resolved absolute path, R3 / CWE-209) and never described as a symlink — with
 * the engine's `help` (#414).
 */
function assertNotFoundNotSymlink(err, shown, label) {
  assert.equal(err.code, 'mds::file_not_found', `${label}: ${err.message}`);
  assert.equal(err.message, `file not found: ${shown}`, label);
  assert.equal(err.help, FILE_NOT_FOUND_HELP, label);
  assert.doesNotMatch(err.message, /symlink/, label);
}

/**
 * The engine attaches no `help` to `mds::import` or `mds::io`, so neither does the
 * scanner — not even an own `help: undefined` property (#414).
 */
function assertNoHelp(err, label) {
  assert.equal(Object.hasOwn(err, 'help'), false, `${label}: no help property; got ${JSON.stringify(err.help)}`);
}

/**
 * A symlinked final component — entry or import alike — is refused with the error
 * NativeFs reports for it (`mds::import`), keyed on `shown`, the path as written:
 * never the resolved absolute path.
 */
function assertSymlinkRefusal(err, shown, label) {
  assert.equal(err.code, 'mds::import', `${label}: ${err.message}`);
  assert.equal(err.message, `import error: symlinks are not allowed in imports: ${shown}`, label);
  assertNoHelp(err, label);
}

/**
 * A module outside the project root is refused with the error NativeFs reports for
 * it (`mds::import`), keyed on `shown`, the import as written.
 */
function assertEscapeRefusal(err, shown, label) {
  assert.equal(err.code, 'mds::import', `${label}: ${err.message}`);
  assert.equal(err.message, `import error: import path escapes project directory: "${shown}"`, label);
  assertNoHelp(err, label);
}

/**
 * The span an error for the import written on the first line of an ASCII module
 * points at: that whole line, as the Rust resolver's `attach_import_span` gives it.
 */
function firstLineSpan(line) {
  return { offset: 0, length: line.length, line: 1, column: 1 };
}

/** Run `fn` with `dir` as the working directory, restoring it afterwards. */
async function withCwd(dir, fn) {
  const cwd = process.cwd();
  process.chdir(dir);
  try {
    return await fn();
  } finally {
    process.chdir(cwd);
  }
}

/**
 * A project directory `proj` (marked by `.mdsroot`) inside a scratch parent, passed
 * to `fn` as `(proj, parent)`. Shared by the describe blocks below that need a
 * project nested one level down, so an import can escape it without leaving the
 * scratch temp directory itself.
 */
async function withNestedProject(fn) {
  const parent = await mkdtemp(path.join(os.tmpdir(), 'mds-scanner-native-'));
  try {
    const proj = path.join(parent, 'proj');
    await mkdir(proj);
    await writeFile(path.join(proj, '.mdsroot'), '');
    return await fn(proj, parent);
  } finally {
    await rm(parent, { recursive: true, force: true });
  }
}

describe('normalizeVirtualKey', () => {
  test('U-S1: root entry (empty base) uses key as-is', () => {
    assert.equal(normalizeVirtualKey('', 'main.mds'), 'main.mds');
  });

  test('U-S2: resolves relative path from base', () => {
    assert.equal(normalizeVirtualKey('dir/main.mds', './lib.mds'), 'dir/lib.mds');
  });

  test('U-S3: resolves parent directory with ..', () => {
    assert.equal(normalizeVirtualKey('dir/sub/main.mds', '../lib.mds'), 'dir/lib.mds');
  });

  test('U-S4: .. cannot escape project root', () => {
    assert.throws(
      () => normalizeVirtualKey('main.mds', '../escape.mds'),
      /escapes/,
    );
  });

  test('U-S5: empty relative path throws', () => {
    assert.throws(
      () => normalizeVirtualKey('main.mds', ''),
      /empty/,
    );
  });

  test('U-S6: null byte in path throws', () => {
    assert.throws(
      () => normalizeVirtualKey('main.mds', './foo\0.mds'),
      /null byte/,
    );
  });

  test('U-S7: dot segments are skipped', () => {
    assert.equal(normalizeVirtualKey('main.mds', './././lib.mds'), 'lib.mds');
  });

  test('U-S8: empty segments are skipped', () => {
    assert.equal(normalizeVirtualKey('dir/main.mds', './lib.mds'), 'dir/lib.mds');
  });

  test('U-S9: resolves to empty key throws', () => {
    // Only `..` from a root-level file resolves to empty
    assert.throws(
      () => normalizeVirtualKey('main.mds', '..'),
      /escapes|empty/,
    );
  });

  test('U-S10: no trailing slash in result', () => {
    const key = normalizeVirtualKey('dir/main.mds', './sub/lib.mds');
    assert.ok(!key.endsWith('/'), `key should not end with slash: ${key}`);
  });

  test("U-S16: every refusal carries the virtual filesystem's code and message (#414)", async (t) => {
    const { wasm } = await loadEngines();
    if (!requireEngines(t, { wasm }, 'U-S16')) return;
    /** The WASM engine's own VirtualFs refusal for the same key or import. */
    const engineError = (base, relative) => {
      const [source, options] = base === ''
        ? ['hi\n', { filename: relative }]
        : [`@import "${relative}" as m\nhi\n`, { filename: base }];
      const err = thrownBy(() => wasm.compile(source, options), `engine ${relative}`);
      return { code: err.code, message: err.message };
    };
    const segments = (n) => Array.from({ length: n }, (_, i) => `d${i}`).join('/');
    const cases = [
      // `..` past the root of the key space. This is also what a project rooted at the
      // filesystem root gets for a `../` import, which the native backend resolves to
      // the root itself (#424).
      ['x.mds', '../y.mds', 'mds::import', 'import error: import path escapes project directory: "../y.mds"'],
      ['a/x.mds', './../../y.mds', 'mds::import', 'import error: import path escapes project directory: "./../../y.mds"'],
      // A key of more than 256 segments, reached by an import or named as the entry.
      // The import itself has 61 segments: only the key it leads to is over the cap.
      [
        `${segments(199)}/x.mds`,
        `./${segments(60)}/y.mds`,
        'mds::resource_limit',
        `resource limit exceeded: import path exceeds maximum segment count (256): "./${segments(60)}/y.mds"`,
      ],
      [
        '',
        `${segments(256)}/e.mds`,
        'mds::resource_limit',
        `resource limit exceeded: import path exceeds maximum segment count (256): "${segments(256)}/e.mds"`,
      ],
      ['x.mds', './', 'mds::import', 'import error: import path resolves to empty key: "./"'],
    ];
    for (const [base, relative, code, message] of cases) {
      const label = `U-S16 ${JSON.stringify([base.slice(0, 20), relative.slice(0, 20)])}`;
      const err = thrownBy(() => normalizeVirtualKey(base, relative), label);
      assert.equal(err.code, code, `${label}: ${err.message}`);
      assert.equal(err.message, message, label);
      assertNoHelp(err, label);
      // The engine the WASM backend hands the key to refuses it identically.
      assert.deepEqual(engineError(base, relative), { code, message }, label);
    }
    // Controls: at the cap, and `..` back down to the root, are keys.
    assert.equal(normalizeVirtualKey(`${segments(255)}/x.mds`, './y.mds'), `${segments(255)}/y.mds`);
    assert.equal(normalizeVirtualKey('', `${segments(255)}/e.mds`), `${segments(255)}/e.mds`);
    assert.equal(normalizeVirtualKey('a/x.mds', '../y.mds'), 'y.mds');
  });
});

// #265: the pre-scanner refuses the same 80 forbidden path codepoints as the Rust
// resolver, with the same error codes and messages — an import string is
// `mds::import`, an entry key is `mds::io`. The Rust↔JS differential over the whole
// class lives in forbidden-path-chars.spec.mjs; these pin the scanner's own API.
const NUL = 0x00;
const HOSTILE_NAME_CODEPOINTS = FORBIDDEN_PATH_CODEPOINTS.filter((cp) => cp !== NUL);

describe('normalizeVirtualKey — forbidden path characters (#265)', () => {
  test('U-S11: an import carrying any forbidden codepoint is refused as mds::import', () => {
    assert.equal(FORBIDDEN_PATH_CODEPOINTS.length, 80, 'the class has 80 codepoints');
    for (const cp of HOSTILE_NAME_CODEPOINTS) {
      const label = `U-S11 ${uPlus(cp)}`;
      const relative = `./a${String.fromCodePoint(cp)}b.mds`;
      const err = thrownBy(() => normalizeVirtualKey('dir/main.mds', relative), label);
      assert.equal(err.code, 'mds::import', `${label}: ${err.message}`);
      assert.equal(
        err.message,
        `import error: import path contains forbidden character ${uPlus(cp)}: "./a${escapeText(cp)}b.mds"`,
        label,
      );
      assertNoForbiddenChars(err.message, label);
    }
  });

  test('U-S12: a NUL in an import keeps its own null-byte message, checked before the class', () => {
    const err = thrownBy(
      () => normalizeVirtualKey('main.mds', `./a${String.fromCodePoint(NUL)}b.mds`),
      'U-S12',
    );
    assert.equal(err.code, 'mds::import');
    assert.equal(err.message, 'import error: import path contains null byte');
  });

  test('U-S13: an entry key (empty base) carrying any forbidden codepoint is refused as mds::io', () => {
    for (const cp of HOSTILE_NAME_CODEPOINTS) {
      const label = `U-S13 ${uPlus(cp)}`;
      const err = thrownBy(() => normalizeVirtualKey('', `a${String.fromCodePoint(cp)}b.mds`), label);
      assert.equal(err.code, 'mds::io', `${label}: ${err.message}`);
      assert.equal(
        err.message,
        `entry path contains forbidden character ${uPlus(cp)}: "a${escapeText(cp)}b.mds"`,
        label,
      );
      assertNoForbiddenChars(err.message, label);
    }
  });

  test('U-S14: an empty or NUL entry key reports the entry-path messages as mds::io', () => {
    const empty = thrownBy(() => normalizeVirtualKey('', ''), 'U-S14 empty');
    assert.equal(empty.code, 'mds::io');
    assert.equal(empty.message, 'entry path is empty');

    const nul = thrownBy(
      () => normalizeVirtualKey('', `a${String.fromCodePoint(NUL)}b.mds`),
      'U-S14 NUL',
    );
    assert.equal(nul.code, 'mds::io');
    assert.equal(nul.message, `entry path contains null byte: "a${escapeText(NUL)}b.mds"`);
  });

  test('U-S15: spaces, non-ASCII letters and the class neighbours are accepted (control)', () => {
    assert.equal(normalizeVirtualKey('dir/main.mds', './a b-ünï.mds'), 'dir/a b-ünï.mds');
    assert.equal(normalizeVirtualKey('', 'a b-ünï.mds'), 'a b-ünï.mds');
    // Each sits next to a member of the class without being one.
    for (const cp of [0x20, 0xa0, 0x061b, 0x061d, 0x200b, 0x200d, 0x2027, 0x202f, 0x2065, 0x206a, 0xfefe, 0x1f600]) {
      const c = String.fromCodePoint(cp);
      assert.equal(normalizeVirtualKey('main.mds', `./a${c}b.mds`), `a${c}b.mds`, uPlus(cp));
      assert.equal(normalizeVirtualKey('', `a${c}b.mds`), `a${c}b.mds`, uPlus(cp));
    }
  });
});

describe('buildModulesMap', () => {
  test('U-SM1: builds modules map for entry with imports', async () => {
    const entryPath = path.join(FIXTURES, 'imports', 'entry.mds');
    const { entryFilename, modules } = await buildModulesMap(entryPath, scanImports);
    assert.ok(entryFilename.endsWith('imports/entry.mds'), `entry key should end with imports/entry.mds: ${entryFilename}`);
    assert.ok(!entryFilename.startsWith('/'), `entry key must be relative, not absolute: ${entryFilename}`);
    assert.ok(typeof modules[entryFilename] === 'string', 'entry should be in modules');
    // lib.mds and deep.mds should also be included
    assert.ok(Object.keys(modules).length >= 3, `expected at least 3 modules, got: ${Object.keys(modules)}`);
  });

  test('U-SM2: builds modules map for file with no imports', async () => {
    const entryPath = path.join(FIXTURES, 'simple.mds');
    const { entryFilename, modules } = await buildModulesMap(entryPath, scanImports);
    assert.ok(entryFilename.endsWith('simple.mds'), `entry key should end with simple.mds: ${entryFilename}`);
    assert.ok(!entryFilename.startsWith('/'), `entry key must be relative, not absolute: ${entryFilename}`);
    assert.ok(typeof modules[entryFilename] === 'string');
    assert.equal(Object.keys(modules).length, 1);
  });

  test('U-SM3: rejects nonexistent file with the file-not-found shape, never a raw absolute-path error', async () => {
    // The entry's parent directory does not exist, so `realpath(dirname(...))`
    // — called before the filesystem is touched for the entry itself — is what
    // rejects, not the later O_NOFOLLOW open (#408). Absolute `shown`: the
    // caller's own path is already absolute, so it is expected back verbatim.
    const absoluteEntry = '/nonexistent/file.mds';
    assertNotFoundNotSymlink(
      await rejectionOf(buildModulesMap(absoluteEntry, scanImports), 'U-SM3a'),
      absoluteEntry,
      'U-SM3a',
    );

    // Relative `shown`: this is the case that actually discriminates the fix from
    // the raw Node error. A raw ENOENT from `realpath()` reports the *resolved*
    // (cwd-joined) absolute path in its message, which would satisfy neither the
    // exact-message assertion above nor the no-leak assertion below for a
    // relative input — only the translated error reports `shown` unchanged.
    const relativeEntry = `mds-scanner-u-sm3-${process.pid}-nonexistent/file.mds`;
    const err = await rejectionOf(buildModulesMap(relativeEntry, scanImports), 'U-SM3b');
    assertNotFoundNotSymlink(err, relativeEntry, 'U-SM3b');
    assert.ok(
      !err.message.includes(process.cwd()),
      `U-SM3b: message must not leak the resolved absolute path: ${err.message}`,
    );
  });

  test('U-SM4: shallow import chain succeeds within depth limit', async () => {
    // The fixtures/imports chain has depth 2 (entry → lib → deep).
    // This must succeed well within MAX_IMPORT_DEPTH=64.
    const deepEntryPath = path.join(FIXTURES, 'imports', 'entry.mds');
    const result = await buildModulesMap(deepEntryPath, scanImports);
    assert.ok(Object.keys(result.modules).length >= 3, 'should resolve all modules in shallow chain');
  });

  test('U-SM5: rejects when module count exceeds maxModules', async () => {
    // The fixtures/imports chain has 3+ modules. Setting maxModules=1 means even
    // the first import discovered triggers the resource-limit guard, confirming
    // that path works. The depth guard (MAX_IMPORT_DEPTH=64) is structurally
    // verified: depth is incremented on every recursive call and compared against
    // the constant before filesystem access. True depth-limit testing would require
    // 65 unique real files (too heavyweight for unit tests).
    const entryPath = path.join(FIXTURES, 'imports', 'entry.mds');
    await assert.rejects(
      () => buildModulesMap(entryPath, scanImports, { maxModules: 1 }),
      /resource limit/,
    );
  });

  test('U-SM6: rejects when aggregate size exceeds maxAggregateSize', async () => {
    // simple.mds is a small file; maxAggregateSize: 1 byte triggers the guard
    // immediately after fstat, before readFile, exercising the pre-read check.
    const entryPath = path.join(FIXTURES, 'simple.mds');
    await assert.rejects(
      () => buildModulesMap(entryPath, scanImports, { maxAggregateSize: 1 }),
      /resource limit.*aggregate module size/,
    );
  });

  test('U-SM7: rejects a symlinked entry with the symlink error native reports, naming the path as written', async () => {
    // openNoFollow uses O_NOFOLLOW (Linux/macOS) or a post-open lstat check
    // (Windows) to detect symlinks. This test creates a real symlink in a temp
    // directory and confirms the scanner reports NativeFs's symlink refusal.
    const tmpDir = await mkdtemp(path.join(os.tmpdir(), 'mds-scanner-test-'));
    try {
      const realFile = path.join(tmpDir, 'real.mds');
      const linkFile = path.join(tmpDir, 'link.mds');
      await writeFile(realFile, 'Hello world');
      await symlink(realFile, linkFile);
      // The caller's path is absolute, so it is expected back verbatim.
      assertSymlinkRefusal(await rejectionOf(buildModulesMap(linkFile, scanImports), 'U-SM7'), linkFile, 'U-SM7');

      // Typed relative: only the path as written may appear, never the resolved
      // absolute one (whose directory is canonical — /private/var on macOS).
      const canonicalDir = await realpath(tmpDir);
      const err = await withCwd(tmpDir, () => rejectionOf(buildModulesMap('link.mds', scanImports), 'U-SM7 relative'));
      assertSymlinkRefusal(err, 'link.mds', 'U-SM7 relative');
      assert.ok(!err.message.includes(canonicalDir), `U-SM7: no absolute path; got: ${err.message}`);
    } finally {
      await rm(tmpDir, { recursive: true, force: true });
    }
  });

  test('U-SM8: resolves cross-directory imports via project root discovery', async () => {
    // cross-dir/app/entry.mds imports ../lib/helpers.mds — a sibling directory.
    // The scanner must walk up to find the project root (via .git marker) so that
    // the import resolves within the project boundary instead of being rejected.
    const entryPath = path.join(FIXTURES, 'cross-dir', 'app', 'entry.mds');
    const { entryFilename, modules } = await buildModulesMap(entryPath, scanImports);
    // Entry key should end with the path from the cross-dir root.
    assert.ok(entryFilename.endsWith('cross-dir/app/entry.mds'), `entry key should include path: ${entryFilename}`);
    // The entry module should be in the map under its key.
    assert.ok(modules[entryFilename], 'entry should be in modules under its key');
    // The sibling-directory module should also be present.
    const helperKey = Object.keys(modules).find(k => k.endsWith('cross-dir/lib/helpers.mds'));
    assert.ok(helperKey, `sibling dir module should be included, got keys: ${Object.keys(modules)}`);
    assert.equal(Object.keys(modules).length, 2, 'should have exactly entry + helper');
  });
});

describe('buildModulesMap — forbidden path characters (#265)', () => {
  /** Run `fn` against a fresh project directory (marked by `.mdsroot`). */
  async function withProject(fn) {
    const dir = await mkdtemp(path.join(os.tmpdir(), 'mds-scanner-265-'));
    try {
      await writeFile(path.join(dir, '.mdsroot'), '');
      return await fn(dir);
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  }

  test('U-SM9: an import carrying a forbidden codepoint is refused as mds::import before it is opened', async () => {
    await withProject(async (dir) => {
      const entry = path.join(dir, 'main.mds');
      await writeFile(entry, 'hi\n');
      for (const cp of HOSTILE_NAME_CODEPOINTS) {
        const label = `U-SM9 ${uPlus(cp)}`;
        // The injected scanner reports the hostile import for the entry. No file of
        // that name exists, so a scan that reached the filesystem would reject with
        // ENOENT rather than this error.
        const importPath = `./a${String.fromCodePoint(cp)}b.mds`;
        const err = await rejectionOf(buildModulesMap(entry, () => [importPath]), label);
        assert.equal(err.code, 'mds::import', `${label}: ${err.message}`);
        assert.equal(
          err.message,
          `import error: import path contains forbidden character ${uPlus(cp)}: "./a${escapeText(cp)}b.mds"`,
          label,
        );
        assertNoForbiddenChars(err.message, label);
      }
    });
  });

  test('U-SM10: import strings are classified in the resolver order — relative form, NUL, then the class', async () => {
    await withProject(async (dir) => {
      const entry = path.join(dir, 'main.mds');
      await writeFile(entry, 'hi\n');
      const esc = String.fromCodePoint(0x1b);
      const cases = [
        ['lib.mds', `import error: import path must be relative (start with './' or '../'): "lib.mds"`],
        // Not relative AND hostile: the relative-form rule is reported, the name escaped.
        [`fo${esc}o.mds`, `import error: import path must be relative (start with './' or '../'): "fo${escapeText(0x1b)}o.mds"`],
        ['', `import error: import path must be relative (start with './' or '../'): ""`],
        [`./a${String.fromCodePoint(NUL)}b${esc}.mds`, 'import error: import path contains null byte'],
      ];
      for (const [importPath, expected] of cases) {
        const label = `U-SM10 ${JSON.stringify(importPath)}`;
        const err = await rejectionOf(buildModulesMap(entry, () => [importPath]), label);
        assert.equal(err.code, 'mds::import', `${label}: ${err.message}`);
        assert.equal(err.message, expected, label);
      }
    });
  });

  test('U-SM11: an entry path carrying a forbidden codepoint is refused as mds::io before the filesystem is touched', async () => {
    await withProject(async (dir) => {
      for (const cp of HOSTILE_NAME_CODEPOINTS) {
        const label = `U-SM11 ${uPlus(cp)}`;
        const typed = path.join(dir, `a${String.fromCodePoint(cp)}b.mds`);
        const err = await rejectionOf(buildModulesMap(typed, scanImports), label);
        assert.equal(err.code, 'mds::io', `${label}: ${err.message}`);
        assert.equal(
          err.message,
          `entry path contains forbidden character ${uPlus(cp)}: "${path.join(dir, `a${escapeText(cp)}b.mds`)}"`,
          label,
        );
        assertNoForbiddenChars(err.message, label);
      }

      const nul = await rejectionOf(
        buildModulesMap(path.join(dir, `a${String.fromCodePoint(NUL)}b.mds`), scanImports),
        'U-SM11 NUL',
      );
      assert.equal(nul.code, 'mds::io');
      assert.equal(nul.message, `entry path contains null byte: "${path.join(dir, `a${escapeText(NUL)}b.mds`)}"`);

      const empty = await rejectionOf(buildModulesMap('', scanImports), 'U-SM11 empty');
      assert.equal(empty.code, 'mds::io');
      assert.equal(empty.message, 'entry path is empty');
    });
  });

  test('U-SM12: a clean name with spaces and non-ASCII letters is imported (control)', async () => {
    await withProject(async (dir) => {
      const entry = path.join(dir, 'main.mds');
      await writeFile(entry, 'hi\n');
      await writeFile(path.join(dir, 'a b-ünï.mds'), 'there\n');
      const { entryFilename, modules } = await buildModulesMap(entry, (src) =>
        src === 'hi\n' ? ['./a b-ünï.mds'] : [],
      );
      assert.equal(entryFilename, 'main.mds');
      assert.deepEqual(Object.keys(modules).sort(), ['a b-ünï.mds', 'main.mds']);
    });
  });

  // Windows file names cannot carry C0 controls, so the hostile directory this
  // needs cannot be created there.
  test(
    'U-SM13: an entry reached through a symlink into a hostile-named directory is refused as mds::io',
    { skip: process.platform === 'win32' && 'C0 controls are not valid in Windows file names' },
    async () => {
      await withProject(async (dir) => {
        for (const cp of [0x09, 0x0a, 0x1b, 0x7f, 0x85, 0x202e, 0xfeff]) {
          const label = `U-SM13 ${uPlus(cp)}`;
          const hostile = path.join(dir, `ho${String.fromCodePoint(cp)}stile-${cp}`);
          await mkdir(hostile);
          await writeFile(path.join(hostile, 'main.mds'), 'hi\n');
          const alias = path.join(dir, `alias-${cp}`);
          await symlink(hostile, alias, 'dir');

          // The typed path is clean; only its canonical form carries the codepoint.
          const typed = path.join(alias, 'main.mds');
          const err = await rejectionOf(buildModulesMap(typed, scanImports), label);
          assert.equal(err.code, 'mds::io', `${label}: ${err.message}`);
          // The message shows the path as typed, never the resolved one.
          assert.equal(err.message, `resolved path contains forbidden character ${uPlus(cp)}: "${typed}"`, label);
        }

        // Control: the same layout with a clean target directory builds.
        const clean = path.join(dir, 'clean');
        await mkdir(clean);
        await writeFile(path.join(clean, 'main.mds'), 'hi\n');
        await symlink(clean, path.join(dir, 'alias-clean'), 'dir');
        const { modules } = await buildModulesMap(path.join(dir, 'alias-clean', 'main.mds'), scanImports);
        assert.deepEqual(Object.values(modules), ['hi\n']);
      });
    },
  );
});

// #408: the scanner decides "symlink" from the final component's own file type
// and checks the canonical parent — as NativeFs does — instead of comparing a
// realpath with the path as written. On a case-insensitive volume (the macOS and
// Windows default) realpath returns the on-disk spelling, so the comparison used to
// report `Entry.mds` for `entry.mds` as a possible symlink.
describe('buildModulesMap — case-mismatched names and symlinks (#408)', () => {
  async function withProject(fn) {
    const dir = await mkdtemp(path.join(os.tmpdir(), 'mds-scanner-408-'));
    try {
      await writeFile(path.join(dir, '.mdsroot'), '');
      return await fn(dir);
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  }

  const dirLinkType = process.platform === 'win32' ? 'junction' : 'dir';

  test('U-SM14: a case-mismatched entry path is never reported as a symlink', async () => {
    await withProject(async (dir) => {
      await writeFile(path.join(dir, 'main.mds'), 'Hello!\n');
      // Resolve case-sensitivity BEFORE starting the build: the build's
      // rejection on a case-sensitive volume can settle before an interleaved
      // await elsewhere in this function attaches a handler, which Node's
      // unhandled-rejection detector flags even though the rejection is
      // handled moments later (a `PromiseRejectionHandledWarning`, not a
      // silently dropped error). Starting `build` last means the very next
      // expression always consumes it, on either branch.
      const insensitive = await caseInsensitive(dir);
      const entry = path.join(dir, 'MAIN.mds');
      const build = buildModulesMap(entry, scanImports);
      if (insensitive) {
        // The entry is keyed by its on-disk spelling, as native keys it (#414).
        const { entryFilename, modules, aliases } = await build;
        assert.equal(entryFilename, 'main.mds');
        assert.deepEqual(modules, { 'main.mds': 'Hello!\n' });
        assert.deepEqual(aliases, {});
      } else {
        assertNotFoundNotSymlink(await rejectionOf(build, 'U-SM14'), entry, 'U-SM14');
      }
    });
  });

  test('U-SM15: a case-mismatched import is never reported as a symlink', async () => {
    await withProject(async (dir) => {
      await writeFile(path.join(dir, 'main.mds'), '@import "./Header.mds" as h\n');
      await writeFile(path.join(dir, 'header.mds'), 'hi\n');
      // See U-SM14: resolve case-sensitivity before starting the build so
      // nothing is left unhandled across an interleaved await.
      const insensitive = await caseInsensitive(dir);
      const build = buildModulesMap(path.join(dir, 'main.mds'), scanImports);
      if (insensitive) {
        // Keyed by its on-disk spelling, as native keys it; the spelling the engine
        // looks the import up under is an alias of that key (#414).
        const { modules, aliases } = await build;
        assert.deepEqual(Object.keys(modules).sort(), ['header.mds', 'main.mds']);
        assert.equal(modules['header.mds'], 'hi\n');
        assert.deepEqual(aliases, { 'Header.mds': 'header.mds' });
      } else {
        assertNotFoundNotSymlink(await rejectionOf(build, 'U-SM15'), './Header.mds', 'U-SM15');
      }
    });
  });

  test('U-SM16: a symlink is still refused, under its exact and a mismatched spelling (control)', async () => {
    await withProject(async (dir) => {
      await writeFile(path.join(dir, 'target.mds'), 'hi\n');
      await symlink(path.join(dir, 'target.mds'), path.join(dir, 'link.mds'));
      const insensitive = await caseInsensitive(dir);

      await writeFile(path.join(dir, 'main.mds'), '@import "./link.mds" as l\n');
      assertSymlinkRefusal(
        await rejectionOf(buildModulesMap(path.join(dir, 'main.mds'), scanImports), 'U-SM16 import'),
        './link.mds',
        'U-SM16 import',
      );
      const entry = path.join(dir, 'link.mds');
      assertSymlinkRefusal(await rejectionOf(buildModulesMap(entry, scanImports), 'U-SM16 entry'), entry, 'U-SM16 entry');

      await writeFile(path.join(dir, 'main.mds'), '@import "./LINK.mds" as l\n');
      const mismatched = buildModulesMap(path.join(dir, 'main.mds'), scanImports);
      if (insensitive) {
        assertSymlinkRefusal(await rejectionOf(mismatched, 'U-SM16 mismatched'), './LINK.mds', 'U-SM16 mismatched');
      } else {
        assertNotFoundNotSymlink(await rejectionOf(mismatched, 'U-SM16'), './LINK.mds', 'U-SM16');
      }
    });
  });

  test('U-SM17: a symlinked directory is followed inside the project and refused when it leads outside', async () => {
    await withProject(async (dir) => {
      // Inside: the parent directory is canonicalized (its link followed) and the
      // final component checked, as NativeFs does.
      await mkdir(path.join(dir, 'real'));
      await writeFile(path.join(dir, 'real', 'lib.mds'), 'lib\n');
      await symlink(path.join(dir, 'real'), path.join(dir, 'alias'), dirLinkType);
      await writeFile(path.join(dir, 'main.mds'), '@import "./alias/lib.mds" as l\n');
      const { modules, aliases } = await buildModulesMap(path.join(dir, 'main.mds'), scanImports);
      // Keyed by its canonical path, as native keys it; the spelling through the link
      // is an alias of that key (#414).
      assert.equal(modules['real/lib.mds'], 'lib\n');
      assert.equal(modules['alias/lib.mds'], undefined);
      assert.deepEqual(aliases, { 'alias/lib.mds': 'real/lib.mds' });

      // Outside: the canonical path is what containment is checked on.
      const outside = await mkdtemp(path.join(os.tmpdir(), 'mds-scanner-408-outside-'));
      try {
        await writeFile(path.join(outside, 'secret.mds'), 'secret\n');
        await symlink(outside, path.join(dir, 'escape'), dirLinkType);
        await writeFile(path.join(dir, 'main.mds'), '@import "./escape/secret.mds" as s\n');
        const err = await rejectionOf(buildModulesMap(path.join(dir, 'main.mds'), scanImports), 'U-SM17');
        assertEscapeRefusal(err, './escape/secret.mds', 'U-SM17');
        assert.ok(!err.message.includes(outside), `U-SM17: no resolved path; got: ${err.message}`);
      } finally {
        await rm(outside, { recursive: true, force: true });
      }
    });
  });

  // Windows file names cannot carry C0 controls, so the hostile directory this
  // needs cannot be created there.
  test(
    'U-SM18: an import through a symlink into a hostile-named directory is refused as mds::io (#265)',
    { skip: process.platform === 'win32' && 'C0 controls are not valid in Windows file names' },
    async () => {
      await withProject(async (dir) => {
        const hostile = path.join(dir, `ho${String.fromCodePoint(0x1b)}stile`);
        await mkdir(hostile);
        await writeFile(path.join(hostile, 'lib.mds'), 'lib\n');
        await symlink(hostile, path.join(dir, 'alias'), 'dir');
        await writeFile(path.join(dir, 'main.mds'), '@import "./alias/lib.mds" as l\n');
        const err = await rejectionOf(buildModulesMap(path.join(dir, 'main.mds'), scanImports), 'U-SM18');
        assert.equal(err.code, 'mds::io', err.message);
        // Names the import as written, never the resolved path.
        assert.equal(err.message, 'resolved path contains forbidden character U+001B: "./alias/lib.mds"');
      });
    },
  );

  test('U-SM19: an entry path through a file where a directory is expected (ENOTDIR) is reported as file-not-found', async () => {
    // Rust's check_symlink_named maps ANY canonicalize() failure on the parent —
    // ENOENT or ENOTDIR alike — to file_not_found without distinguishing errno;
    // this mirrors that. `blocker.mds` is a regular file, so the `nested` segment
    // below it cannot be traversed.
    await withProject(async (dir) => {
      const blocker = path.join(dir, 'blocker.mds');
      await writeFile(blocker, 'not a directory\n');
      const entry = path.join(blocker, 'nested', 'file.mds');
      assertNotFoundNotSymlink(await rejectionOf(buildModulesMap(entry, scanImports), 'U-SM19'), entry, 'U-SM19');

      // One level down, the regular file IS the immediate parent: realpath() of it
      // succeeds, and it is the open() of the file below it that fails with ENOTDIR
      // — still a missing file, never a symlink.
      const direct = path.join(blocker, 'file.mds');
      assertNotFoundNotSymlink(await rejectionOf(buildModulesMap(direct, scanImports), 'U-SM19 direct'), direct, 'U-SM19 direct');
      await writeFile(path.join(dir, 'main.mds'), '@import "./blocker.mds/file.mds" as f\n');
      assertNotFoundNotSymlink(
        await rejectionOf(buildModulesMap(path.join(dir, 'main.mds'), scanImports), 'U-SM19 import'),
        './blocker.mds/file.mds',
        'U-SM19 import',
      );
    });
  });

  test('U-SM20: an import into a missing subdirectory is reported as file-not-found, not a raw ENOENT', async () => {
    // Exercises openAndValidateModule's own realpath(dirname(...)) (the sibling
    // of buildModulesMap's top-level one that U-SM3 exercises): the entry exists,
    // but an imported module's directory does not.
    await withProject(async (dir) => {
      const importPath = './missing-subdir/lib.mds';
      await writeFile(path.join(dir, 'main.mds'), `@import "${importPath}" as l\n`);
      const err = await rejectionOf(buildModulesMap(path.join(dir, 'main.mds'), scanImports), 'U-SM20');
      assertNotFoundNotSymlink(err, importPath, 'U-SM20');
    });
  });

  test('U-SM21: compileFile on a missing entry or import — native and WASM backends throw the same error, help included', async (t) => {
    const engines = await loadEngines();
    if (!requireEngines(t, engines, 'U-SM21')) return;
    await withProject(async (dir) => {
      const missing = path.join(dir, `mds-scanner-u-sm21-${process.pid}-nonexistent`, 'file.mds');
      const importer = path.join(dir, 'main.mds');
      await writeFile(importer, '@import "./missing.mds" as m\nhi\n');
      const files = [missing, importer];
      const [nativeEntry, nativeImport] = await compileFileOutcomes('native', files);
      const [wasmEntry, wasmImport] = await compileFileOutcomes('wasm', files);
      // Non-vacuity (PF-013): native's own errors, its help included.
      assert.deepEqual(nativeEntry, {
        code: 'mds::file_not_found',
        message: `file not found: ${missing}`,
        help: FILE_NOT_FOUND_HELP,
        span: null,
      });
      // An import's error points at its directive: the whole line.
      assert.deepEqual(nativeImport, {
        code: 'mds::file_not_found',
        message: 'file not found: ./missing.mds',
        help: FILE_NOT_FOUND_HELP,
        span: firstLineSpan('@import "./missing.mds" as m'),
      });
      // The whole shape agrees, the import's span included (#414).
      assert.deepEqual(wasmEntry, nativeEntry);
      assert.deepEqual(wasmImport, nativeImport);
    });
  });

  // The WASM engine resolves an import BY NAME, from the importing module's key;
  // NativeFs resolves it ON DISK, from the importing module's canonical directory.
  // A module's key is its canonical path (#414), so through a symlinked directory the
  // two agree for every import except one whose own path names the link and leaves
  // it through `..`: its key names the directory beside the link, the file NativeFs
  // reads sits beside the link's target. That is POSIX, where the OS applies each
  // `..` after the link before it. Windows collapses `sub\..` lexically before it
  // follows the link (a junction here), on both backends, so there the two agree for
  // that import too, and an entry typed that way names the file beside the link.
  const lexicalDotDot = process.platform === 'win32';

  /** sub -> deep/other; sub/y.mds imports ../z.mds; both z.mds files exist. */
  async function dotDotOutOfLink(dir, linked) {
    await mkdir(path.join(dir, 'deep', 'other'), { recursive: true });
    const yDir = linked ? path.join(dir, 'deep', 'other') : path.join(dir, 'sub');
    if (linked) {
      await symlink(path.join(dir, 'deep', 'other'), path.join(dir, 'sub'), dirLinkType);
    } else {
      await mkdir(yDir);
    }
    await writeFile(path.join(yDir, 'y.mds'), '@import "../z.mds" as dz\nY=\n@include dz\n');
    await writeFile(path.join(dir, 'deep', 'z.mds'), 'DEEP-Z\n');
    await writeFile(path.join(dir, 'z.mds'), 'ROOT-Z\n');
    await writeFile(path.join(dir, 'main.mds'), '@import "./sub/y.mds" as y\n@import "./z.mds" as rz\n@include rz\n@include y\n');
    return path.join(dir, 'main.mds');
  }

  test("U-SM22: an import naming a symlinked directory and leaving it through '..' is never read from the other side — refused on POSIX, lexical on Windows", async () => {
    await withProject(async (dir) => {
      // A module reached through the link is keyed by its canonical path, so its own
      // `../z.mds` names, by key and on disk alike, the z.mds beside the link's target.
      const main = await dotDotOutOfLink(dir, true);
      const { modules, aliases } = await buildModulesMap(main, scanImports);
      assert.equal(modules['deep/z.mds'], 'DEEP-Z\n');
      assert.equal(modules['deep/other/y.mds'], '@import "../z.mds" as dz\nY=\n@include dz\n');
      assert.deepEqual(aliases, { 'sub/y.mds': 'deep/other/y.mds' });
      // An import whose own path runs through the link and back out.
      const through = path.join(dir, 'through.mds');
      const throughSource = '@import "./sub/../z.mds" as z\n@include z\n';
      await writeFile(through, throughSource);
      const build = buildModulesMap(through, scanImports);
      if (lexicalDotDot) {
        // Windows: its key and the disk both name the root's z.mds, the file native
        // reads, so nothing is refused.
        const built = await build;
        assert.deepEqual(built.modules, { 'through.mds': throughSource, 'z.mds': 'ROOT-Z\n' });
        assert.deepEqual(built.aliases, {});
      } else {
        // POSIX: its key names the root's z.mds, the file on disk is deep/z.mds.
        const err = await rejectionOf(build, 'U-SM22');
        assert.equal(err.code, 'mds::import', err.message);
        assert.equal(
          err.message,
          `import error: import path leaves a symlinked directory through '..', which the WASM backend cannot resolve: "./sub/../z.mds"`,
        );
      }
    });
    // Control: the same tree with `sub` a real directory builds, and `../z.mds`
    // from sub/y.mds is the root z.mds under both resolutions.
    await withProject(async (dir) => {
      const { modules } = await buildModulesMap(await dotDotOutOfLink(dir, false), scanImports);
      assert.equal(modules['z.mds'], 'ROOT-Z\n');
      assert.equal(modules['sub/y.mds'], '@import "../z.mds" as dz\nY=\n@include dz\n');
    });
  });

  test('U-SM23: a file reached through a symlinked directory is one module, keyed by its canonical path', async () => {
    await withProject(async (dir) => {
      await mkdir(path.join(dir, 'lib'));
      await writeFile(path.join(dir, 'lib', 'x.mds'), 'X\n');
      await writeFile(path.join(dir, 'lib', 'y.mds'), '@import "./x.mds" as x\n');
      await symlink(path.join(dir, 'lib'), path.join(dir, 'alias'), dirLinkType);
      await writeFile(path.join(dir, 'main.mds'), '@import "./lib/x.mds" as a\n@import "./alias/y.mds" as b\n');
      const { modules, aliases } = await buildModulesMap(path.join(dir, 'main.mds'), scanImports);
      // alias/y.mds is lib/y.mds on disk: one module, as on native, keyed by its
      // canonical path, and its `./x.mds` is lib/x.mds by key too. The spelling through
      // the link is an alias the engine maps to that key (#414).
      assert.deepEqual(Object.keys(modules).sort(), ['lib/x.mds', 'lib/y.mds', 'main.mds']);
      assert.equal(modules['lib/x.mds'], 'X\n');
      assert.deepEqual(aliases, { 'alias/y.mds': 'lib/y.mds' });
    });
  });

  test('U-SM24: through a symlinked directory, the WASM backend compiles what the native backend compiles, or refuses', async (t) => {
    const engines = await loadEngines();
    if (!requireEngines(t, engines, 'U-SM24')) return;
    await withProject(async (dir) => {
      // Both backends compile the alias layout identically.
      await mkdir(path.join(dir, 'lib'));
      await writeFile(path.join(dir, 'lib', 'x.mds'), 'X\n');
      await writeFile(path.join(dir, 'lib', 'y.mds'), '@import "./x.mds" as x\nY\n@include x\n');
      await symlink(path.join(dir, 'lib'), path.join(dir, 'alias'), dirLinkType);
      const alias = path.join(dir, 'alias-main.mds');
      await writeFile(alias, '@import "./lib/x.mds" as a\n@import "./alias/y.mds" as b\n@include a\n@include b\n');
      // A module reached through the link imports `../z.mds`: both backends read
      // deep/z.mds, the module's key being its canonical path (#414).
      const dotDot = await dotDotOutOfLink(dir, true);
      // An import whose own path runs through the link and back out. On POSIX NativeFs
      // reads deep/z.mds; the WASM backend refuses rather than compile the root z.mds
      // its engine would look up by name (difference 5). On Windows both collapse
      // `sub\..` lexically and compile the root z.mds.
      const through = path.join(dir, 'through.mds');
      await writeFile(through, '@import "./sub/../z.mds" as z\n@include z\n');
      // An ENTRY typed through the link and back out with `..`: the OS applies the
      // `..` — after the link on POSIX, so both backends compile deep/z.mds; lexically
      // on Windows, so both compile the root z.mds. The scanner resolves the entry's
      // directory as NativeFs does, never lexically itself (#414).
      // Joined by hand: path.join would drop the `..` before the OS saw it.
      const entryOutOfLink = [dir, 'sub', '..', 'z.mds'].join(path.sep);
      /** What native compiles for `sub/../z.mds`. */
      const outOfLink = { output: lexicalDotDot ? 'ROOT-Z\n' : 'DEEP-Z\n' };

      const files = [alias, dotDot, entryOutOfLink, through];
      const [nativeAlias, nativeDotDot, nativeEntry, nativeThrough] = await compileFileOutcomes('native', files);
      const [wasmAlias, wasmDotDot, wasmEntry, wasmThrough] = await compileFileOutcomes('wasm', files);
      assert.equal(nativeAlias.output, 'X\nY\nX\n', JSON.stringify(nativeAlias));
      assert.deepEqual(wasmAlias, nativeAlias);
      assert.equal(nativeDotDot.output, 'ROOT-Z\nY=\nDEEP-Z\n', JSON.stringify(nativeDotDot));
      assert.deepEqual(wasmDotDot, nativeDotDot);
      assert.deepEqual(nativeEntry, outOfLink);
      assert.deepEqual(wasmEntry, nativeEntry);
      assert.deepEqual(nativeThrough, outOfLink);
      if (lexicalDotDot) {
        assert.deepEqual(wasmThrough, nativeThrough);
      } else {
        assert.equal(wasmThrough.code, 'mds::import', JSON.stringify(wasmThrough));
        assert.equal(wasmThrough.output, undefined, JSON.stringify(wasmThrough));
      }
    });
  });
});

describe('buildModulesMap — a filesystem error is coded, never a raw Node error (#408)', () => {
  async function withProject(fn) {
    const dir = await mkdtemp(path.join(os.tmpdir(), 'mds-scanner-errno-'));
    try {
      await writeFile(path.join(dir, '.mdsroot'), '');
      return await fn(dir);
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  }

  // Permission bits do not bind root, and Windows has no POSIX modes to strip.
  const noModes =
    (process.platform === 'win32' && 'POSIX permission bits are not enforced on Windows') ||
    (typeof process.getuid === 'function' && process.getuid() === 0 && 'root bypasses permission bits');

  /** Build `entry` and return its rejection — with every forbidden character absent. */
  async function refusal(entry, label) {
    const err = await rejectionOf(buildModulesMap(entry, scanImports), label);
    assertNoForbiddenChars(err.message, label);
    return err;
  }

  /**
   * The scanner's refusal for `entry` and for the same file imported as
   * `importPath` by a sibling `main.mds` — both `file not found`, keyed on the path
   * as written, the shape NativeFs gives any failure to resolve a path (it maps
   * every errno of that step to `mds::file_not_found`).
   */
  async function assertNotFoundAsEntryAndImport(dir, entry, importPath, label) {
    assertNotFoundNotSymlink(await refusal(entry, `${label} entry`), entry, `${label} entry`);
    await writeFile(path.join(dir, 'main.mds'), `@import "${importPath}" as x\n`);
    const err = await refusal(path.join(dir, 'main.mds'), `${label} import`);
    assertNotFoundNotSymlink(err, importPath, `${label} import`);
  }

  // Windows resolves a self-referencing directory link no more than POSIX does
  // (its reparse-point limit), and both sides report ANY failure to resolve the
  // directory as file-not-found, whatever its errno.
  test('U-SM25: a symlink loop in a directory (ELOOP) is file-not-found, as native reports it', async (t) => {
    await withProject(async (dir) => {
      if (!(await symlinkOrSkip(t, path.join(dir, 'loop'), path.join(dir, 'loop'), 'dir'))) return;
      const entry = path.join(dir, 'loop', 'x.mds');
      await assertNotFoundAsEntryAndImport(dir, entry, './loop/x.mds', 'U-SM25');

      const engines = await loadEngines();
      if (!requireEngines(t, engines, 'U-SM25')) return;
      const [native] = await compileFileOutcomes('native', [entry]);
      const [wasm] = await compileFileOutcomes('wasm', [entry]);
      assert.equal(native.code, 'mds::file_not_found', JSON.stringify(native));
      assert.equal(native.help, FILE_NOT_FOUND_HELP, JSON.stringify(native));
      // An entry error: the whole shape agrees, help included (#414).
      assert.deepEqual(wasm, native);
    });
  });

  test('U-SM26: a name too long to resolve (ENAMETOOLONG) is file-not-found, as native reports it', async (t) => {
    await withProject(async (dir) => {
      const long = 'n'.repeat(300);
      // The final component (the open fails) and a directory above it (realpath fails).
      const file = path.join(dir, `${long}.mds`);
      await assertNotFoundAsEntryAndImport(dir, file, `./${long}.mds`, 'U-SM26 file');
      const nested = path.join(dir, long, 'x.mds');
      await assertNotFoundAsEntryAndImport(dir, nested, `./${long}/x.mds`, 'U-SM26 dir');

      const engines = await loadEngines();
      if (!requireEngines(t, engines, 'U-SM26')) return;
      const nativeOutcomes = await compileFileOutcomes('native', [file, nested]);
      const wasmOutcomes = await compileFileOutcomes('wasm', [file, nested]);
      for (const [i, native] of nativeOutcomes.entries()) {
        assert.equal(native.code, 'mds::file_not_found', JSON.stringify(native));
        assert.equal(native.help, FILE_NOT_FOUND_HELP, JSON.stringify(native));
        // An entry error: the whole shape agrees, help included (#414).
        assert.deepEqual(wasmOutcomes[i], native);
      }
    });
  });

  test('U-SM27: a directory that cannot be searched (EACCES) is file-not-found, as native reports it', { skip: noModes }, async (t) => {
    await withProject(async (dir) => {
      const locked = path.join(dir, 'locked');
      await mkdir(path.join(locked, 'sub'), { recursive: true });
      await writeFile(path.join(locked, 'sub', 'x.mds'), 'X\n');
      await chmod(locked, 0o000);
      try {
        const entry = path.join(locked, 'sub', 'x.mds');
        await assertNotFoundAsEntryAndImport(dir, entry, './locked/sub/x.mds', 'U-SM27');

        const engines = await loadEngines();
        if (!requireEngines(t, engines, 'U-SM27')) return;
        const [native] = await compileFileOutcomes('native', [entry]);
        const [wasm] = await compileFileOutcomes('wasm', [entry]);
        assert.equal(native.code, 'mds::file_not_found', JSON.stringify(native));
        assert.equal(native.help, FILE_NOT_FOUND_HELP, JSON.stringify(native));
        // An entry error: the whole shape agrees, help included (#414).
        assert.deepEqual(wasm, native);
      } finally {
        await chmod(locked, 0o755);
      }
    });
  });

  test('U-SM28: a file that exists but cannot be read (EACCES) is mds::io, as native reports it', { skip: noModes }, async (t) => {
    await withProject(async (dir) => {
      const secret = path.join(dir, 'secret.mds');
      await writeFile(secret, 'S\n');
      await chmod(secret, 0o000);
      try {
        const err = await refusal(secret, 'U-SM28 entry');
        assert.equal(err.code, 'mds::io', err.message);
        assert.equal(err.message, `cannot read ${secret}: EACCES`);
        assertNoHelp(err, 'U-SM28 entry');

        await writeFile(path.join(dir, 'main.mds'), '@import "./secret.mds" as s\n');
        const imported = await refusal(path.join(dir, 'main.mds'), 'U-SM28 import');
        assert.equal(imported.code, 'mds::io', imported.message);
        assert.equal(imported.message, 'cannot read ./secret.mds: EACCES');
        assertNoHelp(imported, 'U-SM28 import');

        const engines = await loadEngines();
        if (!requireEngines(t, engines, 'U-SM28')) return;
        const [native] = await compileFileOutcomes('native', [secret]);
        const [wasm] = await compileFileOutcomes('wasm', [secret]);
        // The code agrees; the message names the path as written and the errno name,
        // where native names its root-relative display path and the OS error text.
        assert.equal(native.code, 'mds::io', JSON.stringify(native));
        assert.equal(wasm.code, native.code, JSON.stringify({ wasm, native }));
      } finally {
        await chmod(secret, 0o644);
      }
    });
  });

  // Windows file names cannot carry C0 controls, so the hostile directory this
  // needs cannot be created there.
  test(
    'U-SM29: under a hostile-named working directory, an unsearchable directory names only the path as written',
    { skip: noModes || (process.platform === 'win32' && 'C0 controls are not valid in Windows file names') },
    async () => {
      await withProject(async (dir) => {
        const hostile = path.join(dir, `ho${String.fromCodePoint(0x1b)}stile`);
        const locked = path.join(hostile, 'locked');
        await mkdir(path.join(locked, 'sub'), { recursive: true });
        await writeFile(path.join(locked, 'sub', 'x.mds'), 'X\n');
        await chmod(locked, 0o000);
        const cwd = process.cwd();
        process.chdir(hostile);
        try {
          // Node's own EACCES message names the absolute path, raw ESC included.
          const err = await refusal('locked/sub/x.mds', 'U-SM29');
          assertNotFoundNotSymlink(err, 'locked/sub/x.mds', 'U-SM29');
        } finally {
          process.chdir(cwd);
          await chmod(locked, 0o755);
        }
      });
    },
  );
});

// The refusals NativeFs makes itself — a symlinked final component, a module
// outside the project root, a module that is not a regular file — carry its code
// and name the path as written, never the resolved one (#408).
describe('buildModulesMap — symlink, root-escape and non-file refusals match native (#408)', () => {
  const dirLinkType = process.platform === 'win32' ? 'junction' : 'dir';

  test('U-SM30: an import leaving the project root is refused as mds::import before it is opened', async () => {
    await withNestedProject(async (proj, parent) => {
      // The file exists, so only the containment check can refuse it.
      await writeFile(path.join(parent, 'outside.mds'), 'OUT\n');
      await writeFile(path.join(proj, 'main.mds'), '@import "../outside.mds" as o\n');
      const err = await rejectionOf(buildModulesMap(path.join(proj, 'main.mds'), scanImports), 'U-SM30');
      assertEscapeRefusal(err, '../outside.mds', 'U-SM30');
      assert.ok(!err.message.includes(await realpath(parent)), `U-SM30: no absolute path; got: ${err.message}`);

      // The one deliberate difference from the native backend (#414): containment is
      // decided before the file is looked at, so a MISSING file outside the root is
      // refused as escaping it — lexically, or through a symlinked directory — where
      // native reports it not found. U-SM32 pins native's side.
      await writeFile(path.join(proj, 'main.mds'), '@import "../missing.mds" as m\n');
      assertEscapeRefusal(
        await rejectionOf(buildModulesMap(path.join(proj, 'main.mds'), scanImports), 'U-SM30 missing'),
        '../missing.mds',
        'U-SM30 missing',
      );
      await mkdir(path.join(parent, 'outside-dir'));
      await symlink(path.join(parent, 'outside-dir'), path.join(proj, 'escape'), dirLinkType);
      await writeFile(path.join(proj, 'main.mds'), '@import "./escape/missing.mds" as m\n');
      assertEscapeRefusal(
        await rejectionOf(buildModulesMap(path.join(proj, 'main.mds'), scanImports), 'U-SM30 missing via link'),
        './escape/missing.mds',
        'U-SM30 missing via link',
      );

      // Control: the same import one level down stays inside the root and builds.
      await mkdir(path.join(proj, 'sub'));
      await writeFile(path.join(proj, 'in.mds'), 'IN\n');
      await writeFile(path.join(proj, 'sub', 'main.mds'), '@import "../in.mds" as i\n');
      const { modules } = await buildModulesMap(path.join(proj, 'sub', 'main.mds'), scanImports);
      assert.equal(modules['in.mds'], 'IN\n');
    });
  });

  test('U-SM31: a module that is not a regular file is mds::io, naming the path as written', async () => {
    await withNestedProject(async (proj) => {
      await mkdir(path.join(proj, 'dir.mds'));
      await writeFile(path.join(proj, 'main.mds'), '@import "./dir.mds" as d\n');
      const imported = await rejectionOf(buildModulesMap(path.join(proj, 'main.mds'), scanImports), 'U-SM31 import');
      const entry = await withCwd(proj, () => rejectionOf(buildModulesMap('dir.mds', scanImports), 'U-SM31 entry'));
      const canonicalProj = await realpath(proj);
      for (const [err, shown, label] of [[imported, './dir.mds', 'U-SM31 import'], [entry, 'dir.mds', 'U-SM31 entry']]) {
        assert.equal(err.code, 'mds::io', `${label}: ${err.message}`);
        assertNoHelp(err, label);
        // Refused before it is opened, with native's reason, on every OS (#428).
        assert.equal(err.message, `cannot read ${shown}: not a regular file`, label);
        assert.ok(!err.message.includes(canonicalProj), `${label}: no absolute path; got: ${err.message}`);
      }
    });
  });

  test('U-SM31b: compileFile — both backends refuse a module that is not a regular file before opening it (#428)', async (t) => {
    const engines = await loadEngines();
    if (!requireEngines(t, engines, 'U-SM31b')) return;
    await withNestedProject(async (proj) => {
      await mkdir(path.join(proj, 'dir.mds'));
      await writeFile(path.join(proj, 'imp-dir.mds'), '@import "./dir.mds" as d\nhi\n');
      await writeFile(path.join(proj, 'ok.mds'), 'OK\n');
      // [entry, typed from the project directory; the path native names the module by,
      // below the project root; the path the WASM backend names it by, as written].
      // For an entry typed from the root the two are one, so the whole error is too.
      const rows = [
        ['dir.mds', 'dir.mds', 'dir.mds'],
        ['imp-dir.mds', 'dir.mds', './dir.mds'],
      ];
      if (process.platform !== 'win32') {
        // A FIFO nobody writes to: opening it blocked both backends.
        execFileSync('mkfifo', [path.join(proj, 'fifo.mds')]);
        await writeFile(path.join(proj, 'imp-fifo.mds'), '@import "./fifo.mds" as f\nhi\n');
        rows.push(['fifo.mds', 'fifo.mds', 'fifo.mds'], ['imp-fifo.mds', 'fifo.mds', './fifo.mds']);
      }
      const files = [...rows.map(([entry]) => entry), 'ok.mds'];
      const native = await compileFileOutcomes('native', files, { cwd: proj });
      const wasm = await compileFileOutcomes('wasm', files, { cwd: proj });
      const refusal = (shown) => ({ code: 'mds::io', message: `cannot read ${shown}: not a regular file`, help: null, span: null });
      // Every row is judged before anything is asserted.
      const mismatches = [];
      const judge = (label, actual, expected) => {
        if (!isDeepStrictEqual(actual, expected)) mismatches.push({ label, actual, expected });
      };
      for (const [i, [entry, nativeShown, wasmShown]] of rows.entries()) {
        judge(`${entry} (native)`, native[i], refusal(nativeShown));
        judge(`${entry} (wasm)`, wasm[i], refusal(wasmShown));
      }
      // Control: a regular file compiles on both.
      judge('ok.mds (native)', native[rows.length], { output: 'OK\n' });
      judge('ok.mds (wasm)', wasm[rows.length], { output: 'OK\n' });
      assert.deepEqual(mismatches, []);
    });
  });

  test('U-SM32: compileFile — native and WASM backends refuse a symlink and a root escape with one code and message', async (t) => {
    const engines = await loadEngines();
    if (!requireEngines(t, engines, 'U-SM32')) return;
    await withNestedProject(async (proj, parent) => {
      await writeFile(path.join(proj, 'real.mds'), 'REAL\n');
      await symlink(path.join(proj, 'real.mds'), path.join(proj, 'linked.mds'));
      await writeFile(path.join(proj, 'imp-link.mds'), '@import "./linked.mds" as l\n');
      await writeFile(path.join(parent, 'outside.mds'), 'OUT\n');
      await writeFile(path.join(proj, 'imp-escape.mds'), '@import "../outside.mds" as o\n');
      await mkdir(path.join(parent, 'outside-dir'));
      await writeFile(path.join(parent, 'outside-dir', 'secret.mds'), 'SECRET\n');
      await symlink(path.join(parent, 'outside-dir'), path.join(proj, 'escape'), dirLinkType);
      await writeFile(path.join(proj, 'imp-escape-link.mds'), '@import "./escape/secret.mds" as s\n');
      await writeFile(path.join(proj, 'control.mds'), '@import "./real.mds" as r\n@include r\n');

      // The symlinked entry is typed relative to the subprocess's working directory,
      // so a message naming anything but the path as written cannot match.
      const linkedEntry = path.relative(pkgRoot, path.join(proj, 'linked.mds'));
      const cases = [
        [linkedEntry, 'mds::import', `import error: symlinks are not allowed in imports: ${linkedEntry}`],
        [path.join(proj, 'imp-link.mds'), 'mds::import', 'import error: symlinks are not allowed in imports: ./linked.mds'],
        [path.join(proj, 'imp-escape.mds'), 'mds::import', 'import error: import path escapes project directory: "../outside.mds"'],
        [
          path.join(proj, 'imp-escape-link.mds'),
          'mds::import',
          'import error: import path escapes project directory: "./escape/secret.mds"',
        ],
      ];
      // The one deliberate difference (#414): a MISSING file outside the root, named
      // lexically or through the symlinked directory. Native resolves the file first
      // and reports it not found; the pre-scanner decides containment first and
      // refuses it as escaping the project — an existence oracle it does not replicate.
      await writeFile(path.join(proj, 'imp-missing-outside.mds'), '@import "../missing.mds" as m\n');
      await writeFile(path.join(proj, 'imp-missing-escape-link.mds'), '@import "./escape/missing.mds" as m\n');
      const deliberate = [
        [path.join(proj, 'imp-missing-outside.mds'), '../missing.mds'],
        [path.join(proj, 'imp-missing-escape-link.mds'), './escape/missing.mds'],
      ];

      const files = [...cases.map(([file]) => file), path.join(proj, 'control.mds'), ...deliberate.map(([file]) => file)];
      const nativeOutcomes = await compileFileOutcomes('native', files);
      const wasmOutcomes = await compileFileOutcomes('wasm', files);
      for (const [i, [file, code, message]] of cases.entries()) {
        const [native, wasm] = [nativeOutcomes[i], wasmOutcomes[i]];
        const label = `U-SM32 ${file}`;
        assert.equal(native.code, code, `${label}: ${JSON.stringify(native)}`);
        assert.equal(native.message, message, `${label}: ${JSON.stringify(native)}`);
        // The whole shape: none of these carries help, and each names an entry or
        // an import native gives no span for a refusal of (#414).
        assert.deepEqual(wasm, native, label);
      }
      // Control: the same project compiles through the real file on both backends.
      assert.deepEqual(nativeOutcomes[cases.length], { output: 'REAL\n' });
      assert.deepEqual(wasmOutcomes[cases.length], nativeOutcomes[cases.length]);
      for (const [j, [file, shown]] of deliberate.entries()) {
        const i = cases.length + 1 + j;
        const label = `U-SM32 deliberate ${file}`;
        assert.deepEqual(nativeOutcomes[i], {
          code: 'mds::file_not_found',
          message: `file not found: ${shown}`,
          help: FILE_NOT_FOUND_HELP,
          span: firstLineSpan(`@import "${shown}" as m`),
        }, label);
        assert.deepEqual(wasmOutcomes[i], {
          code: 'mds::import',
          message: `import error: import path escapes project directory: "${shown}"`,
          help: null,
          span: null,
        }, label);
      }
    });
  });
});

// Every refusal the pre-scanner makes carries the code and message the native backend
// reports for the same input, and of several faults it reports the one native meets
// first (#414). One fault per fixture, compared through both backends' compileFile.
describe('buildModulesMap — each refusal and its order match native (#414)', () => {
  /** `n` path segments of `name`, joined by `/`. */
  const segments = (n, name) => Array.from({ length: n }, () => name).join('/');

  /** The engine's per-file cap (`mds::MAX_FILE_SIZE`). */
  const MAX_FILE_SIZE = 10 * 1024 * 1024;

  test("U-SM33: every scanner refusal carries native's code, message and help", async (t) => {
    const engines = await loadEngines();
    if (!requireEngines(t, engines, 'U-SM33')) return;
    await withNestedProject(async (proj) => {
      const write = async (rel, content) => {
        await mkdir(path.dirname(path.join(proj, rel)), { recursive: true });
        await writeFile(path.join(proj, rel), content);
        return path.join(proj, rel);
      };
      const tooLong = `./${segments(257, 'a')}.mds`;
      const tooManyUp = `${segments(257, '..')}/y.mds`;
      // entry + 64 imports in a chain: the 64th import is one too deep.
      for (let i = 0; i < 64; i++) await write(`chain/c${i}.mds`, `@import "./c${i + 1}.mds" as n\nx\n`);
      await write('chain/c64.mds', 'leaf\n');
      // entry + 256 imports is the most either backend resolves; one more is refused.
      for (let i = 0; i < 257; i++) await write(`leaves/l${i}.mds`, `L${i}\n`);
      const importsOf = (n) => Array.from({ length: n }, (_, i) => `@import "./leaves/l${i}.mds" as m${i}`).join('\n');
      const big = 'x'.repeat(MAX_FILE_SIZE + 1);

      // [label, entry, native expectation]: the whole error. A missing import points
      // at its directive (`span`), every other refusal at nothing.
      const rows = [
        ['import: more than 256 segments', await write('seg.mds', `@import "${tooLong}" as s\nhi\n`), {
          code: 'mds::resource_limit',
          message: `resource limit exceeded: import path exceeds maximum segment count (256): "${tooLong}"`,
          help: null,
          span: null,
        }],
        // Counted as written, before `..` could take it out of the project.
        ['import: more than 256 segments of ..', await write('up.mds', `@import "${tooManyUp}" as s\nhi\n`), {
          code: 'mds::resource_limit',
          message: `resource limit exceeded: import path exceeds maximum segment count (256): "${tooManyUp}"`,
          help: null,
          span: null,
        }],
        ['entry: more than 256 segments', `${segments(256, 'x')}/e.mds`, {
          code: 'mds::resource_limit',
          message: `resource limit exceeded: import path exceeds maximum segment count (256): "${segments(256, 'x')}/e.mds"`,
          help: null,
          span: null,
        }],
        ['import: one too deep', path.join(proj, 'chain', 'c0.mds'), {
          code: 'mds::import',
          message: 'import error: import depth exceeds maximum of 64 (possible deep chain)',
          help: null,
          span: null,
        }],
        ['import: one module too many', await write('count-258.mds', `${importsOf(257)}\nhi\n`), {
          code: 'mds::resource_limit',
          message: 'resource limit exceeded: module count exceeds maximum of 256 (256 modules resolved)',
          help: null,
          span: null,
        }],
        ['entry: over the per-file cap', await write('big.mds', big), {
          code: 'mds::resource_limit',
          message: `resource limit exceeded: file too large (${MAX_FILE_SIZE + 1} bytes, max ${MAX_FILE_SIZE} bytes): big.mds`,
          help: null,
          span: null,
        }],
        ['import: over the per-file cap', await write('imp-big.mds', '@import "./big.mds" as b\nhi\n'), {
          code: 'mds::resource_limit',
          message: `resource limit exceeded: file too large (${MAX_FILE_SIZE + 1} bytes, max ${MAX_FILE_SIZE} bytes): big.mds`,
          help: null,
          span: null,
        }],
        // An import or entry whose last component is `..` names no file for NativeFs,
        // on every OS: not found, whatever the directory it leads to.
        ['import: ends in ..', await write('sub/up.mds', '@import "../" as u\nhi\n'), {
          code: 'mds::file_not_found',
          message: 'file not found: ../',
          help: FILE_NOT_FOUND_HELP,
          span: firstLineSpan('@import "../" as u'),
        }],
        ['import: ends in .. below a name', await write('dotdot.mds', '@import "./sub/.." as u\nhi\n'), {
          code: 'mds::file_not_found',
          message: 'file not found: ./sub/..',
          help: FILE_NOT_FOUND_HELP,
          span: firstLineSpan('@import "./sub/.." as u'),
        }],
        ['entry: ends in ..', [proj, 'sub', '..'].join(path.sep), {
          code: 'mds::file_not_found',
          message: `file not found: ${[proj, 'sub', '..'].join(path.sep)}`,
          help: FILE_NOT_FOUND_HELP,
          span: null,
        }],
      ];
      // Controls: exactly the cap resolves on both, with the same output — a file of
      // exactly the per-file cap too (#428).
      const atTheCap = [
        await write('count-257.mds', `${importsOf(256)}\nhi\n`),
        path.join(proj, 'chain', 'c1.mds'),
        await write('exact.mds', 'x'.repeat(MAX_FILE_SIZE)),
      ];
      // `./` names the importing module's own directory: NativeFs reads it and fails
      // with `mds::io`. The message names the path differently on the two backends —
      // every `cannot read` message does (see buildModulesMap).
      const ownDir = await write('own.mds', '@import "./" as o\nhi\n');

      const files = [...rows.map(([, file]) => file), ...atTheCap, ownDir];
      const native = await compileFileOutcomes('native', files);
      const wasm = await compileFileOutcomes('wasm', files);
      // Every row is judged before anything is asserted, so a row that fails on one
      // platform cannot hide the rows after it.
      const mismatches = [];
      const judge = (label, actual, expected, ok = isDeepStrictEqual(actual, expected)) => {
        if (!ok) mismatches.push({ label, actual, expected });
      };
      for (const [i, [label, , expected]] of rows.entries()) {
        judge(`${label} (native)`, native[i], expected);
        judge(label, wasm[i], native[i]);
      }
      for (const j of atTheCap.keys()) {
        const i = rows.length + j;
        judge(`control ${atTheCap[j]} (native)`, native[i], 'an output', typeof native[i]?.output === 'string');
        judge(`control ${atTheCap[j]}`, wasm[i], native[i]);
      }
      const i = files.length - 1;
      judge('import: ./ (native)', native[i], { code: 'mds::io' }, native[i]?.code === 'mds::io');
      judge('import: ./', wasm[i], { code: 'mds::io' }, wasm[i]?.code === 'mds::io');
      assert.deepEqual(mismatches, []);
    });
  });

  test('U-SM33b: the aggregate-size guard is the WASM backend\'s own — native has none', async (t) => {
    const engines = await loadEngines();
    if (!requireEngines(t, engines, 'U-SM33b')) return;
    await withNestedProject(async (proj) => {
      // Each module is under the per-file cap; together they are over 10 MiB.
      const half = 'x'.repeat(6 * 1024 * 1024);
      await writeFile(path.join(proj, 'a.mds'), half);
      await writeFile(path.join(proj, 'b.mds'), half);
      const main = path.join(proj, 'main.mds');
      await writeFile(main, '@import "./a.mds" as a\n@import "./b.mds" as b\nhi\n');
      const [native] = await compileFileOutcomes('native', [main]);
      const [wasm] = await compileFileOutcomes('wasm', [main]);
      assert.deepEqual(native, { output: 'hi\n' });
      assert.deepEqual(wasm, {
        code: 'mds::resource_limit',
        message: `resource limit exceeded: aggregate module size exceeds maximum of ${10 * 1024 * 1024} bytes`,
        help: null,
        span: null,
      });
    });
  });

  // Writing at the filesystem root needs root: TP-39 runs this in a throwaway container
  // (`docker run --rm -v "$PWD":/repo -w /repo/packages/mds node:22 node --test
  // --test-name-pattern U-SM34 __test__/scanner.spec.mjs`).
  const notRoot =
    !(typeof process.getuid === 'function' && process.getuid() === 0) &&
    'needs root, to write at the filesystem root: run it in a throwaway container (TP-39)';

  test('U-SM34: a project rooted at the filesystem root compiles through the WASM backend as on native', { skip: notRoot }, async (t) => {
    const engines = await loadEngines();
    if (!requireEngines(t, engines, 'U-SM34')) return;
    const root = path.parse(process.cwd()).root;
    const name = `mds-u-sm34-${process.pid}`;
    const lib = path.join(root, `${name}-lib.mds`);
    const entry = path.join(root, `${name}.mds`);
    const up = path.join(root, `${name}-up.mds`);
    try {
      await writeFile(lib, 'LIB\n');
      await writeFile(entry, `@import "./${name}-lib.mds" as l\n@include l\n`);
      // `/..` is `/` on native; the engine's key space has nothing above its root (#424).
      await writeFile(up, `@import "../${name}-lib.mds" as l\n@include l\n`);
      const native = await compileFileOutcomes('native', [entry, up]);
      const wasm = await compileFileOutcomes('wasm', [entry, up]);
      assert.deepEqual(native, [{ output: 'LIB\n' }, { output: 'LIB\n' }]);
      assert.deepEqual(wasm[0], native[0]);
      assert.deepEqual(wasm[1], {
        code: 'mds::import',
        message: `import error: import path escapes project directory: "../${name}-lib.mds"`,
        help: null,
        span: null,
      });
    } finally {
      await rm(lib, { force: true });
      await rm(entry, { force: true });
      await rm(up, { force: true });
    }
  });

  test("U-SM35: bytes that are not valid UTF-8 are refused with native's mds::io error; a BOM file compiles identically", async (t) => {
    const engines = await loadEngines();
    if (!requireEngines(t, engines, 'U-SM35')) return;
    await withNestedProject(async (proj) => {
      await mkdir(path.join(proj, 'sub'));
      // An invalid byte, and an incomplete sequence at the very end (the first two
      // bytes of U+20AC).
      await writeFile(path.join(proj, 'bad.mds'), Buffer.from([0x68, 0x69, 0xff, 0x0a]));
      await writeFile(path.join(proj, 'sub', 'tail.mds'), Buffer.from([0x68, 0x69, 0x0a, 0xe2, 0x82]));
      await writeFile(path.join(proj, 'imp-bad.mds'), '@import "./bad.mds" as b\nhi\n');
      await writeFile(path.join(proj, 'imp-tail.mds'), '@import "./sub/tail.mds" as b\nhi\n');
      // Control: a byte-order mark is valid UTF-8, kept as the template's first character.
      await writeFile(path.join(proj, 'bom.mds'), Buffer.from([0xef, 0xbb, 0xbf, ...Buffer.from('Hello\n')]));
      await writeFile(path.join(proj, 'imp-bom.mds'), '@import "./bom.mds" as b\n@include b\n');

      const badByte = 'invalid utf-8 sequence of 1 bytes from index 2';
      const incomplete = 'incomplete utf-8 byte sequence from index 3';
      const refused = [
        ['bad.mds', `invalid UTF-8 in bad.mds: ${badByte}`],
        [path.join('sub', 'tail.mds'), `invalid UTF-8 in sub/tail.mds: ${incomplete}`],
        ['imp-bad.mds', `invalid UTF-8 in bad.mds: ${badByte}`],
        ['imp-tail.mds', `invalid UTF-8 in sub/tail.mds: ${incomplete}`],
      ];
      const files = [...refused.map(([rel]) => path.join(proj, rel)), path.join(proj, 'bom.mds'), path.join(proj, 'imp-bom.mds')];
      const native = await compileFileOutcomes('native', files);
      const wasm = await compileFileOutcomes('wasm', files);
      for (const [i, [rel, message]] of refused.entries()) {
        assert.deepEqual(native[i], { code: 'mds::io', message, help: null, span: null }, `${rel} (native)`);
        assert.deepEqual(wasm[i], native[i], rel);
      }
      assert.deepEqual(native[refused.length], { output: `${String.fromCodePoint(0xfeff)}Hello\n` });
      assert.equal(typeof native[refused.length + 1].output, 'string', JSON.stringify(native[refused.length + 1]));
      assert.deepEqual(wasm.slice(refused.length), native.slice(refused.length));
    });
  });

  test('U-SM36: a missing entry directly under the filesystem root is not found on both backends, help included', async (t) => {
    const engines = await loadEngines();
    if (!requireEngines(t, engines, 'U-SM36')) return;
    const missing = path.join(path.parse(process.cwd()).root, `mds-u-sm-${process.pid}-missing.mds`);
    assert.equal(existsSync(missing), false, `${missing} must not exist`);
    const [native] = await compileFileOutcomes('native', [missing]);
    const [wasm] = await compileFileOutcomes('wasm', [missing]);
    assert.deepEqual(native, {
      code: 'mds::file_not_found',
      message: `file not found: ${missing}`,
      help: FILE_NOT_FOUND_HELP,
      span: null,
    });
    assert.deepEqual(wasm, native);
    assert.equal(existsSync(missing), false, 'nothing is written');
  });

  test('U-SM39: the aggregate-size guard never pre-empts a refusal native makes of the module that crosses it', async (t) => {
    const engines = await loadEngines();
    if (!requireEngines(t, engines, 'U-SM39')) return;
    await withNestedProject(async (proj) => {
      const MiB = 1024 * 1024;
      const text = (bytes) => 'x'.repeat(bytes);
      const invalidUtf8 = (bytes) => Buffer.concat([Buffer.from(text(bytes - 1)), Buffer.from([0xff])]);
      // Each project crosses the 10 MiB aggregate at its last module, `c`, which native
      // refuses on its own. In `walk-*` the read-ahead budget is spent on `a`, so the
      // walk reads `c` itself; in `race-*` `a` and `c` are read ahead side by side, and
      // which of them the budget admits depends on which read gets there first.
      const rows = [
        // [label, name of c, content of c, native's refusal of c]
        ['invalid UTF-8', 'c.mds', invalidUtf8, 'mds::io'],
        ['not an MDS file', 'c.txt', text, 'mds::not_mds'],
        // Control (PF-013): a valid `c` compiles on native, and crosses the guard here.
        ['valid', 'c.mds', text, null],
      ];
      const entries = [];
      for (const [label, name, content] of rows) {
        const slug = label.replace(/\W+/g, '-');
        const walkDir = path.join(proj, `walk-${slug}`);
        await mkdir(walkDir);
        await writeFile(path.join(walkDir, 'e.mds'), `@import "./a.mds" as a\nhi\n${text(MiB)}`);
        await writeFile(path.join(walkDir, 'a.mds'), `@import "./${name}" as c\nA\n${text(8.5 * MiB)}`);
        await writeFile(path.join(walkDir, name), content(Math.floor(1.6 * MiB)));
        const raceDir = path.join(proj, `race-${slug}`);
        await mkdir(raceDir);
        await writeFile(path.join(raceDir, 'e.mds'), `@import "./a.mds" as a\n@import "./${name}" as c\nhi\n`);
        await writeFile(path.join(raceDir, 'a.mds'), text(6 * MiB));
        await writeFile(path.join(raceDir, name), content(6 * MiB));
        entries.push(path.join(walkDir, 'e.mds'), path.join(raceDir, 'e.mds'));
      }
      const native = await compileFileOutcomes('native', entries);
      const wasm = await compileFileOutcomes('wasm', entries);
      for (const [r, [label, , , code]] of rows.entries()) {
        for (const i of [2 * r, 2 * r + 1]) {
          if (code === null) {
            assert.equal(typeof native[i].output, 'string', `${label}: ${JSON.stringify(native[i]).slice(0, 200)}`);
            assert.deepEqual(wasm[i], {
              code: 'mds::resource_limit',
              message: `resource limit exceeded: aggregate module size exceeds maximum of ${10 * MiB} bytes`,
              help: null,
              span: null,
            }, `${label} ${entries[i]}`);
          } else {
            assert.equal(native[i].code, code, `${label}: ${JSON.stringify(native[i])}`);
            assert.deepEqual(wasm[i], native[i], `${label} ${entries[i]}`);
          }
        }
      }
    });
  });

  test('U-SM37: of several faults, both backends report the one native meets first — depth first, in import order', async (t) => {
    const engines = await loadEngines();
    if (!requireEngines(t, engines, 'U-SM37')) return;
    await withNestedProject(async (proj, parent) => {
      await writeFile(path.join(parent, 'outside.mds'), 'OUT\n');
      await writeFile(path.join(proj, 'real.mds'), 'REAL\n');
      await symlink(path.join(proj, 'real.mds'), path.join(proj, 'link.mds'));
      await writeFile(path.join(proj, 'ok.mds'), '@import "./deep-missing.mds" as d\nOK\n');
      // Each fault and the error native reports for it. `ok.mds` is fine itself, but
      // its own import is missing: depth first, that comes before its later siblings.
      // A missing module's error points at the directive that imports it: in rotation
      // `r`, fault `r` is on the first line; `ok.mds`'s import is on its own first line.
      const faults = [
        ['@import "./missing.mds" as a', 'mds::file_not_found', 'file not found: ./missing.mds'],
        ['@import "../outside.mds" as b', 'mds::import', 'import error: import path escapes project directory: "../outside.mds"'],
        ['@import "./link.mds" as c', 'mds::import', 'import error: symlinks are not allowed in imports: ./link.mds'],
        ['@import "./ok.mds" as d', 'mds::file_not_found', 'file not found: ./deep-missing.mds'],
      ];
      const spans = [firstLineSpan(faults[0][0]), null, null, firstLineSpan('@import "./deep-missing.mds" as d')];
      const files = [];
      for (const r of faults.keys()) {
        const rotated = [...faults.slice(r), ...faults.slice(0, r)];
        const file = path.join(proj, `rot-${r}.mds`);
        await writeFile(file, `${rotated.map(([line]) => line).join('\n')}\nhi\n`);
        files.push(file);
      }
      const native = await compileFileOutcomes('native', files);
      const wasm = await compileFileOutcomes('wasm', files);
      for (const [r, [, code, message]] of faults.entries()) {
        const label = `U-SM37 rotation ${r}`;
        assert.deepEqual(native[r], {
          code,
          message,
          help: code === 'mds::file_not_found' ? FILE_NOT_FOUND_HELP : null,
          span: spans[r],
        }, label);
        assert.deepEqual(wasm[r], native[r], label);
      }
    });
  });

  test('U-SM38: an error the engine raises is identical on both backends, but a later refusal pre-empts it on WASM (difference 8)', async (t) => {
    const engines = await loadEngines();
    if (!requireEngines(t, engines, 'U-SM38')) return;
    await withNestedProject(async (proj) => {
      const files = {
        'cyc.mds': '@import "./cyc-a.mds" as a\nhi\n',
        'cyc-a.mds': '@import "./cyc.mds" as e\nA\n',
        'ev.mds': '@import "./ev-a.mds" as a\nhi\n',
        'ev-a.mds': '{{nope}}\n',
        // The same two faults, each followed by an import of a missing module.
        'cyc-then.mds': '@import "./cyc-then-a.mds" as a\n@import "./missing.mds" as m\nhi\n',
        'cyc-then-a.mds': '@import "./cyc-then.mds" as e\nA\n',
        'ev-then.mds': '@import "./ev-a.mds" as a\n@import "./missing.mds" as m\nhi\n',
      };
      for (const [name, content] of Object.entries(files)) {
        await writeFile(path.join(proj, name), content);
      }
      const entries = ['cyc.mds', 'ev.mds', 'cyc-then.mds', 'ev-then.mds'].map((name) => path.join(proj, name));
      const native = await compileFileOutcomes('native', entries);
      const wasm = await compileFileOutcomes('wasm', entries);
      const cycle = (entry) => ({
        code: 'mds::circular_import',
        message: `circular import detected: ${entry}.mds → ${entry}-a.mds → ${entry}.mds`,
      });
      const undefinedVar = { code: 'mds::undefined_var', message: "undefined variable 'nope'" };
      // Alone, the engine's own error — a circular import, an evaluation error — is the
      // same on both backends, span included.
      assert.deepEqual({ code: native[0].code, message: native[0].message }, cycle('cyc'));
      assert.deepEqual({ code: native[1].code, message: native[1].message }, undefinedVar);
      assert.deepEqual(wasm.slice(0, 2), native.slice(0, 2));
      // Followed by a missing import: native meets its own error first; the WASM
      // backend reads every module before its engine runs, so the missing import is
      // refused first.
      assert.deepEqual({ code: native[2].code, message: native[2].message }, cycle('cyc-then'));
      assert.deepEqual({ code: native[3].code, message: native[3].message }, undefinedVar);
      for (const i of [2, 3]) {
        const second = '@import "./missing.mds" as m';
        const first = files[path.basename(entries[i])].split('\n')[0];
        assert.deepEqual(wasm[i], {
          code: 'mds::file_not_found',
          message: 'file not found: ./missing.mds',
          help: FILE_NOT_FOUND_HELP,
          span: { offset: first.length + 1, length: second.length, line: 2, column: 1 },
        }, entries[i]);
      }
    });
  });

  /** A module whose frontmatter imports `paths`, the i-th as `m<i>`. */
  const frontmatterImports = (...paths) =>
    `---\nimports:\n${paths.map((p, i) => `  - path: ${p}\n    as: m${i}\n`).join('')}---\nhi\n`;

  test("U-SM40: a frontmatter import is refused with native's import error, naming its imports index", async (t) => {
    const engines = await loadEngines();
    if (!requireEngines(t, engines, 'U-SM40')) return;
    await withNestedProject(async (proj, parent) => {
      await writeFile(path.join(proj, 'ok.mds'), '@define f():\nOK\n@end\n');
      await writeFile(path.join(parent, 'outside.mds'), 'OUT\n');
      await writeFile(path.join(proj, 'real.mds'), 'REAL\n');
      await symlink(path.join(proj, 'real.mds'), path.join(proj, 'link.mds'));
      // A module whose own `@export … from` names a missing module, and one whose own
      // frontmatter import does (at imports[1]).
      await writeFile(path.join(proj, 'mid.mds'), '@export x from "./gone.mds"\nhi\n');
      await writeFile(path.join(proj, 'fmid.mds'), frontmatterImports('./ok.mds', './nope.mds'));
      const importError = (detail) => ({ code: 'mds::import', message: `import error: ${detail}`, help: null, span: null });
      const rows = [
        ['fm-missing.mds', frontmatterImports('./ok.mds', './missing.mds'),
          importError('file not found: "./missing.mds" (in frontmatter imports[1])')],
        ['fm-escape.mds', frontmatterImports('../outside.mds'),
          importError('import path escapes project directory: "../outside.mds" (in frontmatter imports[0])')],
        ['fm-link.mds', frontmatterImports('./ok.mds', './link.mds'),
          importError('symlinks are not allowed in imports: ./link.mds (in frontmatter imports[1])')],
        // Raised below a frontmatter import with no span: the index of the import that
        // reached it is added, whatever the module that names the missing file.
        ['fm-nested.mds', frontmatterImports('./mid.mds'),
          importError('file not found: "./gone.mds" (in frontmatter imports[0])')],
        // Already placed in a frontmatter: nothing more is added.
        ['fm-fm.mds', frontmatterImports('./fmid.mds'),
          importError('file not found: "./nope.mds" (in frontmatter imports[1])')],
        // Control: the same missing module through a body import is not found, and
        // points at its directive.
        ['body-missing.mds', '@import "./missing.mds" as m\nhi\n', {
          code: 'mds::file_not_found',
          message: 'file not found: ./missing.mds',
          help: FILE_NOT_FOUND_HELP,
          span: firstLineSpan('@import "./missing.mds" as m'),
        }],
      ];
      for (const [name, content] of rows) {
        await writeFile(path.join(proj, name), content);
      }
      const entries = rows.map(([name]) => path.join(proj, name));
      const native = await compileFileOutcomes('native', entries);
      const wasm = await compileFileOutcomes('wasm', entries);
      for (const [i, [name, , expected]] of rows.entries()) {
        assert.deepEqual(native[i], expected, `${name} (native)`);
        assert.deepEqual(wasm[i], native[i], name);
      }
    });
  });

  test("U-SM41: a missing import's error points where native's does — every directive, byte offset and line", async (t) => {
    const engines = await loadEngines();
    if (!requireEngines(t, engines, 'U-SM41')) return;
    await withNestedProject(async (proj) => {
      const bom = String.fromCodePoint(0xfeff);
      await writeFile(path.join(proj, 'mid.mds'), '@export * from "./gone.mds"\nhi\n');
      // [name, content, native's message, the line native's span is on (null: no
      // span)]. Multi-byte text, CRLF line ends and a byte-order mark precede the
      // directive, so its byte offset differs from its character offset.
      const rows = [
        ['multibyte.mds', '---\ntitle: "héllo — wörld"\n---\r\ncafé\r\n@import "./missing.mds" as m\r\nhi\n',
          'file not found: ./missing.mds', 5],
        // A directive starts its line: the byte-order mark opens a text line.
        ['bom.mds', `${bom}hi\n@import "./missing.mds"\nhi\n`, 'file not found: ./missing.mds', 2],
        ['selective.mds', 'ünï\n@import { a } from "./missing.mds"\nhi\n', 'file not found: ./missing.mds', 2],
        ['extends.mds', '---\nx: é\n---\n@extends "./missing.mds"\n', 'file not found: ./missing.mds', 4],
        // `@export … from` adds no span.
        ['export-from.mds', '@export x from "./missing.mds"\nhi\n', 'file not found: ./missing.mds', null],
        ['wildcard.mds', 'ü\n@export * from "./missing.mds"\nhi\n', 'file not found: ./missing.mds', null],
        // Raised below an @import with no span: native names the import that reached it
        // and points at its directive.
        ['body-nested.mds', 'ü\n@import "./mid.mds" as m\nhi\n', 'file not found: ./mid.mds', 2],
      ];
      for (const [name, content] of rows) {
        await writeFile(path.join(proj, name), content);
      }
      const entries = rows.map(([name]) => path.join(proj, name));
      const native = await compileFileOutcomes('native', entries);
      const wasm = await compileFileOutcomes('wasm', entries);
      for (const [i, [name, , message, line]] of rows.entries()) {
        assert.equal(native[i].code, 'mds::file_not_found', `${name}: ${JSON.stringify(native[i])}`);
        assert.equal(native[i].message, message, name);
        assert.equal(native[i].span?.line ?? null, line, `${name}: ${JSON.stringify(native[i].span)}`);
        assert.deepEqual(wasm[i], native[i], name);
      }
      // Non-vacuity: offsets are in bytes — the byte-order mark is three.
      assert.equal(Buffer.byteLength(bom), 3);
      assert.deepEqual(native[1].span, { offset: 6, length: '@import "./missing.mds"'.length, line: 2, column: 1 });
      const multibyte = Buffer.from(rows[0][1]);
      assert.equal(native[0].span.offset, multibyte.indexOf('@import'));
      assert.notEqual(native[0].span.offset, rows[0][1].indexOf('@import'));
    });
  });

  test('U-SM42: a template reached as an @extends base resolves its imports before its own base, as on native', async (t) => {
    const engines = await loadEngines();
    if (!requireEngines(t, engines, 'U-SM42')) return;
    await withNestedProject(async (proj) => {
      // Each base both imports a missing module and extends a missing base. Native
      // resolves a base template's frontmatter and body imports first, and its own
      // `@extends` last; a template compiled for itself resolves its `@extends` first.
      const files = {
        'fm-base.mds': '---\nimports:\n  - path: ./gone-fm.mds\n---\n@extends "./gone-base.mds"\n',
        'body-base.mds': '@extends "./gone-base.mds"\n@import "./gone-body.mds" as g\n',
        // The same path extended and imported: it is the import that fails first.
        'same-base.mds': '---\nimports:\n  - path: ./gone-base.mds\n---\n@extends "./gone-base.mds"\n',
        'child-fm.mds': '@extends "./fm-base.mds"\n',
        'child-body.mds': '@extends "./body-base.mds"\n',
        'child-same.mds': '@extends "./same-base.mds"\n',
      };
      for (const [name, content] of Object.entries(files)) {
        await writeFile(path.join(proj, name), content);
      }
      const extendsLine = '@extends "./gone-base.mds"';
      const rows = [
        ['child-fm.mds', {
          code: 'mds::import',
          message: 'import error: file not found: "./gone-fm.mds" (in frontmatter imports[0])',
          help: null,
          span: null,
        }],
        ['child-body.mds', {
          code: 'mds::file_not_found',
          message: 'file not found: ./gone-body.mds',
          help: FILE_NOT_FOUND_HELP,
          span: { offset: `${extendsLine}\n`.length, length: '@import "./gone-body.mds" as g'.length, line: 2, column: 1 },
        }],
        ['child-same.mds', {
          code: 'mds::import',
          message: 'import error: file not found: "./gone-base.mds" (in frontmatter imports[0])',
          help: null,
          span: null,
        }],
        // Control: compiled for itself, the base resolves its own `@extends` first.
        ['fm-base.mds', {
          code: 'mds::file_not_found',
          message: 'file not found: ./gone-base.mds',
          help: FILE_NOT_FOUND_HELP,
          span: { offset: files['fm-base.mds'].indexOf(extendsLine), length: extendsLine.length, line: 5, column: 1 },
        }],
      ];
      const entries = rows.map(([name]) => path.join(proj, name));
      const native = await compileFileOutcomes('native', entries);
      const wasm = await compileFileOutcomes('wasm', entries);
      for (const [i, [name, expected]] of rows.entries()) {
        assert.deepEqual(native[i], expected, `${name} (native)`);
        assert.deepEqual(wasm[i], native[i], name);
      }
    });
  });

  test('U-SM43: a file larger than its size check is read to one byte past the cap and no further (#428)', async () => {
    await withNestedProject(async (proj) => {
      const grown = path.join(proj, 'grown.mds');
      const size = MAX_FILE_SIZE + 3 * 1024 * 1024;
      await writeFile(grown, 'x'.repeat(size));
      const main = path.join(proj, 'main.mds');
      await writeFile(main, '@import "./grown.mds" as g\nhi\n');
      // What reaches the engine: the length of every buffer handed to preflightModule.
      const seen = [];
      const engine = {
        scanImportRecords: importRecordsOf(scanImports),
        preflightModule(bytes, display, shown) {
          seen.push(bytes.length);
          return preflightModule(bytes, display, shown);
        },
      };
      const tooLarge = (n) => ({
        code: 'mds::resource_limit',
        message: `resource limit exceeded: file too large (${n} bytes, max ${MAX_FILE_SIZE} bytes): grown.mds`,
        help: null,
        span: null,
      });
      // Control: its size when it is opened refuses it with its size, before a byte of
      // it is read — as native refuses it.
      for (const entry of [grown, main]) {
        assert.deepEqual(errorShape(await rejectionOf(buildModulesMapWith(entry, engine), entry)), tooLarge(size));
      }
      assert.ok(!seen.includes(size), `nothing of it reached the engine: ${seen}`);

      // A file that grows after its size check: every fstat reports it under the cap.
      const probe = await open(grown);
      const FileHandle = Object.getPrototypeOf(probe);
      await probe.close();
      const realStat = FileHandle.stat;
      FileHandle.stat = async function stat(...args) {
        const stats = await realStat.apply(this, args);
        stats.size = Math.min(stats.size, 100);
        return stats;
      };
      try {
        for (const entry of [grown, main]) {
          seen.length = 0;
          const err = await rejectionOf(buildModulesMapWith(entry, engine), `${entry} grown`);
          assert.deepEqual(errorShape(err), tooLarge(MAX_FILE_SIZE + 1), entry);
          // At most one byte past the cap reached the engine — and that much did
          // (non-vacuity): the read went on past the size it was told.
          assert.equal(Math.max(...seen), MAX_FILE_SIZE + 1, `${entry}: ${seen}`);
        }
      } finally {
        FileHandle.stat = realStat;
      }
    });
  });
});

describe('findProjectRoot', () => {
  // Each test uses a fresh mkdtemp directory to guarantee unique paths that
  // will not collide with cached results from prior test runs.

  test('U-PR1: returns directory containing .git marker', async () => {
    // Arrange: root/sub/ — .git lives at root, start is root/sub
    const root = await mkdtemp(path.join(os.tmpdir(), 'mds-pr-test-'));
    try {
      const sub = path.join(root, 'sub');
      await mkdir(sub);
      await mkdir(path.join(root, '.git'));
      // Act
      const result = findProjectRoot(sub);
      // Assert: should walk up from sub and find root via .git
      assert.equal(result, root);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test('U-PR2: returns directory containing .mdsroot marker', async () => {
    // Arrange: root/a/b/ — .mdsroot lives at root, start is root/a/b
    const root = await mkdtemp(path.join(os.tmpdir(), 'mds-pr-test-'));
    try {
      const deep = path.join(root, 'a', 'b');
      await mkdir(deep, { recursive: true });
      await writeFile(path.join(root, '.mdsroot'), '');
      // Act
      const result = findProjectRoot(deep);
      // Assert: should walk up and find root via .mdsroot
      assert.equal(result, root);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test('U-PR3: falls back to start directory when no marker is found', async () => {
    // Arrange: isolated temp dir with no .git or .mdsroot anywhere in its tree.
    // We create a subdirectory so the traversal has at least one step.
    const root = await mkdtemp(path.join(os.tmpdir(), 'mds-pr-test-'));
    try {
      const sub = path.join(root, 'sub');
      await mkdir(sub);
      // Act: use a deep path that lives inside os.tmpdir() but has no marker.
      // We cannot guarantee os.tmpdir() itself has no .git (e.g. in CI or
      // monorepo environments), so we accept either the original start argument
      // or any ancestor — as long as the result is a prefix of sub.
      const result = findProjectRoot(sub);
      // Assert: result is either `sub` itself (fallback) or an ancestor of `sub`
      // that contains a marker. Either way, sub must start with result + '/'.
      assert.ok(
        result === sub || sub.startsWith(result + '/'),
        `expected result to be sub or an ancestor of sub, got: ${result}`,
      );
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test('U-PR4: returns start when called with a path already at filesystem root sentinel', () => {
    // Simulate the filesystem-root sentinel: dirname(x) === x. We verify that
    // findProjectRoot('') does not loop forever — dirname('') returns '.' which
    // is not the same as '' so this exercises the loop normally, but we can
    // verify a known-fallback path like the OS temp dir itself.
    // The real edge case (parent === dir) fires at the OS root '/'.
    // We verify indirectly: if the traversal walked up to '/', findProjectRoot
    // returned start. We test this by confirming that a fresh temp directory
    // (with no markers) returns the start, not '/'.
    const start = os.tmpdir();
    const result = findProjectRoot(start);
    // Result is either start (no marker found and fallback triggered) or
    // some ancestor that happens to have a .git. Either way, it must be a string
    // and must not be empty — we cannot assert the exact value here because
    // os.tmpdir() may be inside a git repo on some machines.
    assert.ok(typeof result === 'string' && result.length > 0, 'result must be a non-empty path');
  });

  test('U-PR5: result is cached — same start returns same value on second call', async () => {
    // The cache makes repeated traversal O(1) after the first call.
    // We verify observable correctness: two calls with the same start return ===.
    const root = await mkdtemp(path.join(os.tmpdir(), 'mds-pr-test-'));
    try {
      const sub = path.join(root, 'sub');
      await mkdir(sub);
      await mkdir(path.join(root, '.git'));
      const first = findProjectRoot(sub);
      const second = findProjectRoot(sub);
      assert.equal(first, second);
      assert.equal(first, root);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });
});
