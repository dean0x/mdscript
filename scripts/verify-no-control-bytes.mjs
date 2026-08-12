#!/usr/bin/env node
/**
 * D-CB1: Source-hygiene gate — scans tracked git source for hazardous codepoints.
 *
 * The hazard class is derived from crates/mds-cli/tests/common/mod.rs:38-62
 * (`assert_no_control_chars`), with ONE documented divergence:
 *
 * D-CB3 DIVERGENCE: CR (U+000D) is permitted when immediately followed by LF
 * (i.e., CRLF line endings are allowed). The Rust helper flags all CR
 * unconditionally. This carve-out preserves the 17 CRLF pairs in the two
 * `mds fmt` fixture files. A future `.gitattributes text=auto` would normalize
 * those fixtures and may break the fmt tests — documented here so that change
 * is deliberate, not accidental.
 *
 * D-CB2: No hazard codepoint appears as a literal or backslash-u escape in
 * this file. All are written as numeric values. The edit tooling decodes
 * \uXXXX (4-hex) patterns into live bytes — this file's self-scan guards
 * against that vector (avoids PF-018).
 *
 * D-CB7: Pure Node codepoint iteration — no grep. BSD grep (macOS default)
 * lacks -P and exits 2 with empty output, making the absence of hazard bytes
 * indistinguishable from a grep invocation that cannot run (avoids PF-013).
 *
 * D-CB5: Fails closed. Zero-files-scanned is exit 1, not exit 0 (avoids
 * PF-016 — an empty scan masquerades as clean).
 *
 * D-CB8: --staged mode reads file content from the git index (git cat-file
 * blob :<path>), never from the working tree. Staging a clean file then
 * modifying the working copy does not bypass the hook.
 *
 * Usage:
 *   node scripts/verify-no-control-bytes.mjs              # full tree scan
 *   node scripts/verify-no-control-bytes.mjs --staged      # pre-commit (index)
 *   node scripts/verify-no-control-bytes.mjs <path> ...   # explicit paths
 *
 * Exit codes:
 *   0 — no hazards found (prints file count and byte count for non-vacuity)
 *   1 — hazard found, zero files scanned, invalid UTF-8, un-allowlisted NUL,
 *       unreadable tracked path, stale/unmatched allowlist entry, git missing
 *       from PATH, or not inside a git work tree (all fail-closed)
 *   2 — indeterminate: a git subcommand (ls-files / diff --cached / cat-file)
 *       failed unexpectedly
 */
'use strict';

import { spawnSync } from 'node:child_process';
import { readFileSync, realpathSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

/**
 * True when this module is the process entry point.
 *
 * Two traps make the obvious `import.meta.url === 'file://' + process.argv[1]`
 * wrong, and both fail SILENTLY: main() never runs, the process exits 0, and a
 * gate that scanned nothing is indistinguishable from a clean tree.
 *   1. Percent-encoding — any path containing a space never matches.
 *   2. Symlinks — Node resolves import.meta.url through realpath, while
 *      process.argv[1] keeps the path as typed (on macOS /tmp and
 *      /var/folders are symlinks, so this is the common case, not a corner).
 * Comparing realpaths handles both (applies ADR-009, avoids PF-013).
 *
 * @param {string} metaUrl — the caller's import.meta.url
 * @returns {boolean}
 */
export function isMainModule(metaUrl) {
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
// D-CB1: Hazard class definition.
//
// Exported so tests can import and assert completeness (D-CB1a: golden-set
// test prevents silent narrowing).
//
// Entry forms:
//   { from, to }                   — inclusive codepoint range
//   { cp, crlfException: true }    — single codepoint with CRLF exception
//   number                         — single codepoint
// ---------------------------------------------------------------------------
export const HAZARD_RANGES = [
  // C0 control characters (0x00-0x1F), excluding TAB (0x09) and LF (0x0A).
  // D-CB3: CR (0x0D) has a CRLF exception — see entry below.
  { from: 0x00, to: 0x08 },         //  1. C0: NUL..BS (below TAB)
  { from: 0x0b, to: 0x0c },         //  2. C0: VT, FF (between LF and CR)
  { cp: 0x0d, crlfException: true }, //  3. CR — lone CR fails; CRLF passes (D-CB3)
  { from: 0x0e, to: 0x1f },         //  4. C0: SO..US (above CR)
  { from: 0x7f, to: 0x7f },         //  5. DEL
  { from: 0x80, to: 0x9f },         //  6. C1 (at codepoint level — catches 0xC2 0x80-0x9F
                                    //        in UTF-8; continuation bytes are NOT matched
                                    //        because a 0x80-0x9F byte following a start byte
                                    //        is decoded to a codepoint >= 0x100 that falls
                                    //        outside this range)
  // Twelve Unicode Bidi_Control=Yes codepoints (Trojan Source, CVE-2021-42574).
  0x061c,   //  7. U+061C  Arabic Letter Mark
  0x200e,   //  8. U+200E  Left-to-Right Mark
  0x200f,   //  9. U+200F  Right-to-Left Mark
  0x202a,   // 10. U+202A  Left-to-Right Embedding
  0x202b,   // 11. U+202B  Right-to-Left Embedding
  0x202c,   // 12. U+202C  Pop Directional Formatting
  0x202d,   // 13. U+202D  Left-to-Right Override
  0x202e,   // 14. U+202E  Right-to-Left Override
  0x2066,   // 15. U+2066  Left-to-Right Isolate
  0x2067,   // 16. U+2067  Right-to-Left Isolate
  0x2068,   // 17. U+2068  First Strong Isolate
  0x2069,   // 18. U+2069  Pop Directional Isolate
  // JavaScript line/paragraph terminators (outside Bidi_Control, still hazardous
  // because JS parsers treat them as line endings inside string literals).
  0x2028,   // 19. U+2028  Line Separator
  0x2029,   // 20. U+2029  Paragraph Separator
  // Byte-Order Mark / Zero-Width No-Break Space.
  0xfeff,   // 21. U+FEFF  BOM / ZWNBSP
];
// HAZARD_RANGES has 21 entries. AC-12 lists the same class; the test asserts
// this exact count and composition (D-CB1a: golden-set completeness guard).

// ---------------------------------------------------------------------------
// D-CB6: Two enumerated allowlists, both empty. Every entry carries a written
// `reason`. A stale entry (path absent or declared codepoints no longer
// present) is itself an exit 1.
//
// BINARY_ALLOWLIST: files containing NUL bytes (intentional binary content).
// HAZARD_ALLOWLIST: files with specific hazardous codepoints for test fixtures.
// ---------------------------------------------------------------------------
export const BINARY_ALLOWLIST = [
  // { path: 'relative/from/root', reason: 'explanation' }
];

export const HAZARD_ALLOWLIST = [
  // { path: 'relative/from/root', codepoints: [0x...], reason: 'explanation' }
];

// ---------------------------------------------------------------------------
// Hazard predicate
// ---------------------------------------------------------------------------

/**
 * @param {number} cp    — Unicode codepoint being tested
 * @param {number|null} nextCp — codepoint immediately following cp (for CRLF)
 * @returns {boolean}
 */
export function isHazardous(cp, nextCp) {
  for (const entry of HAZARD_RANGES) {
    if (typeof entry === 'number') {
      if (cp === entry) return true;
    } else if (entry.crlfException) {
      // D-CB3: CR (0x0D) is only hazardous when the NEXT char is NOT LF.
      if (cp === entry.cp && nextCp !== 0x0a) return true;
    } else {
      if (cp >= entry.from && cp <= entry.to) return true;
    }
  }
  return false;
}

// ---------------------------------------------------------------------------
// Hexdump context helper (D-CB7: readable failure output)
// ---------------------------------------------------------------------------

/**
 * Return ±8-byte hex context around `offset` in `buf`.
 * @param {Buffer} buf
 * @param {number} offset
 * @returns {string}
 */
function hexContext(buf, offset) {
  const start = Math.max(0, offset - 8);
  const end = Math.min(buf.length, offset + 10);
  const hex = [];
  for (let i = start; i < end; i++) {
    const byte = buf[i].toString(16).padStart(2, '0');
    hex.push(i === offset ? `[${byte}]` : byte);
  }
  return hex.join(' ');
}

// ---------------------------------------------------------------------------
// UTF-8 decoder (returns array of {cp, byteOffset} objects)
// ---------------------------------------------------------------------------

/**
 * Decode a UTF-8 buffer into an array of {cp, byteOffset}.
 * Returns null if the buffer is not valid UTF-8 (after NUL check).
 * @param {Buffer} buf
 * @returns {{ cp: number, byteOffset: number }[] | null}
 */
function decodeUtf8(buf) {
  const codepoints = [];
  let i = 0;
  while (i < buf.length) {
    const b0 = buf[i];
    let cp, len;
    if (b0 <= 0x7f) {
      cp = b0;
      len = 1;
    } else if ((b0 & 0xe0) === 0xc0) {
      if (i + 1 >= buf.length) return null;
      const b1 = buf[i + 1];
      if ((b1 & 0xc0) !== 0x80) return null;
      cp = ((b0 & 0x1f) << 6) | (b1 & 0x3f);
      len = 2;
    } else if ((b0 & 0xf0) === 0xe0) {
      if (i + 2 >= buf.length) return null;
      const b1 = buf[i + 1];
      const b2 = buf[i + 2];
      if ((b1 & 0xc0) !== 0x80 || (b2 & 0xc0) !== 0x80) return null;
      cp = ((b0 & 0x0f) << 12) | ((b1 & 0x3f) << 6) | (b2 & 0x3f);
      len = 3;
    } else if ((b0 & 0xf8) === 0xf0) {
      if (i + 3 >= buf.length) return null;
      const b1 = buf[i + 1];
      const b2 = buf[i + 2];
      const b3 = buf[i + 3];
      if ((b1 & 0xc0) !== 0x80 || (b2 & 0xc0) !== 0x80 || (b3 & 0xc0) !== 0x80) return null;
      cp = ((b0 & 0x07) << 18) | ((b1 & 0x3f) << 12) | ((b2 & 0x3f) << 6) | (b3 & 0x3f);
      len = 4;
    } else {
      return null; // Invalid lead byte
    }
    codepoints.push({ cp, byteOffset: i });
    i += len;
  }
  return codepoints;
}

// ---------------------------------------------------------------------------
// git helpers
// ---------------------------------------------------------------------------

function gitExec(args, cwd = process.cwd()) {
  const result = spawnSync('git', args, { cwd, encoding: 'buffer', maxBuffer: 64 * 1024 * 1024 });
  if (result.error) {
    if (result.error.code === 'ENOENT') {
      // AC-16: fail-closed (exit 1) — "git missing" is a known, named failure,
      // not an indeterminate tool error.
      console.error('✖ verify-no-control-bytes: git is not on PATH');
      process.exit(1);
    }
    console.error(`✖ verify-no-control-bytes: git error: ${result.error.message}`);
    process.exit(2);
  }
  return result;
}

/** Verify we are inside a git work tree (exit 1 if not — AC-16: fail-closed). */
function assertGitRepo(cwd) {
  const r = gitExec(['rev-parse', '--is-inside-work-tree'], cwd);
  if (r.status !== 0) {
    // AC-16: fail-closed (exit 1) — "not a git repo" is a known, named failure,
    // not an indeterminate tool error.
    console.error('✖ verify-no-control-bytes: not inside a git work tree');
    process.exit(1);
  }
}

/**
 * Get file list in default mode via `git ls-files -sz`.
 * Returns array of { path, mode } objects.
 * D-CB5a: skips git modes 120000 (symlink) and 160000 (gitlink).
 *
 * `git ls-files -sz` output format (each entry NUL-terminated):
 *   <mode> <sha> <stage>\t<path>\0...
 * The TAB separates the staging info from the file path within ONE NUL record.
 */
function getTrackedFiles(cwd) {
  const r = gitExec(['ls-files', '-sz'], cwd);
  if (r.status !== 0) {
    console.error('✖ verify-no-control-bytes: git ls-files failed');
    process.exit(2);
  }
  // Each NUL-terminated entry is "<mode> <sha> <stage>\t<path>"
  const entries = r.stdout.toString('utf8').split('\0').filter(s => s.length > 0);
  const files = [];
  for (const entry of entries) {
    const tabIdx = entry.indexOf('\t');
    if (tabIdx === -1) continue; // Malformed entry — skip
    const meta = entry.slice(0, tabIdx);
    const path = entry.slice(tabIdx + 1);
    // meta format: "<mode> <sha> <stage>"
    const mode = parseInt(meta.split(' ')[0], 8);
    if (mode === 0o120000 || mode === 0o160000) {
      files.push({ path, mode, skip: true });
    } else {
      files.push({ path, mode, skip: false });
    }
  }
  return files;
}

/**
 * Get staged file list for --staged mode.
 * Uses `git diff --cached --name-only -z --diff-filter=ACMR` for paths.
 * D-CB8: content read from git index via `git cat-file blob :<path>`.
 */
function getStagedFiles(cwd) {
  const r = gitExec(['diff', '--cached', '--name-only', '-z', '--diff-filter=ACMR'], cwd);
  if (r.status !== 0) {
    // D-CB5: fail closed. `git diff --cached` exits 0 even when nothing is
    // staged, so a non-zero status is a real tool failure (corrupt index,
    // unreadable object). Treating it as "no staged files" would let the
    // pre-commit hook report success on a scan that never happened.
    console.error(
      `✖ verify-no-control-bytes: git diff --cached failed (status ${r.status}): ` +
      `${r.stderr.toString('utf8').trim()}`,
    );
    process.exit(2);
  }
  const paths = r.stdout.toString('utf8').split('\0').filter(s => s.length > 0);
  return paths.map(p => ({ path: p, mode: 0o100644, skip: false, staged: true }));
}

/**
 * Read file content from the git index (staged blob) via `git cat-file blob :<path>`.
 * D-CB8: never reads from the working tree in --staged mode.
 */
function readIndexBlob(path, cwd) {
  const r = gitExec(['cat-file', 'blob', `:${path}`], cwd);
  if (r.status !== 0) {
    console.error(`✖ verify-no-control-bytes: cannot read staged blob for ${path}`);
    process.exit(2);
  }
  return r.stdout; // Buffer
}

// ---------------------------------------------------------------------------
// Scanner core
// ---------------------------------------------------------------------------

/**
 * Scan a single file buffer for hazardous codepoints.
 * @param {Buffer} buf    — raw file bytes
 * @param {string} relPath — repo-relative path (for error messages)
 * @param {Set<number>} allowedCps — codepoints explicitly allowlisted for this file
 * @returns {{ codepoint: number, byteOffset: number, allowed?: boolean }[]}
 *   Every hazard occurrence, including allowlisted ones. Allowlisted hits carry
 *   `allowed: true` so the caller can record the entry as exercised — dropping
 *   them here would make every HAZARD_ALLOWLIST entry look stale (D-CB6).
 */
export function scanBuffer(buf, relPath, allowedCps) {
  // Check for NUL (binary file indicator)
  if (buf.includes(0x00)) {
    const inBinaryAllowlist = BINARY_ALLOWLIST.some(e => e.path === relPath);
    if (!inBinaryAllowlist) {
      return [{ codepoint: 0x00, byteOffset: buf.indexOf(0x00), binaryError: true }];
    }
    return []; // Allowed binary file
  }

  const codepoints = decodeUtf8(buf);
  if (codepoints === null) {
    return [{ codepoint: -1, byteOffset: 0, invalidUtf8: true }];
  }

  const hits = [];
  for (let i = 0; i < codepoints.length; i++) {
    const { cp, byteOffset } = codepoints[i];
    const nextCp = i + 1 < codepoints.length ? codepoints[i + 1].cp : null;
    if (isHazardous(cp, nextCp)) {
      hits.push({ codepoint: cp, byteOffset, allowed: allowedCps.has(cp) });
    }
  }
  return hits;
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

function main() {
  const args = process.argv.slice(2);
  const isStaged = args.includes('--staged');
  const explicitPaths = args.filter(a => a !== '--staged');

  const cwd = process.cwd();

  // Verify git is accessible and we are in a repo (D-CB5)
  assertGitRepo(cwd);

  // ---- Build file list ----
  let fileEntries;
  let skippedCount = 0;

  if (explicitPaths.length > 0) {
    // Explicit path mode (used by tests with temp repos)
    fileEntries = explicitPaths.map(p => ({
      path: p,
      mode: 0o100644,
      skip: false,
      staged: false,
      absolutePath: resolve(cwd, p),
    }));
  } else if (isStaged) {
    // D-CB8: staged mode — read from git index
    fileEntries = getStagedFiles(cwd).map(e => ({ ...e, absolutePath: null }));
  } else {
    // Default: full tracked tree
    const all = getTrackedFiles(cwd);
    skippedCount = all.filter(e => e.skip).length;
    fileEntries = all
      .filter(e => !e.skip)
      .map(e => ({ ...e, absolutePath: resolve(cwd, e.path) }));
  }

  // ---- D-CB5: Non-vacuity guard (AC-6) ----
  // Zero files scanned — including in --staged mode — is exit 1, never 0.
  // "Scanned 0 file(s), 0 byte(s): clean" is indistinguishable from a broken
  // path-discovery that never found anything (the exact shape D-CB5 forbids).
  if (fileEntries.length === 0) {
    console.error('✖ verify-no-control-bytes: zero files scanned (D-CB5: empty scan is not a pass)');
    console.error(isStaged
      ? '  No staged files found — stage at least one file before running the pre-commit hook.'
      : '  If this is a new repo with no commits, run `git add` first.');
    process.exit(1);
  }

  // ---- Validate allowlists upfront (D-CB6) ----
  const errors = [];

  // Build allowlist lookup: path -> Set<codepoint>
  const hazardAllowMap = new Map(); // relPath -> Set<cp>
  for (const entry of HAZARD_ALLOWLIST) {
    if (!hazardAllowMap.has(entry.path)) hazardAllowMap.set(entry.path, new Set());
    for (const cp of entry.codepoints) {
      hazardAllowMap.get(entry.path).add(cp);
    }
  }

  // ---- Scan each file ----
  let totalBytes = 0;
  let scannedFiles = 0;
  const exercisedAllowlist = new Set(); // tracks which allowlist entries are hit
  // AC-30: hexCtx is pre-computed so the file buffer is not retained beyond the
  // scan of a single file. { path, codepoint, byteOffset, hexCtx }
  const hazardHits = [];

  for (const entry of fileEntries) {
    let buf;
    try {
      if (isStaged) {
        buf = readIndexBlob(entry.path, cwd);
      } else {
        buf = readFileSync(entry.absolutePath || resolve(cwd, entry.path));
      }
    } catch (err) {
      errors.push(`Cannot read ${entry.path}: ${err.message}`);
      continue;
    }

    totalBytes += buf.length;
    scannedFiles++;

    const allowedCps = hazardAllowMap.get(entry.path) ?? new Set();
    const hits = scanBuffer(buf, entry.path, allowedCps);

    for (const hit of hits) {
      if (hit.invalidUtf8) {
        errors.push(`${entry.path}: invalid UTF-8 content (not a text file?)`);
      } else if (hit.binaryError) {
        errors.push(`${entry.path}: contains NUL bytes — add to BINARY_ALLOWLIST with a reason`);
      } else if (hit.allowed) {
        // D-CB6: the allowlist entry is genuinely exercised — record it so the
        // stale-entry check below does not flag it.
        exercisedAllowlist.add(`${entry.path}:${hit.codepoint}`);
      } else {
        // AC-30: compute hex context now so buf is not retained after this iteration.
        hazardHits.push({ path: entry.path, codepoint: hit.codepoint, byteOffset: hit.byteOffset, hexCtx: hexContext(buf, hit.byteOffset) });
      }
    }
  }

  // ---- Stale allowlist check (D-CB6) ----
  for (const entry of BINARY_ALLOWLIST) {
    const exists = fileEntries.some(e => e.path === entry.path);
    if (!exists) {
      errors.push(`BINARY_ALLOWLIST: stale entry "${entry.path}" — file is no longer tracked`);
    }
  }
  for (const entry of HAZARD_ALLOWLIST) {
    const exists = fileEntries.some(e => e.path === entry.path);
    if (!exists) {
      errors.push(`HAZARD_ALLOWLIST: stale entry "${entry.path}" — file is no longer tracked`);
    } else {
      // Verify the declared codepoints actually occur in the file
      for (const cp of entry.codepoints) {
        const key = `${entry.path}:${cp}`;
        if (!exercisedAllowlist.has(key)) {
          errors.push(`HAZARD_ALLOWLIST: stale entry "${entry.path}" cp U+${cp.toString(16).toUpperCase().padStart(4, '0')} — codepoint not found in file`);
        }
      }
    }
  }

  // ---- Report ----
  const passStats = `Scanned ${scannedFiles} file(s), ${totalBytes} byte(s)` +
    (skippedCount > 0 ? `, ${skippedCount} symlink/gitlink skipped` : '');

  // AC-17: name every allowlist entry that was actually exercised by this run.
  for (const entry of HAZARD_ALLOWLIST) {
    const exercisedCps = entry.codepoints.filter(cp => exercisedAllowlist.has(`${entry.path}:${cp}`));
    if (exercisedCps.length > 0) {
      const cpList = exercisedCps
        .map(cp => `U+${cp.toString(16).toUpperCase().padStart(4, '0')}`)
        .join(', ');
      console.log(`  allowlist exercised: ${entry.path} [${cpList}] (reason: ${entry.reason})`);
    }
  }

  if (hazardHits.length > 0 || errors.length > 0) {
    for (const e of errors) {
      console.error(`✖ ${e}`);
    }
    for (const hit of hazardHits) {
      const cpHex = `U+${hit.codepoint.toString(16).toUpperCase().padStart(4, '0')}`;
      console.error(`✖ ${hit.path}: hazardous codepoint ${cpHex} at byte offset ${hit.byteOffset}`);
      console.error(`  context: ${hit.hexCtx}`);
    }
    console.error(`✖ source-hygiene gate FAILED — ${passStats}`);
    process.exit(1);
  }

  console.log(`✓ source-hygiene gate: ${passStats}`);
  process.exit(0);
}

// Run only when executed directly (not imported by tests). See isMainModule.
if (isMainModule(import.meta.url)) {
  main();
}
