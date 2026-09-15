/**
 * Tests for scripts/verify-ledger-citations.mjs
 *
 * D1.3 (comment-only sweep + ledger-citation gate): every planted denylist or
 * ceiling token in this file is built AT RUNTIME via string concatenation
 * (e.g. `['ADR', '099'].join('-')`) — no bare ADR-NNN or PF-NNN literal
 * appears anywhere in this file. T-D1-16 verifies this mechanically, against
 * both the guard script's own source and this spec's own source.
 *
 * Helpers below (runScanner, mkTempGitRepo, cleanup) are re-implemented from
 * scratch, matching the shape of scripts/__test__/verify-no-control-bytes.mjs
 * — not imported from that sibling, so this spec has no runtime dependency on
 * it.
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, mkdirSync, rmSync, readFileSync } from 'node:fs';
import { join, resolve, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync, execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

import {
  CEILING,
  DENYLIST,
  classify,
  scanText,
} from '../verify-ledger-citations.mjs';

const ROOT = resolve(fileURLToPath(import.meta.url), '../../..');
const SCANNER = join(ROOT, 'scripts/verify-ledger-citations.mjs');
const SPEC_SELF = fileURLToPath(import.meta.url);

// ---------------------------------------------------------------------------
// Helper: run the guard script as a subprocess
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
  const dir = mkdtempSync(join(tmpdir(), 'mds-ledger-'));
  const git = (...args) => execFileSync('git', args, { cwd: dir, encoding: 'utf8', stdio: 'pipe' });
  git('init');
  git('config', 'user.email', 'test@test.test');
  git('config', 'user.name', 'Test');
  return { dir, git };
}

function cleanup(dir) {
  try { rmSync(dir, { recursive: true, force: true }); } catch { /* ignore */ }
}

function writeAndAdd(dir, git, relPath, content) {
  const abs = join(dir, relPath);
  mkdirSync(dirname(abs), { recursive: true });
  writeFileSync(abs, content);
  git('add', relPath);
}

// A file with no citation tokens — keeps the scanned set non-empty when the
// file under test lives entirely under an excluded path.
const PLAIN_CONTENT = 'nothing to see here\n';

// A citation-token regex built the same piece-wise way the guard script
// builds its own — kept local so this spec never depends on an internal,
// unexported helper of the script under test. Self-clean because "ADR"/"PF"
// and the trailing "-\d{3}" never sit adjacent in this file's own source.
function countCitationTokens(text) {
  const re = new RegExp('\\b(' + 'ADR' + '|' + 'PF' + ')' + '-' + '(\\d{3})\\b', 'g');
  const matches = text.match(re);
  return matches === null ? 0 : matches.length;
}

// ---------------------------------------------------------------------------
// T-D1-10: real-tree run — stays RED until the sweep commits land, because
// the tree does not yet clear the non-vacuity floor without them.
// ---------------------------------------------------------------------------
describe('T-D1-10: real tree', () => {
  test('real repo tree (cwd: ROOT) exits 0 with files >= 500 and tokens >= 1000', () => {
    const r = runScanner([], { cwd: ROOT });
    assert.equal(r.status, 0, `expected exit 0; stderr: ${r.stderr}\nstdout: ${r.stdout}`);
    const m = r.stdout.match(/scanned (\d+) file\(s\), (\d+) citation token\(s\)/);
    assert.ok(m, `success output must include "scanned N file(s), M citation token(s)"; got: ${r.stdout}`);
    const files = parseInt(m[1], 10);
    const tokens = parseInt(m[2], 10);
    assert.ok(files >= 500, `expected >= 500 files scanned; got ${files}`);
    assert.ok(tokens >= 1000, `expected >= 1000 citation tokens; got ${tokens}`);
  });
});

// ---------------------------------------------------------------------------
// T-D1-11: ceiling control — a number the ledger has not minted
// ---------------------------------------------------------------------------
describe('T-D1-11: ceiling control', () => {
  test('an unminted ADR number exits 1 and is named as not minted', () => {
    const { dir, git } = mkTempGitRepo();
    try {
      const token = ['ADR', '099'].join('-');
      writeAndAdd(dir, git, 'src/note.rs', `// ${token}: plan-local, not yet minted\n`);
      const r = runScanner([], { cwd: dir });
      assert.equal(r.status, 1, `expected exit 1; stdout: ${r.stdout}\nstderr: ${r.stderr}`);
      assert.ok(r.stderr.includes('not minted'), `expected "not minted" in output; got: ${r.stderr}`);
    } finally { cleanup(dir); }
  });
});

// ---------------------------------------------------------------------------
// T-D1-12: denylist control — a number that carried a retired, source-local
// meaning until the 2026-09 sweep
// ---------------------------------------------------------------------------
describe('T-D1-12: denylist control', () => {
  test('a retired-meaning ADR number exits 1 and names the source-local meaning', () => {
    const { dir, git } = mkTempGitRepo();
    try {
      const token = ['ADR', '021'].join('-');
      writeAndAdd(dir, git, 'src/note.rs', `// applies ${token}\n`);
      const r = runScanner([], { cwd: dir });
      assert.equal(r.status, 1, `expected exit 1; stdout: ${r.stdout}\nstderr: ${r.stderr}`);
      assert.ok(r.stderr.includes('source-local meaning'), `expected "source-local meaning" in output; got: ${r.stderr}`);
    } finally { cleanup(dir); }
  });
});

// ---------------------------------------------------------------------------
// T-D1-13: legitimate id — minted, under the ceiling, not denylisted
// ---------------------------------------------------------------------------
describe('T-D1-13: legitimate id', () => {
  test('a minted, non-denylisted PF number exits 0 and counts one citation token', () => {
    const { dir, git } = mkTempGitRepo();
    try {
      const token = ['PF', '004'].join('-');
      writeAndAdd(dir, git, 'src/note.rs', `// all .mds reads funnel through one path (${token}).\n`);
      const r = runScanner([], { cwd: dir });
      assert.equal(r.status, 0, `expected exit 0; stdout: ${r.stdout}\nstderr: ${r.stderr}`);
      assert.ok(r.stdout.includes('1 citation token'), `expected "1 citation token" in output; got: ${r.stdout}`);
    } finally { cleanup(dir); }
  });
});

// ---------------------------------------------------------------------------
// T-D1-14: scope exclusions — a denylisted token that lives ONLY under an
// excluded path must not be scanned at all
// ---------------------------------------------------------------------------
describe('T-D1-14: scope exclusions', () => {
  test('a denylisted token present only under .devflow/features and CHANGELOG.md exits 0', () => {
    const { dir, git } = mkTempGitRepo();
    try {
      const token = ['ADR', '021'].join('-');
      writeAndAdd(dir, git, 'src/plain.rs', PLAIN_CONTENT);
      writeAndAdd(dir, git, '.devflow/features/x/KNOWLEDGE.md', `mentions ${token} for history\n`);
      writeAndAdd(dir, git, 'CHANGELOG.md', `mentions ${token} for history\n`);
      const r = runScanner([], { cwd: dir });
      assert.equal(r.status, 0, `expected exit 0 (excluded paths must not be scanned); stdout: ${r.stdout}\nstderr: ${r.stderr}`);
    } finally { cleanup(dir); }
  });
});

// ---------------------------------------------------------------------------
// T-D1-15: empty repo — non-vacuity guard
// ---------------------------------------------------------------------------
describe('T-D1-15: empty repo', () => {
  test('a git repo with zero tracked files exits 1 and names zero files', () => {
    const { dir } = mkTempGitRepo();
    try {
      const r = runScanner([], { cwd: dir });
      assert.equal(r.status, 1, `expected exit 1; stdout: ${r.stdout}\nstderr: ${r.stderr}`);
      const combined = r.stdout + r.stderr;
      assert.ok(combined.includes('zero files'), `expected "zero files" in output; got: ${combined}`);
    } finally { cleanup(dir); }
  });
});

// ---------------------------------------------------------------------------
// T-D1-16: self-clean — the guard cannot flag its own source or this spec
// ---------------------------------------------------------------------------
describe('T-D1-16: self-clean', () => {
  test('the guard script and this spec have zero citation-token regex matches against themselves', () => {
    const scriptText = readFileSync(SCANNER, 'utf8');
    assert.equal(countCitationTokens(scriptText), 0, 'script must be self-clean (no literal citation tokens)');
    assert.equal(scanText(SCANNER, scriptText).length, 0, 'script must produce zero findings against itself');

    const specText = readFileSync(SPEC_SELF, 'utf8');
    assert.equal(countCitationTokens(specText), 0, 'spec must be self-clean (no literal citation tokens)');
    assert.equal(scanText(SPEC_SELF, specText).length, 0, 'spec must produce zero findings against itself');
  });
});

// ---------------------------------------------------------------------------
// T-D1-17: golden — the frozen ceiling and denylist cannot silently drift
// ---------------------------------------------------------------------------
describe('T-D1-17: golden', () => {
  test('CEILING and DENYLIST match the frozen snapshot, and classify()/scanText() agree with it', () => {
    assert.deepEqual(CEILING, { ADR: 17, PF: 53 });

    assert.deepEqual(DENYLIST.map(d => d.num), [14, 16, 19, 21, 22, 23]);
    for (const entry of DENYLIST) {
      assert.equal(entry.prefix, 'ADR', `entry for ${entry.num} must have prefix "ADR"`);
      assert.ok(entry.reason.length > 0, `entry for ${entry.num} must have a non-empty reason`);
    }

    const minted = classify('PF', 4);
    assert.equal(minted.ok, true, 'this PF id is minted, under ceiling, and not denylisted');

    const retired = classify('ADR', 21);
    assert.equal(retired.ok, false);
    assert.ok(retired.reason.includes('source-local meaning'));

    const unminted = classify('ADR', 99);
    assert.equal(unminted.ok, false);
    assert.ok(unminted.reason.includes('not minted'));

    assert.deepEqual(scanText('x.rs', 'no citation tokens in this text at all'), []);
  });
});
