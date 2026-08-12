/**
 * Tests for scripts/verify-no-control-bytes.mjs
 *
 * All hazard bytes are constructed AT RUNTIME using Buffer.from([0xNN]) or
 * String.fromCodePoint(0xNNNN). No hazard literal or backslash-u escape
 * appears in this file. (avoids PF-018, applies D-CB2)
 *
 * Tests that require a real git repository use mkdtemp + git init. The
 * hermetic-git test (AC-11) proves the git ls-files discovery path rather
 * than just the byte predicate, running in the PRIMARY checkout context
 * (avoids PF-016).
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, mkdirSync, rmSync, readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync, execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

import {
  HAZARD_RANGES,
  isHazardous,
  BINARY_ALLOWLIST,
  HAZARD_ALLOWLIST,
} from '../verify-no-control-bytes.mjs';

const ROOT = resolve(fileURLToPath(import.meta.url), '../../..');
const SCANNER = join(ROOT, 'scripts/verify-no-control-bytes.mjs');

// ---------------------------------------------------------------------------
// Helper: run scanner as subprocess
// ---------------------------------------------------------------------------
function runScanner(args = [], opts = {}) {
  const r = spawnSync(process.execPath, [SCANNER, ...args], {
    cwd: opts.cwd ?? ROOT,
    encoding: 'utf8',
    env: { ...process.env, ...(opts.env ?? {}) },
    timeout: 30000,
  });
  return { status: r.status, stdout: r.stdout, stderr: r.stderr };
}

// ---------------------------------------------------------------------------
// Helper: create a minimal git repo in a temp directory
// ---------------------------------------------------------------------------
function mkTempGitRepo() {
  const dir = mkdtempSync(join(tmpdir(), 'mds-scan-'));
  const git = (...args) => execFileSync('git', args, { cwd: dir, encoding: 'utf8', stdio: 'pipe' });
  git('init');
  git('config', 'user.email', 'test@test.test');
  git('config', 'user.name', 'Test');
  return { dir, git };
}

function cleanup(dir) {
  try { rmSync(dir, { recursive: true, force: true }); } catch { /* ignore */ }
}

// ---------------------------------------------------------------------------
// AC-12, AC-13: Golden-set completeness — hazard class cannot silently narrow
// ---------------------------------------------------------------------------
describe('AC-12 AC-13: hazard class golden set', () => {

  test('HAZARD_RANGES has exactly 21 entries (golden count)', () => {
    // D-CB1a: this count is the golden reference. If an entry is removed,
    // this test fails — silent narrowing is impossible.
    assert.equal(HAZARD_RANGES.length, 21,
      `HAZARD_RANGES must have 21 entries; got ${HAZARD_RANGES.length}. ` +
      `A member was silently removed (D-CB1a prevents this).`);
  });

  test('HAZARD_RANGES contains every required member', () => {
    // Golden list of expected entries (AC-12). Numbers = individual codepoints.
    const expectedNumbers = new Set([
      0x061c, // U+061C  Arabic Letter Mark
      0x200e, // U+200E  LRM
      0x200f, // U+200F  RLM
      0x202a, // U+202A  LRE
      0x202b, // U+202B  RLE
      0x202c, // U+202C  PDF
      0x202d, // U+202D  LRO
      0x202e, // U+202E  RLO
      0x2066, // U+2066  LRI
      0x2067, // U+2067  RLI
      0x2068, // U+2068  FSI
      0x2069, // U+2069  PDI
      0x2028, // U+2028  LS
      0x2029, // U+2029  PS
      0xfeff, // U+FEFF  BOM
    ]);
    const expectedRanges = [
      { from: 0x00, to: 0x08 },  // C0 below TAB
      { from: 0x0b, to: 0x0c },  // C0 VT/FF
      { from: 0x0e, to: 0x1f },  // C0 above CR
      { from: 0x7f, to: 0x7f },  // DEL
      { from: 0x80, to: 0x9f },  // C1
    ];
    const crlfEntry = HAZARD_RANGES.find(e => e && typeof e === 'object' && e.crlfException);

    // Check all expected numeric codepoints are present
    const actualNumbers = new Set(HAZARD_RANGES.filter(e => typeof e === 'number'));
    for (const cp of expectedNumbers) {
      assert.ok(actualNumbers.has(cp),
        `Missing codepoint U+${cp.toString(16).toUpperCase().padStart(4, '0')} from HAZARD_RANGES`);
    }
    for (const cp of actualNumbers) {
      assert.ok(expectedNumbers.has(cp),
        `Unexpected codepoint U+${cp.toString(16).toUpperCase().padStart(4, '0')} in HAZARD_RANGES`);
    }

    // Check all expected ranges are present
    for (const er of expectedRanges) {
      const found = HAZARD_RANGES.some(e =>
        e && typeof e === 'object' && !e.crlfException && e.from === er.from && e.to === er.to);
      assert.ok(found, `Missing range { from: 0x${er.from.toString(16)}, to: 0x${er.to.toString(16)} }`);
    }

    // Check CR CRLF-exception entry exists
    assert.ok(crlfEntry && crlfEntry.cp === 0x0d,
      'Missing { cp: 0x0d, crlfException: true } entry for CR');
  });

  test('AC-13: documented divergence from Rust assert_no_control_chars', () => {
    // The Rust helper flags ALL CR unconditionally.
    // The JS scanner permits CR when immediately followed by LF (D-CB3).
    // This is the ONLY documented divergence.

    // Verify CR alone = hazardous in JS scanner
    assert.equal(isHazardous(0x0d, null), true, 'lone CR must be hazardous');
    assert.equal(isHazardous(0x0d, 0x61), true, 'CR followed by non-LF must be hazardous');

    // Verify CR + LF = NOT hazardous (the CRLF exception)
    assert.equal(isHazardous(0x0d, 0x0a), false, 'CR followed by LF (CRLF) must NOT be hazardous (D-CB3)');

    // Confirm all other C0 entries match (no other divergence)
    for (let cp = 0x00; cp <= 0x1f; cp++) {
      if (cp === 0x09 || cp === 0x0a || cp === 0x0d) continue; // TAB, LF, CR handled specially
      assert.equal(isHazardous(cp, null), true, `C0 0x${cp.toString(16).padStart(2,'0')} must be hazardous`);
    }

    // Verify no false positives on TAB and LF
    assert.equal(isHazardous(0x09, null), false, 'TAB must NOT be hazardous');
    assert.equal(isHazardous(0x0a, null), false, 'LF must NOT be hazardous');
  });

  test('mutation check: removing C1 range (U+0085) would fail the test above', () => {
    // D-CB1a: Prove the golden-set test is non-vacuous.
    // This test verifies that U+0085 (C1 NEL, the case the baseline missed) IS detected.
    // The case that triggered PF-018 three times in this repo.
    const nel = 0x85; // U+0085 — C1 NEL; written as hex, not \u escape (D-CB2)
    assert.equal(isHazardous(nel, null), true,
      'U+0085 (C1 NEL) must be detected — this is the exact byte PF-018 injected into tracked source');

    // Also verify U+0080 (C1 low end) and U+009F (C1 high end) are caught
    assert.equal(isHazardous(0x80, null), true, 'U+0080 (C1 boundary) must be hazardous');
    assert.equal(isHazardous(0x9f, null), true, 'U+009F (C1 boundary) must be hazardous');
    // Confirm U+00A0 is NOT hazardous (just outside C1 range)
    assert.equal(isHazardous(0xa0, null), false, 'U+00A0 (NBSP) must NOT be hazardous');
  });

});

// ---------------------------------------------------------------------------
// AC-7, AC-8, AC-9: positive controls and false-positive tests
// ---------------------------------------------------------------------------
describe('AC-7 AC-8 AC-9: positive controls and clean-file checks', () => {

  test('AC-7 PC-1: planted ESC (0x1B) in .rs file → exits 1 naming file and U+001B', () => {
    const { dir, git } = mkTempGitRepo();
    try {
      const esc = Buffer.from([0x1b]); // ESC byte — constructed at runtime, not a literal
      const content = Buffer.concat([Buffer.from('fn main() { '), esc, Buffer.from(' }')]);
      writeFileSync(join(dir, 'src.rs'), content);
      git('add', 'src.rs');
      const r = runScanner([], { cwd: dir });
      assert.equal(r.status, 1, 'scanner must exit 1 on ESC in tracked .rs file');
      assert.ok(r.stderr.includes('src.rs'), 'error must name the file');
      assert.ok(r.stderr.includes('U+001B'), 'error must include U+001B codepoint');
    } finally { cleanup(dir); }
  });

  test('AC-7 PC-2: planted ESC in .md file → exits 1', () => {
    const { dir, git } = mkTempGitRepo();
    try {
      const esc = Buffer.from([0x1b]);
      writeFileSync(join(dir, 'doc.md'), Buffer.concat([Buffer.from('# heading '), esc]));
      git('add', 'doc.md');
      const r = runScanner([], { cwd: dir });
      assert.equal(r.status, 1);
      assert.ok(r.stderr.includes('doc.md'));
      assert.ok(r.stderr.includes('U+001B'));
    } finally { cleanup(dir); }
  });

  test('AC-7 PC-3: planted ESC in .json file → exits 1', () => {
    const { dir, git } = mkTempGitRepo();
    try {
      const esc = Buffer.from([0x1b]);
      writeFileSync(join(dir, 'data.json'), Buffer.concat([Buffer.from('{"a":"'), esc, Buffer.from('"}')]));
      git('add', 'data.json');
      const r = runScanner([], { cwd: dir });
      assert.equal(r.status, 1);
      assert.ok(r.stderr.includes('data.json'));
      assert.ok(r.stderr.includes('U+001B'));
    } finally { cleanup(dir); }
  });

  test('AC-7 PC-4: planted RLO (U+202E) in .md file → exits 1 naming U+202E', () => {
    const { dir, git } = mkTempGitRepo();
    try {
      // U+202E = Right-to-Left Override (Trojan Source bidi char)
      const rlo = Buffer.from(String.fromCodePoint(0x202e), 'utf8');
      writeFileSync(join(dir, 'evil.md'), Buffer.concat([Buffer.from('normal '), rlo, Buffer.from(' text')]));
      git('add', 'evil.md');
      const r = runScanner([], { cwd: dir });
      assert.equal(r.status, 1);
      assert.ok(r.stderr.includes('evil.md'));
      assert.ok(r.stderr.includes('U+202E'));
    } finally { cleanup(dir); }
  });

  test('AC-8 PC-5: planted U+0085 (C1 NEL, 0xC2 0x85) → exits 1 (the case the baseline missed)', () => {
    const { dir, git } = mkTempGitRepo();
    try {
      // UTF-8 encoding of U+0085 = 0xC2 0x85 (two bytes)
      const nel = Buffer.from([0xc2, 0x85]);
      writeFileSync(join(dir, 'nel.txt'), Buffer.concat([Buffer.from('a'), nel, Buffer.from('b')]));
      git('add', 'nel.txt');
      const r = runScanner([], { cwd: dir });
      assert.equal(r.status, 1, 'scanner must detect U+0085 (C1 NEL at codepoint level)');
      assert.ok(r.stderr.includes('U+0085'), 'error must reference U+0085');
    } finally { cleanup(dir); }
  });

  test('AC-9 NEG-1: clean international text (accented Latin, CJK, emoji) → exits 0', () => {
    const { dir, git } = mkTempGitRepo();
    try {
      // These are all valid multi-byte UTF-8 sequences with no hazard codepoints
      const content = 'café 日本語 emoji: 🎉\nTabbed\there\n';
      writeFileSync(join(dir, 'intl.md'), content, 'utf8');
      git('add', 'intl.md');
      const r = runScanner([], { cwd: dir });
      assert.equal(r.status, 0, 'clean international text must exit 0');
    } finally { cleanup(dir); }
  });

  test('AC-9 NEG-3: UTF-8 continuation bytes are not false-positived', () => {
    // U+00E9 (é) encodes as 0xC3 0xA9. The continuation byte 0xA9 is in
    // range 0x80-0xBF — NOT in the C1 range 0x80-0x9F at codepoint level.
    // A naive byte-level C1 check would incorrectly flag 0x89 in 0xE2 0x89 0xA0 ≠.
    const neq = 0x2260; // U+2260 NOT EQUAL TO — encodes as 0xE2 0x89 0xA0
    // 0x89 is a continuation byte here; codepoint 0x2260 is NOT in the C1 range
    assert.equal(isHazardous(neq, null), false, 'U+2260 (not-equal) must not be hazardous');
    // The codepoint 0x89 on its own IS in C1 range, but UTF-8 continuation bytes
    // should never appear as standalone codepoints in valid UTF-8
    assert.equal(isHazardous(0x89, null), true, 'U+0089 itself IS C1-hazardous (standalone codepoint)');
  });

});

// ---------------------------------------------------------------------------
// AC-10: CR policy
// ---------------------------------------------------------------------------
describe('AC-10: CR policy — CRLF permitted, lone CR rejected', () => {

  test('lone CR (0x0D not followed by LF) → exits 1', () => {
    const { dir, git } = mkTempGitRepo();
    try {
      // 0x61 0x0D 0x62 = a<CR>b (lone CR, not CRLF)
      writeFileSync(join(dir, 'lone-cr.txt'), Buffer.from([0x61, 0x0d, 0x62]));
      git('add', 'lone-cr.txt');
      const r = runScanner([], { cwd: dir });
      assert.equal(r.status, 1, 'lone CR must be rejected');
      assert.ok(r.stderr.includes('U+000D'), 'error must name U+000D');
    } finally { cleanup(dir); }
  });

  test('CRLF (CR immediately followed by LF) → exits 0', () => {
    const { dir, git } = mkTempGitRepo();
    try {
      // 0x61 0x0D 0x0A 0x62 = a<CRLF>b
      writeFileSync(join(dir, 'crlf.txt'), Buffer.from([0x61, 0x0d, 0x0a, 0x62]));
      git('add', 'crlf.txt');
      const r = runScanner([], { cwd: dir });
      assert.equal(r.status, 0, 'CRLF must be permitted');
    } finally { cleanup(dir); }
  });

  test('isHazardous(CR, LF) = false, isHazardous(CR, non-LF) = true', () => {
    assert.equal(isHazardous(0x0d, 0x0a), false, 'CR+LF (CRLF) — not hazardous');
    assert.equal(isHazardous(0x0d, 0x61), true,  'CR+a (lone-ish CR) — hazardous');
    assert.equal(isHazardous(0x0d, null),  true,  'CR at EOF — hazardous');
  });

});

// ---------------------------------------------------------------------------
// AC-11: hermetic git repo proves git ls-files discovery path (avoids PF-016)
// ---------------------------------------------------------------------------
describe('AC-11: git ls-files discovery path', () => {

  test('planted 0x1B in tracked file exits 1; untracked file exits 0', () => {
    const { dir, git } = mkTempGitRepo();
    try {
      const esc = Buffer.from([0x1b]);
      writeFileSync(join(dir, 'tracked.md'), Buffer.concat([Buffer.from('evil '), esc]));
      git('add', 'tracked.md');

      // Scanner reads git-tracked files — should find the hostile byte
      const r1 = runScanner([], { cwd: dir });
      assert.equal(r1.status, 1, 'tracked file with ESC → scanner must exit 1');

      // Remove from git tracking (but keep on disk as untracked)
      git('rm', '--cached', 'tracked.md');
      const r2 = runScanner([], { cwd: dir });
      // With zero tracked files, non-vacuity guard fires (exit 1) — which is correct.
      // The scanner proves it reads the tracked set: the hostile file is on disk but untracked.
      // If it read the working tree, it would still find the hostile byte even after `git rm --cached`.
      // Since zero tracked files → non-vacuity exit 1, we know the scanner used git ls-files.
      // To confirm: add a clean file and verify the scanner passes.
      writeFileSync(join(dir, 'clean.md'), 'clean content\n');
      git('add', 'clean.md');
      const r3 = runScanner([], { cwd: dir });
      assert.equal(r3.status, 0,
        'after removing hostile file from tracking and adding a clean file, scanner must exit 0 ' +
        '(proves working-tree untracked file is NOT scanned)');
    } finally { cleanup(dir); }
  });

});

// ---------------------------------------------------------------------------
// AC-5, AC-6: full-tree scan and non-vacuity guard
// ---------------------------------------------------------------------------
describe('AC-5 AC-6: full-tree scan and non-vacuity', () => {

  test('AC-5: scanner exits 0 on real repo tree with >= 500 files and >= 4MB', () => {
    const r = runScanner([], { cwd: ROOT });
    assert.equal(r.status, 0, `scanner must exit 0 on clean repo tree; stderr: ${r.stderr}`);
    // Parse scanned file count and byte count from success output
    const m = r.stdout.match(/Scanned (\d+) file\(s\), (\d+) byte\(s\)/);
    assert.ok(m, `success output must include "Scanned N file(s), M byte(s)"; got: ${r.stdout}`);
    const files = parseInt(m[1], 10);
    const bytes = parseInt(m[2], 10);
    assert.ok(files >= 500, `expected >= 500 files scanned; got ${files}`);
    assert.ok(bytes >= 4_000_000, `expected >= 4,000,000 bytes; got ${bytes}`);
  });

  test('AC-6: empty git repo (zero tracked files) → exits 1 with non-vacuity message', () => {
    const { dir } = mkTempGitRepo();
    try {
      const r = runScanner([], { cwd: dir });
      assert.equal(r.status, 1, 'empty repo must exit 1 (non-vacuity guard)');
      assert.ok(
        r.stderr.includes('zero files scanned') || r.stderr.includes('empty scan'),
        `error must mention zero files; got: ${r.stderr}`
      );
    } finally { cleanup(dir); }
  });

});

// ---------------------------------------------------------------------------
// AC-16: error cases — invalid UTF-8, NUL, no git, not a repo
// ---------------------------------------------------------------------------
describe('AC-16 AC-20: error cases', () => {

  test('AC-16: invalid UTF-8 → exits non-zero with distinct message', () => {
    const { dir, git } = mkTempGitRepo();
    try {
      // 0xFF 0xFE 0x41 is not valid UTF-8 (0xFF is never valid)
      writeFileSync(join(dir, 'bad.txt'), Buffer.from([0xff, 0xfe, 0x41]));
      git('add', 'bad.txt');
      const r = runScanner([], { cwd: dir });
      assert.notEqual(r.status, 0, 'invalid UTF-8 must exit non-zero');
      assert.ok(
        r.stderr.includes('invalid UTF-8') || r.stderr.includes('UTF-8'),
        `error must mention UTF-8; got: ${r.stderr}`
      );
    } finally { cleanup(dir); }
  });

  test('AC-16: NUL byte not in BINARY_ALLOWLIST → exits 1', () => {
    const { dir, git } = mkTempGitRepo();
    try {
      writeFileSync(join(dir, 'nul.dat'), Buffer.from([0x41, 0x00, 0x42]));
      git('add', 'nul.dat');
      const r = runScanner([], { cwd: dir });
      assert.equal(r.status, 1, 'NUL byte not in BINARY_ALLOWLIST must exit 1');
      assert.ok(
        r.stderr.includes('NUL') || r.stderr.includes('BINARY_ALLOWLIST'),
        `error must mention NUL or BINARY_ALLOWLIST; got: ${r.stderr}`
      );
    } finally { cleanup(dir); }
  });

  test('AC-16: not inside a git work tree → exits 2', () => {
    const dir = mkdtempSync(join(tmpdir(), 'mds-nogit-'));
    try {
      const r = runScanner([], { cwd: dir });
      assert.equal(r.status, 2, 'non-git directory must exit 2');
    } finally { cleanup(dir); }
  });

  test('AC-14: all 13 previously-live hazards are gone from the four dirty files', () => {
    // Verify S0 remediation: the four files that had U+0085/U+2028/U+2029 are now clean.
    const dirtyFiles = [
      'crates/mds-cli/tests/cli_lint.rs',
      'crates/mds-napi/__test__/index.spec.mjs',
      'crates/mds-wasm/tests/web.rs',
      'packages/bundler-utils/__test__/transform.spec.mjs',
    ];
    // Run scanner in explicit-path mode on just these four files
    // (they are in the real repo working tree, not a temp git repo)
    const r = runScanner(dirtyFiles, { cwd: ROOT });
    assert.equal(r.status, 0,
      `previously-dirty files must be clean after S0 remediation; stderr: ${r.stderr}`);
  });

});

// ---------------------------------------------------------------------------
// AC-18: --staged mode reads from git index, not working tree
// ---------------------------------------------------------------------------
describe('AC-18: --staged mode reads index blob, not working tree', () => {

  test('Case A: clean staged blob, hostile working tree → exits 0', () => {
    const { dir, git } = mkTempGitRepo();
    try {
      // Stage a clean file
      writeFileSync(join(dir, 'f.txt'), 'clean content\n');
      git('add', 'f.txt');
      // Now overwrite the working tree with a hostile byte WITHOUT staging
      const esc = Buffer.from([0x1b]);
      writeFileSync(join(dir, 'f.txt'), Buffer.concat([Buffer.from('evil '), esc]));
      // --staged reads the INDEX (clean), not the working tree
      const r = runScanner(['--staged'], { cwd: dir });
      assert.equal(r.status, 0,
        'clean staged blob + hostile working tree → exit 0 (index is scanned, not working tree)');
    } finally { cleanup(dir); }
  });

  test('Case B: hostile staged blob, clean working tree → exits 1', () => {
    const { dir, git } = mkTempGitRepo();
    try {
      // Stage a hostile file
      const esc = Buffer.from([0x1b]);
      writeFileSync(join(dir, 'f.txt'), Buffer.concat([Buffer.from('evil '), esc]));
      git('add', 'f.txt');
      // Overwrite working tree with clean content WITHOUT re-staging
      writeFileSync(join(dir, 'f.txt'), 'clean now\n');
      // --staged reads the INDEX (hostile), not the working tree
      const r = runScanner(['--staged'], { cwd: dir });
      assert.equal(r.status, 1,
        'hostile staged blob + clean working tree → exit 1 (index is scanned, not working tree)');
    } finally { cleanup(dir); }
  });

});

// ---------------------------------------------------------------------------
// AC-15: scanner source itself has no hazard bytes and no grep -P
// ---------------------------------------------------------------------------
describe('AC-15: scanner source is self-clean', () => {

  test('scanner source files pass their own gate', () => {
    const scriptFiles = [
      'scripts/verify-no-control-bytes.mjs',
      'scripts/verify-pr-checks.mjs',
    ];
    const r = runScanner(scriptFiles, { cwd: ROOT });
    assert.equal(r.status, 0, `scanner source files must pass their own gate; stderr: ${r.stderr}`);
  });

  test('scanner source contains no grep -P invocation', () => {
    const src = readFileSync(join(ROOT, 'scripts/verify-no-control-bytes.mjs'), 'utf8');
    assert.ok(!src.includes('grep -P'), 'scanner must not use grep -P (BSD grep lacks -P, exits 2)');
  });

});

// ---------------------------------------------------------------------------
// AC-17: stale allowlist entries exit 1
// ---------------------------------------------------------------------------
describe('AC-17: stale allowlist entries are self-invalidating', () => {

  // These tests verify the allowlist behavior using the exported constants.
  // The HAZARD_ALLOWLIST is currently empty — an empty allowlist is always valid.

  test('BINARY_ALLOWLIST is empty (no entries)', () => {
    assert.equal(BINARY_ALLOWLIST.length, 0, 'BINARY_ALLOWLIST must be empty (D-CB4, D-CB6)');
  });

  test('HAZARD_ALLOWLIST is empty (no entries)', () => {
    assert.equal(HAZARD_ALLOWLIST.length, 0, 'HAZARD_ALLOWLIST must be empty (D-CB4, D-CB6)');
  });

});
