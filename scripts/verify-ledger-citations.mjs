#!/usr/bin/env node
/**
 * D1.3 (D-C4): Ledger-citation gate — fails on a decision-log id citation in
 * tracked source that is either not yet minted in the learning ledger, or
 * that carried a different, source-local meaning before the 2026-09 sweep
 * replaced every such citation with the decision spelled out inline.
 *
 * Two failure classes, checked in this order (denylist first — several
 * denylisted numbers sit ABOVE the ceiling too, and the denylist reason must
 * win so an operator is told "this number meant something else here", not
 * just "this number is not minted yet"):
 *
 *   R1 ceiling — CEILING is a frozen snapshot of the highest minted id per
 *   prefix at sweep time. The learning ledger (.devflow/learning/) is
 *   untracked and therefore absent in CI, so this gate cannot read it at
 *   run time to stay live — the snapshot is the only thing a CI checkout
 *   can see. A number above the ceiling reads as "the learning ledger has
 *   not minted this — a plan-local number", because that is the only
 *   pattern this sweep ever found above the snapshot line: draft plan
 *   prose that got merged as if the id already existed. Bumping the
 *   ceiling (when the ledger genuinely mints the next ADR or PF number) is
 *   the deliberate moment to grep tracked source for the newly-legal number —
 *   this file is not the place that grep runs.
 *
 *   R2 denylist — six ADR numbers (14, 16, 19, 21, 22, 23) carried a
 *   different, source-local meaning in comments and docs before the
 *   2026-09 sweep rewrote every citation to spell the decision out inline.
 *   The numbers are frozen here, not re-derived from anything — this gate
 *   never reads the learning ledger.
 *
 * Residual (not mechanically detectable): a semantic re-collision, where an
 * id is BOTH validly minted AND legitimately cited for its real meaning, but
 * a source comment elsewhere happens to reuse the same three digits for an
 * unrelated, informal sense. Today the one instance in this tree is the
 * pitfall id for a bare relative filename passed where an absolute path was
 * required (distinct from that same id's watcher self-trigger meaning in
 * this same tree). This gate cannot distinguish the two senses of one
 * number; the control is the added-lines id check every PR gate already
 * runs, plus human review.
 *
 * Scope: every tracked, non-symlink, non-gitlink file, EXCEPT anything under
 * `.devflow/` (the learning ledger and per-feature knowledge bases cite
 * these ids by design and are not sweep targets) and `CHANGELOG.md` (release
 * history is immutable once a section ships; only its `[Unreleased]` head
 * was in scope for the sweep itself, and this gate does not special-case
 * "which half of one file").
 *
 * The token regex is assembled from string pieces (`'ADR'`, `'PF'`, the
 * hyphen, `'\\d{3}'`) rather than written as one literal, so this file's own
 * source never contains an `ADR-NNN`/`PF-NNN`-shaped substring — see the
 * self-clean test in the paired spec.
 *
 * Usage:
 *   node scripts/verify-ledger-citations.mjs
 *
 * Exit codes:
 *   0 — no unminted or retired-meaning citation found (prints file count and
 *       citation-token count for non-vacuity)
 *   1 — a finding was reported, OR zero files were in scope, OR the current
 *       directory is not inside a git work tree (all fail-closed)
 *   2 — a git subcommand failed unexpectedly (indeterminate, not clean)
 */
'use strict';

import { spawnSync } from 'node:child_process';
import { readFileSync, realpathSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

/**
 * True when this module is the process entry point.
 *
 * Comparing realpaths (not raw argv[1] against import.meta.url) handles both
 * percent-encoded paths (a space in the path) and symlinked temp dirs
 * (macOS /tmp and /var/folders) — either trap would otherwise make main()
 * silently never run.
 *
 * @param {string} metaUrl — the caller's import.meta.url
 * @returns {boolean}
 */
function isMainModule(metaUrl) {
  const entry = process.argv[1];
  if (!entry) return false;
  const modulePath = fileURLToPath(metaUrl);
  try {
    return realpathSync(entry) === realpathSync(modulePath);
  } catch {
    return pathToFileURL(resolve(entry)).href === metaUrl;
  }
}

// ---------------------------------------------------------------------------
// R1: frozen ceiling snapshot. See the header comment above for why this is
// a snapshot rather than a live read of the (untracked, CI-absent) ledger.
// ---------------------------------------------------------------------------
export const CEILING = { ADR: 17, PF: 53 };

const CEILING_REASON =
  'cites an id the learning ledger has not minted — a plan-local number';

// ---------------------------------------------------------------------------
// R2: frozen denylist. See the header comment above for the 2026-09 sweep
// this reflects.
// ---------------------------------------------------------------------------
const DENYLIST_REASON =
  'this id carried a different, source-local meaning until the 2026-09 sweep; name the invariant instead';

export const DENYLIST = [14, 16, 19, 21, 22, 23].map(num => ({
  prefix: 'ADR',
  num,
  reason: DENYLIST_REASON,
}));

/**
 * Classify one (prefix, num) citation.
 *
 * Denylist is checked BEFORE the ceiling: several denylisted numbers sit
 * above CEILING.ADR, and a denylisted number must report as retired-meaning,
 * not as merely unminted.
 *
 * @param {string} prefix — 'ADR' or 'PF'
 * @param {number} num — the parsed 3-digit id
 * @returns {{ok: true} | {ok: false, reason: string}}
 */
export function classify(prefix, num) {
  const denied = DENYLIST.find(d => d.prefix === prefix && d.num === num);
  if (denied) return { ok: false, reason: denied.reason };
  const ceiling = CEILING[prefix];
  if (typeof ceiling !== 'number' || num > ceiling) {
    return { ok: false, reason: CEILING_REASON };
  }
  return { ok: true };
}

/** Assembled from pieces — see the header comment's self-clean note. */
function buildTokenRegex() {
  return new RegExp('\\b(' + 'ADR' + '|' + 'PF' + ')' + '-' + '(\\d{3})\\b', 'g');
}

/**
 * Scan one file's text for citation tokens that classify as a finding
 * (unminted or retired-meaning). Tokens that classify ok are not returned —
 * callers that also need the total token count re-run buildTokenRegex().
 *
 * @param {string} path — the file's path, used only to label findings
 * @param {string} text — the file's decoded UTF-8 content
 * @returns {Array<{path: string, line: number, token: string, reason: string}>}
 */
export function scanText(path, text) {
  const re = buildTokenRegex();
  const findings = [];
  let match;
  while ((match = re.exec(text)) !== null) {
    const prefix = match[1];
    const num = parseInt(match[2], 10);
    const result = classify(prefix, num);
    if (!result.ok) {
      const line = text.slice(0, match.index).split('\n').length;
      findings.push({ path, line, token: match[0], reason: result.reason });
    }
  }
  return findings;
}

/** Total citation-token matches in text, regardless of classify() result. */
function countTokens(text) {
  const re = buildTokenRegex();
  let n = 0;
  while (re.exec(text) !== null) n++;
  return n;
}

// ---------------------------------------------------------------------------
// git helpers — same shape as scripts/verify-no-control-bytes.mjs.
// ---------------------------------------------------------------------------

function gitExec(args, cwd = process.cwd()) {
  const result = spawnSync('git', args, {
    cwd,
    encoding: 'buffer',
    maxBuffer: 64 * 1024 * 1024,
    timeout: 30_000,
  });
  if (result.error) {
    console.error(`✖ ledger-citation gate: git error: ${result.error.message}`);
    process.exit(2);
  }
  return result;
}

/** Verify we are inside a git work tree (exit 1 if not — a known, named, fail-closed case). */
function assertGitRepo(cwd) {
  const r = gitExec(['rev-parse', '--is-inside-work-tree'], cwd);
  if (r.status !== 0) {
    console.error('✖ ledger-citation gate: not inside a git work tree');
    process.exit(1);
  }
}

/**
 * Tracked files via `git ls-files -sz`. Skips git modes 120000 (symlink) and
 * 160000 (gitlink) — same treatment as scripts/verify-no-control-bytes.mjs.
 *
 * `git ls-files -sz` output format (each entry NUL-terminated):
 *   <mode> <sha> <stage>\t<path>\0...
 */
function getTrackedFiles(cwd) {
  const r = gitExec(['ls-files', '-sz'], cwd);
  if (r.status !== 0) {
    console.error('✖ ledger-citation gate: git ls-files failed');
    process.exit(2);
  }
  const entries = r.stdout.toString('utf8').split('\0').filter(s => s.length > 0);
  const files = [];
  for (const entry of entries) {
    const tabIdx = entry.indexOf('\t');
    if (tabIdx === -1) continue; // Malformed entry — skip
    const meta = entry.slice(0, tabIdx);
    const path = entry.slice(tabIdx + 1);
    const mode = parseInt(meta.split(' ')[0], 8);
    const skip = mode === 0o120000 || mode === 0o160000;
    files.push({ path, mode, skip });
  }
  return files;
}

const EXCLUDED_FILE = 'CHANGELOG.md';
const EXCLUDED_PREFIX = '.devflow/';

function inScope(path) {
  if (path === EXCLUDED_FILE) return false;
  if (path.startsWith(EXCLUDED_PREFIX)) return false;
  return true;
}

const MAX_FINDINGS_PRINTED = 200;

function main() {
  const cwd = process.cwd();

  assertGitRepo(cwd);

  const all = getTrackedFiles(cwd);
  const scannable = all.filter(e => !e.skip && inScope(e.path));

  if (scannable.length === 0) {
    console.error('✖ ledger-citation gate: zero files scanned (non-vacuity: an empty scan is not a pass)');
    process.exit(1);
  }

  let scannedFiles = 0;
  let citationTokens = 0;
  const findings = [];

  for (const entry of scannable) {
    const absolutePath = resolve(cwd, entry.path);
    let buf;
    try {
      buf = readFileSync(absolutePath);
    } catch {
      continue; // Unreadable tracked path — nothing further this gate can do about it.
    }
    if (buf.includes(0x00)) continue; // Binary — not scanned for text citations.

    const text = buf.toString('utf8');
    scannedFiles += 1;
    citationTokens += countTokens(text);
    findings.push(...scanText(entry.path, text));
  }

  if (findings.length > 0) {
    const printed = findings.slice(0, MAX_FINDINGS_PRINTED);
    for (const f of printed) {
      console.error(`✖ ledger-citation gate: ${f.path}:${f.line}: ${f.token} — ${f.reason}`);
    }
    const remaining = findings.length - printed.length;
    if (remaining > 0) {
      console.error(`  … ${remaining} more`);
    }
    process.exit(1);
  }

  console.log(
    `✓ ledger-citation gate: scanned ${scannedFiles} file(s), ${citationTokens} citation token(s); ` +
    'none unminted or retired-meaning',
  );
  process.exit(0);
}

// Run only when executed directly (not imported by tests). See isMainModule.
if (isMainModule(import.meta.url)) {
  main();
}
