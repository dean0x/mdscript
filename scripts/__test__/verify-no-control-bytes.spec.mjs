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
import { mkdtempSync, writeFileSync, rmSync, readFileSync, symlinkSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync, execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

import {
  HAZARD_RANGES,
  isHazardous,
  scanBuffer,
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

  test('C1 range covers U+0080-U+009F including NEL (U+0085), and stops at U+00A0', () => {
    // AC-12: The C1 range { from: 0x80, to: 0x9f } must cover all codepoints in
    // that band, including U+0085 (C1 NEL) — the exact byte PF-018 injected into
    // tracked source three times in this repo.
    //
    // The genuine non-vacuity guard for D-CB1a is the golden-count test at line 67
    // (exactly 21 entries) and the bidirectional membership assertions above (lines
    // 104-124); those tests catch both removal and narrowing. This test documents
    // the boundary behaviour of the C1 range specifically.
    const nel = 0x85; // U+0085 — C1 NEL; written as hex, not backslash-u (D-CB2)
    assert.equal(isHazardous(nel, null), true,
      'U+0085 (C1 NEL) must be detected — this is the exact byte PF-018 injected into tracked source');

    // Verify U+0080 (C1 low end) and U+009F (C1 high end) are caught
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

  test('AC-7 PC-6: planted U+FEFF (BOM) at offset 0 → exits 1 naming U+FEFF', () => {
    // U+FEFF is the most likely codepoint to be special-cased by a future
    // change to the decoder (BOM-stripping is a common optimization) and must
    // therefore be exercised end-to-end through the full decode → predicate →
    // report pipeline, not only via the table-driven isHazardous unit tests.
    //
    // UTF-8 encoding of U+FEFF: 0xEF 0xBB 0xBF (3 bytes).
    // Bytes constructed at runtime from hex — no backslash-u escape (PF-018).
    const { dir, git } = mkTempGitRepo();
    try {
      const bom = Buffer.from([0xef, 0xbb, 0xbf]);
      writeFileSync(join(dir, 'bom.mjs'), Buffer.concat([bom, Buffer.from('export const x = 1;')]));
      git('add', 'bom.mjs');
      const r = runScanner([], { cwd: dir });
      assert.equal(r.status, 1, 'scanner must exit 1 on U+FEFF (BOM) at offset 0');
      assert.ok(r.stderr.includes('bom.mjs'), 'error must name the file');
      assert.ok(r.stderr.includes('U+FEFF'), 'error must include U+FEFF codepoint');
    } finally { cleanup(dir); }
  });

  test('AC-7 PC-7: planted U+2028 (Line Separator) mid-file → exits 1 naming U+2028', () => {
    // U+2028 (LINE SEPARATOR) is the second codepoint most likely to be
    // special-cased by a future change to scanBuffer (it looks like a line
    // break and some decoders treat it as whitespace).  Exercise it
    // end-to-end through the full pipeline.
    //
    // UTF-8 encoding of U+2028: 0xE2 0x80 0xA8 (3 bytes).
    // Bytes constructed at runtime from hex — no backslash-u escape (PF-018).
    const { dir, git } = mkTempGitRepo();
    try {
      const ls = Buffer.from([0xe2, 0x80, 0xa8]);
      writeFileSync(join(dir, 'ls.md'), Buffer.concat([Buffer.from('before'), ls, Buffer.from('after')]));
      git('add', 'ls.md');
      const r = runScanner([], { cwd: dir });
      assert.equal(r.status, 1, 'scanner must exit 1 on U+2028 (Line Separator) mid-file');
      assert.ok(r.stderr.includes('ls.md'), 'error must name the file');
      assert.ok(r.stderr.includes('U+2028'), 'error must include U+2028 codepoint');
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
      // Zero tracked files → non-vacuity guard fires (exit 1), proving the scanner
      // reads the git-tracked set rather than the working tree directory.
      // The hostile file is still on disk but is now untracked; a working-tree
      // scanner would still find it and exit 1 for the wrong reason.
      assert.equal(r2.status, 1, 'zero tracked files (hostile file on disk but untracked) → non-vacuity guard exits 1');
      assert.ok(
        r2.stderr.includes('zero files scanned') || r2.stderr.includes('empty scan'),
        `non-vacuity message must mention zero files; got: ${r2.stderr}`
      );
      // Add a clean tracked file; hostile file is still on disk but untracked.
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

  test('AC-5 AC-30: scanner exits 0 on real repo tree with >= 500 files and >= 4MB in under 5 s', () => {
    // AC-30 (clause a): full tracked-tree scan must complete in under 5 seconds wall-clock.
    // A generous CI-safe bound of 5 s is used; local runs are typically < 1 s.
    const start = Date.now();
    const r = runScanner([], { cwd: ROOT });
    const elapsed = Date.now() - start;
    assert.equal(r.status, 0, `scanner must exit 0 on clean repo tree; stderr: ${r.stderr}`);
    assert.ok(elapsed < 5000,
      `full-tree scan must complete in < 5 s wall-clock (AC-30); took ${elapsed}ms`);
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

  test('AC-6 --staged: no staged content (amend/nothing staged) → exits 0 with explicit message', () => {
    // D-CB5's non-vacuity guard applies to full-tree mode only. In --staged mode
    // an empty ACMR-filtered set is a legitimate state — `git commit --amend
    // --no-edit` and `--allow-empty` produce exactly this. Exiting 1 here blocks
    // valid commits and trains contributors to reach for --no-verify, which
    // disables the gate for ALL commits. Fix: exit 0 with an explicit message.
    const { dir } = mkTempGitRepo();
    try {
      // Nothing staged — `git diff --cached` returns empty.
      const r = runScanner(['--staged'], { cwd: dir });
      assert.equal(r.status, 0, '--staged with nothing staged must exit 0 (no content to scan)');
      assert.ok(
        r.stdout.includes('nothing to scan') || r.stdout.includes('no staged'),
        `stdout must explain why scanning was skipped; got stdout: ${r.stdout}`
      );
    } finally { cleanup(dir); }
  });

  test('AC-6 --staged: deletion-only commit → exits 0 with deletion count', () => {
    // A commit that removes files only (git rm) yields zero ACMR-filtered paths
    // because D = deletion is excluded from the ACMR filter. The scanner must
    // exit 0, not 1. D-CB5 non-vacuity applies to full-tree mode only.
    const { dir, git } = mkTempGitRepo();
    try {
      // Commit a clean file, then stage its deletion
      writeFileSync(join(dir, 'to-delete.md'), 'content\n');
      git('add', 'to-delete.md');
      git('commit', '-m', 'add file');
      git('rm', 'to-delete.md');
      // The deletion is staged; ACMR filter excludes it → zero content-bearing paths
      const r = runScanner(['--staged'], { cwd: dir });
      assert.equal(r.status, 0, 'deletion-only staged set must exit 0');
      assert.ok(
        r.stdout.includes('deletion') || r.stdout.includes('nothing to scan'),
        `stdout must explain why scanning was skipped; got stdout: ${r.stdout}`
      );
    } finally { cleanup(dir); }
  });

  test('AC-6 --staged: amend-message-only (prior commit + nothing newly staged) → exits 0', () => {
    // Regression for the empirically-confirmed bug: `git commit --amend -m "..."` was
    // rejected with exit 1 by the pre-commit hook. During a message-only amend the
    // index is identical to HEAD, so `git diff --cached --diff-filter=ACMR` returns
    // zero paths. The non-vacuity guard (D-CB5) MUST NOT fire in --staged mode for
    // this legitimate git state.
    //
    // This test uses a repo with a real prior commit (unlike the fresh-repo test above)
    // to faithfully simulate the amend scenario where HEAD exists.
    const { dir, git } = mkTempGitRepo();
    try {
      // Establish a prior commit — this is the HEAD that an amend would rewrite.
      writeFileSync(join(dir, 'initial.md'), 'initial content\n');
      git('add', 'initial.md');
      git('commit', '-m', 'initial commit');
      // Index == HEAD: `git diff --cached` returns nothing. Simulates --amend -m.
      const r = runScanner(['--staged'], { cwd: dir });
      assert.equal(r.status, 0,
        '--staged with index == HEAD must exit 0 (amend -m "..." is a valid workflow)');
      assert.ok(
        r.stdout.includes('nothing to scan') || r.stdout.includes('no staged'),
        `stdout must explain why scanning was skipped; got: ${r.stdout}`
      );
    } finally { cleanup(dir); }
  });

  test('AC-6 --staged: allow-empty commit (nothing staged, prior commit) → exits 0', () => {
    // Regression for the empirically-confirmed bug: `git commit --allow-empty` was
    // rejected with exit 1 by the pre-commit hook. An allow-empty commit intentionally
    // carries no staged content; `git diff --cached --diff-filter=ACMR` returns zero
    // paths, which is a LEGITIMATE state. The scanner must exit 0 with an explicit
    // message so the contributor knows the gate ran and found nothing to check.
    const { dir, git } = mkTempGitRepo();
    try {
      // Establish a prior commit so HEAD exists (matching typical allow-empty usage).
      writeFileSync(join(dir, 'initial.md'), 'initial content\n');
      git('add', 'initial.md');
      git('commit', '-m', 'initial commit');
      // Nothing staged — simulates `git commit --allow-empty`.
      const r = runScanner(['--staged'], { cwd: dir });
      assert.equal(r.status, 0,
        '--staged with nothing staged must exit 0 (allow-empty is a valid workflow)');
      assert.ok(
        r.stdout.includes('nothing to scan') || r.stdout.includes('no staged'),
        `stdout must explain why scanning was skipped; got: ${r.stdout}`
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

  test('AC-16: not inside a git work tree → exits 1 (fail-closed)', () => {
    // AC-16 mandates exit 1 for all four named failure conditions. "Not a git
    // work tree" is a known, named failure — it is fail-closed (exit 1), not
    // indeterminate (exit 2).
    const dir = mkdtempSync(join(tmpdir(), 'mds-nogit-'));
    try {
      const r = runScanner([], { cwd: dir });
      assert.equal(r.status, 1, 'non-git directory must exit 1 (fail-closed)');
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
// AC-15: scanner source itself has no hazard bytes and no PCRE grep flag
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

  test('scanner source files contain no forbidden grep flag and no backslash-u escape (AC-15, PF-018)', () => {
    // AC-15: the scripts, spec files, and hook must not invoke the BSD-incompatible
    // grep PCRE flag (BSD grep lacks it, exits 2 with empty output that reads as
    // clean — D-CB7), and must not contain a backslash-u-plus-4-hex escape (the
    // edit-tooling decode vector that injected live hazard bytes into this repo
    // three times — PF-018, D-CB2).
    //
    // Both search patterns are built from parts / numeric char codes so this test
    // does not trip its own rule when the spec file is in the checked file set.
    // The forbidden grep invocation is 'grep' joined with ' -P'; split here so
    // the contiguous substring is absent from this source file.
    const grepPFlag = 'grep' + ' -P';
    // 0x5C = backslash, then 'u' and 4 hex digits:
    const bs = String.fromCodePoint(0x5c);
    const bsUPattern = new RegExp(bs + 'u[0-9a-fA-F]{4}');

    const fileSet = [
      'scripts/verify-no-control-bytes.mjs',
      'scripts/verify-pr-checks.mjs',
      'scripts/__test__/verify-no-control-bytes.spec.mjs',
      'scripts/__test__/verify-pr-checks.spec.mjs',
      'scripts/hooks/pre-commit',
    ];

    for (const rel of fileSet) {
      const src = readFileSync(join(ROOT, rel), 'utf8');
      assert.ok(
        !src.includes(grepPFlag),
        `${rel}: must not invoke the POSIX-extension grep flag (BSD grep lacks PCRE support, exits 2 — AC-15, D-CB7)`
      );
      assert.ok(
        !bsUPattern.test(src),
        `${rel}: must not contain a backslash-u escape (edit tooling decodes them into live bytes — AC-15, PF-018)`
      );
    }
  });

});

// ---------------------------------------------------------------------------
// AC-30: hex context stored per hit as a pre-computed string, not as the raw
//        buffer — one-file-at-a-time memory discipline, verified by code shape.
//
// AC-30 has three clauses:
//   (a) Wall-clock full-tree scan < 5 s — asserted with Date.now() in the
//       AC-5 test above (generous CI-safe bound).
//   (b) --staged mode < 2 s for a 20-file commit — not directly timed here;
//       the same structural bound holds (git cat-file reads one blob at a time).
//   (c) MUST NOT hold more than one file's contents in memory at a time —
//       verified by code shape: at verify-no-control-bytes.mjs:473,
//       hazardHits.push stores { hexCtx } (a pre-computed string) not { buf }
//       (the raw buffer), so the buffer is GC-eligible after each iteration.
//
// This describe block tests clause (c) indirectly: by proving the correct
// hexCtx string reaches the output across multiple files, it demonstrates
// that hexCtx was computed and stored before buf went out of scope — which
// is only possible if buf was NOT retained in hazardHits.
// ---------------------------------------------------------------------------
describe('AC-30: hex context stored as string per hit, not as file buffer', () => {

  test('scanner reports hex context for every hazard across multiple files', () => {
    // Verify that hexCtx is computed and stored correctly for each hit.
    // Memory discipline (clause c) is by code shape: hazardHits stores { hexCtx }
    // not { buf } (scanner:473), so buf is GC-eligible after each file's iteration.
    // This test proves the correct context string reaches the output regardless
    // of how many files are scanned.
    const { dir, git } = mkTempGitRepo();
    try {
      // Construct two files each with an ESC at a known position
      // a.md: "hello " (6 bytes) then ESC — offset 6
      // b.md: "foo "   (4 bytes) then ESC — offset 4
      const esc = Buffer.from([0x1b]);
      writeFileSync(join(dir, 'a.md'), Buffer.concat([Buffer.from('hello '), esc, Buffer.from(' world')]));
      writeFileSync(join(dir, 'b.md'), Buffer.concat([Buffer.from('foo '), esc]));
      git('add', 'a.md', 'b.md');

      const r = runScanner([], { cwd: dir });
      assert.equal(r.status, 1, 'scanner must exit 1 for files with hazard bytes');
      // Both files must be named
      assert.ok(r.stderr.includes('a.md'), 'must report hazard in a.md');
      assert.ok(r.stderr.includes('b.md'), 'must report hazard in b.md');
      // Hex context lines must appear (proves hexCtx is pre-computed and stored)
      assert.ok(r.stderr.includes('context:'), 'must include hex context lines');
      // The codepoint must be identified
      assert.ok(r.stderr.includes('U+001B'), 'must name the hazardous codepoint');
    } finally { cleanup(dir); }
  });

});

// ---------------------------------------------------------------------------
// AC-17: allowlist entries are exercised-or-stale
// ---------------------------------------------------------------------------
describe('AC-17: allowlist entries are self-invalidating', () => {

  test('BINARY_ALLOWLIST is empty (no entries)', () => {
    assert.equal(BINARY_ALLOWLIST.length, 0, 'BINARY_ALLOWLIST must be empty (D-CB4, D-CB6)');
  });

  test('HAZARD_ALLOWLIST is empty (no entries)', () => {
    assert.equal(HAZARD_ALLOWLIST.length, 0, 'HAZARD_ALLOWLIST must be empty (D-CB4, D-CB6)');
  });

  test('scanBuffer reports allowlisted hazards as allowed, others as not', () => {
    // The allowlist is empty in the shipped file, so the exercised/stale
    // machinery below can only be reached by an entry that does not exist yet.
    // Assert the predicate the machinery depends on, then drive the whole
    // script with a patched allowlist in the integration cases that follow.
    const esc = Buffer.from([0x61, 0x1b, 0x62]);
    const notAllowed = scanBuffer(esc, 'x.md', new Set());
    assert.equal(notAllowed.length, 1);
    assert.equal(notAllowed[0].codepoint, 0x1b);
    assert.equal(notAllowed[0].allowed, false);

    const allowed = scanBuffer(esc, 'x.md', new Set([0x1b]));
    assert.equal(allowed.length, 1, 'an allowlisted hazard must still be REPORTED to the caller');
    assert.equal(allowed[0].allowed, true, 'so the entry can be recorded as exercised, not stale');
  });

  /**
   * Write a copy of the scanner with a patched HAZARD_ALLOWLIST into `dir`.
   * Patching the source is the only way to exercise a non-empty allowlist
   * while keeping the shipped allowlist empty (D-CB4).
   */
  function writePatchedScanner(dir, entryLiteral) {
    const marker = 'export const HAZARD_ALLOWLIST = [';
    const src = readFileSync(SCANNER, 'utf8');
    assert.ok(src.includes(marker), 'scanner must declare HAZARD_ALLOWLIST for this test to patch');
    const patched = src.replace(marker, `${marker} ${entryLiteral},`);
    assert.notEqual(patched, src, 'patch must have applied');
    const target = join(dir, 'scan.mjs');
    writeFileSync(target, patched);
    return target;
  }

  function runPatched(dir, target) {
    const r = spawnSync(process.execPath, [target], { cwd: dir, encoding: 'utf8', timeout: 30000 });
    return { status: r.status, stdout: r.stdout, stderr: r.stderr };
  }

  test('Case 1: an exercised entry exits 0 and is named in the output', () => {
    const { dir, git } = mkTempGitRepo();
    try {
      writeFileSync(join(dir, 'evil.md'), Buffer.concat([Buffer.from('x '), Buffer.from([0x1b])]));
      const target = writePatchedScanner(dir, "{ path: 'evil.md', codepoints: [0x1b], reason: 'test fixture' }");
      git('add', 'evil.md', 'scan.mjs');
      const r = runPatched(dir, target);
      assert.equal(r.status, 0, `exercised allowlist entry must pass; stderr: ${r.stderr}`);
      assert.ok(r.stdout.includes('evil.md'), `output must name the exercised entry; got: ${r.stdout}`);
      assert.ok(r.stdout.includes('U+001B'), `output must name the allowed codepoint; got: ${r.stdout}`);
      assert.ok(r.stdout.includes('test fixture'), `output must quote the written reason; got: ${r.stdout}`);
    } finally { cleanup(dir); }
  });

  test('Case 2: an entry naming a file that is not tracked exits 1 as stale', () => {
    const { dir, git } = mkTempGitRepo();
    try {
      writeFileSync(join(dir, 'clean.md'), 'nothing to see\n');
      const target = writePatchedScanner(dir, "{ path: 'gone.md', codepoints: [0x1b], reason: 'test fixture' }");
      git('add', 'clean.md', 'scan.mjs');
      const r = runPatched(dir, target);
      assert.equal(r.status, 1, 'an allowlist entry for an untracked path must fail');
      assert.ok(r.stderr.includes('stale'), `must identify the entry as stale; got: ${r.stderr}`);
      assert.ok(r.stderr.includes('gone.md'), 'must name the stale path');
    } finally { cleanup(dir); }
  });

  test('Case 3: an entry whose declared codepoint no longer occurs exits 1 as stale', () => {
    const { dir, git } = mkTempGitRepo();
    try {
      writeFileSync(join(dir, 'evil.md'), 'the hazard byte has since been removed\n');
      const target = writePatchedScanner(dir, "{ path: 'evil.md', codepoints: [0x1b], reason: 'test fixture' }");
      git('add', 'evil.md', 'scan.mjs');
      const r = runPatched(dir, target);
      assert.equal(r.status, 1, 'an allowlist entry whose codepoint is gone must fail');
      assert.ok(r.stderr.includes('U+001B'), `must name the declared codepoint; got: ${r.stderr}`);
    } finally { cleanup(dir); }
  });

});

// ---------------------------------------------------------------------------
// Entry-point guard: the gate must actually RUN wherever the repo is checked out
// ---------------------------------------------------------------------------
describe('the scanner runs from a path containing a space', () => {

  test('planted ESC is still caught when the script path contains a space', () => {
    // `import.meta.url === "file://" + process.argv[1]` is false for any path a
    // file URL percent-encodes, so main() never runs and the gate exits 0
    // having scanned nothing — a silent pass indistinguishable from a clean
    // tree. The scanner has no local imports, so a copy is a faithful subject.
    const dir = mkdtempSync(join(tmpdir(), 'mds scan space-'));
    try {
      assert.ok(dir.includes(' '), 'this test is meaningless unless the path has a space');
      execFileSync('git', ['init'], { cwd: dir, stdio: 'pipe' });
      execFileSync('git', ['config', 'user.email', 'test@test.test'], { cwd: dir, stdio: 'pipe' });
      execFileSync('git', ['config', 'user.name', 'Test'], { cwd: dir, stdio: 'pipe' });
      const target = join(dir, 'scan.mjs');
      writeFileSync(target, readFileSync(SCANNER));
      writeFileSync(join(dir, 'evil.md'), Buffer.concat([Buffer.from('x '), Buffer.from([0x1b])]));
      execFileSync('git', ['add', 'evil.md', 'scan.mjs'], { cwd: dir, stdio: 'pipe' });

      const r = spawnSync(process.execPath, [target], { cwd: dir, encoding: 'utf8', timeout: 30000 });
      assert.equal(r.status, 1,
        `scanner must run (and fail) from a spaced path; got status ${r.status}, stdout: ${r.stdout}`);
      assert.ok(r.stderr.includes('U+001B'), 'must report the planted byte');
    } finally { cleanup(dir); }
  });

});

// ---------------------------------------------------------------------------
// AC-20: non-regular git entries are skipped, not read
// ---------------------------------------------------------------------------
describe('AC-20: symlinks are skipped and counted, not read', () => {

  test('a tracked symlink (mode 120000) is skipped and reported', () => {
    const { dir, git } = mkTempGitRepo();
    try {
      writeFileSync(join(dir, 'real.md'), 'clean content\n');
      symlinkSync('real.md', join(dir, 'link.md'));
      git('add', 'real.md', 'link.md');
      const modes = execFileSync('git', ['ls-files', '-s'], { cwd: dir, encoding: 'utf8' });
      assert.ok(modes.includes('120000'), 'the fixture must actually stage a symlink');

      const r = runScanner([], { cwd: dir });
      assert.equal(r.status, 0, `symlink must not produce a read error; stderr: ${r.stderr}`);
      assert.ok(/1 symlink\/gitlink skipped/.test(r.stdout),
        `skipped entries must be counted separately; got: ${r.stdout}`);
      assert.ok(/Scanned 1 file\(s\)/.test(r.stdout), 'only the regular file is scanned');
    } finally { cleanup(dir); }
  });

});

// ---------------------------------------------------------------------------
// AC-16: git missing from PATH fails closed with exit 1
// ---------------------------------------------------------------------------
describe('AC-16: git not on PATH', () => {

  test('scanner exits 1 when git cannot be found (fail-closed)', () => {
    // AC-16 mandates exit 1 for all four named failure conditions. "Git not on
    // PATH" is a known, named failure — it is fail-closed (exit 1), not
    // indeterminate (exit 2).
    // Give the child a PATH containing node but not git, so the failure is
    // specifically "git is missing" and not "node is missing".
    const binDir = mkdtempSync(join(tmpdir(), 'mds-nopath-'));
    try {
      symlinkSync(process.execPath, join(binDir, 'node'));
      const r = spawnSync(process.execPath, [SCANNER], {
        cwd: ROOT,
        encoding: 'utf8',
        env: { PATH: binDir },
        timeout: 30000,
      });
      assert.equal(r.status, 1, `missing git must exit 1 (fail-closed); stdout: ${r.stdout}, stderr: ${r.stderr}`);
      assert.ok(/git is not on PATH/.test(r.stderr), `must name the condition; got: ${r.stderr}`);
    } finally { cleanup(binDir); }
  });

});
