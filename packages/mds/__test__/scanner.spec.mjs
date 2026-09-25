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
import path from 'node:path';
import { chmod, mkdtemp, mkdir, symlink, writeFile, rm } from 'node:fs/promises';
import os from 'node:os';
import {
  FORBIDDEN_PATH_CODEPOINTS,
  assertNoForbiddenChars,
  caseInsensitive,
  compileFileOutcomes,
  escapeText,
  loadEngines,
  rejectionOf,
  requireEngines,
  thrownBy,
  uPlus,
} from './helpers.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

// Import from the compiled dist.
// Note: module-scanner is a Node-only utility (uses fs/promises).
const { normalizeVirtualKey, buildModulesMap, findProjectRoot } = await import('../dist/util/module-scanner.js');

const FIXTURES = path.join(__dirname, 'fixtures');

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
 * A missing file — whether never found (nonexistent name, nonexistent parent or
 * grandparent directory, ENOENT/ENOTDIR alike) or a mismatched spelling on a
 * case-sensitive volume (#408) — is reported with the same `mds::file_not_found`
 * shape the Rust engine reports for a missing file, keyed on `shown` (never a
 * resolved absolute path, R3 / CWE-209) and never described as a symlink.
 */
function assertNotFoundNotSymlink(err, shown, label) {
  assert.equal(err.code, 'mds::file_not_found', `${label}: ${err.message}`);
  assert.equal(err.message, `file not found: ${shown}`, label);
  assert.doesNotMatch(err.message, /symlink/, label);
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

  test('U-SM7: rejects symlink with security error', async () => {
    // openNoFollow uses O_NOFOLLOW (Linux/macOS) or a post-open realpath check
    // (Windows) to detect symlinks. This test creates a real symlink in a temp
    // directory and confirms the scanner surfaces a security error.
    const tmpDir = await mkdtemp(path.join(os.tmpdir(), 'mds-scanner-test-'));
    try {
      const realFile = path.join(tmpDir, 'real.mds');
      const linkFile = path.join(tmpDir, 'link.mds');
      await writeFile(realFile, 'Hello world');
      await symlink(realFile, linkFile);
      await assert.rejects(
        () => buildModulesMap(linkFile, scanImports),
        /security.*symlink/,
      );
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
        // The entry is keyed as typed: that key is what the WASM engine is handed.
        const { entryFilename, modules } = await build;
        assert.equal(entryFilename, 'MAIN.mds');
        assert.deepEqual(modules, { 'MAIN.mds': 'Hello!\n' });
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
        // Keyed as written, since the engine's virtual filesystem looks the import
        // up under exactly that key.
        const { modules } = await build;
        assert.equal(modules['Header.mds'], 'hi\n');
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
      await assert.rejects(buildModulesMap(path.join(dir, 'main.mds'), scanImports), /security.*symlink/);
      await assert.rejects(buildModulesMap(path.join(dir, 'link.mds'), scanImports), /security.*symlink/);

      await writeFile(path.join(dir, 'main.mds'), '@import "./LINK.mds" as l\n');
      const mismatched = buildModulesMap(path.join(dir, 'main.mds'), scanImports);
      if (insensitive) {
        await assert.rejects(mismatched, /security.*symlink/);
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
      const { modules } = await buildModulesMap(path.join(dir, 'main.mds'), scanImports);
      assert.equal(modules['alias/lib.mds'], 'lib\n');

      // Outside: the canonical path is what containment is checked on.
      const outside = await mkdtemp(path.join(os.tmpdir(), 'mds-scanner-408-outside-'));
      try {
        await writeFile(path.join(outside, 'secret.mds'), 'secret\n');
        await symlink(outside, path.join(dir, 'escape'), dirLinkType);
        await writeFile(path.join(dir, 'main.mds'), '@import "./escape/secret.mds" as s\n');
        await assert.rejects(
          buildModulesMap(path.join(dir, 'main.mds'), scanImports),
          /security: path escapes project root/,
        );
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

  test('U-SM21: compileFile on an entry with a missing parent directory — native and WASM backends agree on code and message', async (t) => {
    const engines = await loadEngines();
    if (!requireEngines(t, engines, 'U-SM21')) return;
    const missing = path.join(FIXTURES, `mds-scanner-u-sm21-${process.pid}-nonexistent`, 'file.mds');
    const [native] = await compileFileOutcomes('native', [missing]);
    const [wasm] = await compileFileOutcomes('wasm', [missing]);
    assert.equal(native.code, 'mds::file_not_found', JSON.stringify(native));
    assert.equal(native.message, `file not found: ${missing}`, JSON.stringify(native));
    // code/message only, not the full errorShape: native reaches this error through
    // Rust's own NativeFs (compileFile calls the napi addon directly, never
    // buildModulesMap), whose FileNotFound carries a `help` diagnostic
    // (crates/mds-core/src/error.rs); the WASM backend reaches it through this
    // file's buildModulesMap, whose PathError type carries no `help` field at all
    // (true of every mds::io/mds::import/mds::file_not_found refusal built by
    // pathError() in this file, not something this fix introduces or narrows).
    assert.equal(wasm.code, native.code, JSON.stringify({ wasm, native }));
    assert.equal(wasm.message, native.message, JSON.stringify({ wasm, native }));
  });

  // The WASM engine resolves an import BY NAME, from the importing module's key;
  // NativeFs resolves it ON DISK, from the importing module's canonical directory.
  // Through a symlinked directory the two agree for every import except one that
  // leaves the link through `..`: its key names the directory beside the link,
  // the file NativeFs reads sits beside the link's target.

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

  test("U-SM22: an import that leaves a symlinked directory through '..' is refused, never read from the other side", async () => {
    await withProject(async (dir) => {
      const main = await dotDotOutOfLink(dir, true);
      const err = await rejectionOf(buildModulesMap(main, scanImports), 'U-SM22');
      assert.equal(err.code, 'mds::import', err.message);
      assert.equal(
        err.message,
        `import error: import path leaves a symlinked directory through '..', which the WASM backend cannot resolve: "../z.mds"`,
      );
    });
    // Control: the same tree with `sub` a real directory builds, and `../z.mds`
    // from sub/y.mds is the root z.mds under both resolutions.
    await withProject(async (dir) => {
      const { modules } = await buildModulesMap(await dotDotOutOfLink(dir, false), scanImports);
      assert.equal(modules['z.mds'], 'ROOT-Z\n');
      assert.equal(modules['sub/y.mds'], '@import "../z.mds" as dz\nY=\n@include dz\n');
    });
  });

  test('U-SM23: a file reached through a symlinked directory is stored under every key the engine looks up', async () => {
    await withProject(async (dir) => {
      await mkdir(path.join(dir, 'lib'));
      await writeFile(path.join(dir, 'lib', 'x.mds'), 'X\n');
      await writeFile(path.join(dir, 'lib', 'y.mds'), '@import "./x.mds" as x\n');
      await symlink(path.join(dir, 'lib'), path.join(dir, 'alias'), dirLinkType);
      await writeFile(path.join(dir, 'main.mds'), '@import "./lib/x.mds" as a\n@import "./alias/y.mds" as b\n');
      const { modules } = await buildModulesMap(path.join(dir, 'main.mds'), scanImports);
      // lib/x.mds and alias/x.mds are one file on disk, but the engine resolves
      // alias/y.mds's `./x.mds` to the key alias/x.mds: both keys must be present.
      assert.equal(modules['lib/x.mds'], 'X\n');
      assert.equal(modules['alias/x.mds'], 'X\n');
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
      // `..` out of the link: NativeFs reads deep/z.mds; the WASM backend refuses
      // rather than compile the root z.mds its engine would look up by name.
      const dotDot = await dotDotOutOfLink(dir, true);

      const [nativeAlias, nativeDotDot] = await compileFileOutcomes('native', [alias, dotDot]);
      const [wasmAlias, wasmDotDot] = await compileFileOutcomes('wasm', [alias, dotDot]);
      assert.equal(nativeAlias.output, 'X\nY\nX\n', JSON.stringify(nativeAlias));
      assert.deepEqual(wasmAlias, nativeAlias);
      assert.equal(nativeDotDot.output, 'ROOT-Z\nY=\nDEEP-Z\n', JSON.stringify(nativeDotDot));
      assert.equal(wasmDotDot.code, 'mds::import', JSON.stringify(wasmDotDot));
      assert.equal(wasmDotDot.output, undefined, JSON.stringify(wasmDotDot));
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

  test(
    'U-SM25: a symlink loop in a directory (ELOOP) is file-not-found, as native reports it',
    { skip: process.platform === 'win32' && 'a directory symlink loop needs a symlink privilege on Windows' },
    async (t) => {
      await withProject(async (dir) => {
        await symlink(path.join(dir, 'loop'), path.join(dir, 'loop'), 'dir');
        const entry = path.join(dir, 'loop', 'x.mds');
        await assertNotFoundAsEntryAndImport(dir, entry, './loop/x.mds', 'U-SM25');

        const engines = await loadEngines();
        if (!requireEngines(t, engines, 'U-SM25')) return;
        const [native] = await compileFileOutcomes('native', [entry]);
        const [wasm] = await compileFileOutcomes('wasm', [entry]);
        assert.equal(native.code, 'mds::file_not_found', JSON.stringify(native));
        assert.equal(wasm.code, native.code, JSON.stringify({ wasm, native }));
        assert.equal(wasm.message, native.message, JSON.stringify({ wasm, native }));
      });
    },
  );

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
        const wasm = wasmOutcomes[i];
        assert.equal(native.code, 'mds::file_not_found', JSON.stringify(native));
        assert.equal(wasm.code, native.code, JSON.stringify({ wasm, native }));
        assert.equal(wasm.message, native.message, JSON.stringify({ wasm, native }));
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
        assert.equal(wasm.code, native.code, JSON.stringify({ wasm, native }));
        assert.equal(wasm.message, native.message, JSON.stringify({ wasm, native }));
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

        await writeFile(path.join(dir, 'main.mds'), '@import "./secret.mds" as s\n');
        const imported = await refusal(path.join(dir, 'main.mds'), 'U-SM28 import');
        assert.equal(imported.code, 'mds::io', imported.message);
        assert.equal(imported.message, 'cannot read ./secret.mds: EACCES');

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
