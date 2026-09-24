#!/usr/bin/env node
// Pack-contents gate — asserts no publishable npm package ships a dangling
// source map.
//
// All six TS packages inherit sourceMap/declarationMap from tsconfig.base.json.
// Every emitted map's "sources" entry points at ../src/*.ts, but no package's
// "files" allowlist ships src/, and no map embeds sourcesContent. The maps are
// unusable by any consumer and were roughly 27% of unpacked tarball size before
// this gate existed. tsconfig.base.json now sets both flags to false; this gate
// makes the absence durable so a future tsconfig edit (or a package-specific
// override) cannot silently reintroduce them.
//
// Checks, per publishable workspace:
//   1. No packed file entry ends in .map.
//   2. No packed .js/.cjs/.mjs/.d.ts/.d.cts/.d.mts file contains a
//      `sourceMappingURL=` trailer (a map file could be pruned from "files"
//      while the comment pointing at it ships regardless).
//
// Usage:
//   node scripts/verify-pack-contents.mjs
//
// Exit codes:
//   0 — no .map entries, no sourceMappingURL trailers, floors cleared
//   1 — a violation was found, or the non-vacuity floor was not cleared
//   2 — indeterminate: npm pack failed, or its JSON output could not be parsed
'use strict';

import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

// The 8 publishable workspaces (mirrors PKG_PATHS in scripts/verify-versions.mjs,
// expressed as npm workspace identifiers rather than package.json paths since
// each entry here is passed straight to `npm pack --dry-run -w <ws>`).
export const WORKSPACES = [
  '@mdscript/mds',
  '@mdscript/mds-wasm',
  '@mdscript/bundler-utils',
  '@mdscript/vite-plugin',
  '@mdscript/rollup-plugin',
  '@mdscript/webpack-loader',
  '@mdscript/rspack-loader',
  '@mdscript/mds-napi',
];

// File extensions worth scanning for a sourceMappingURL trailer. A .map file
// can be pruned from a "files" allowlist while the comment pointing at it
// still ships in the compiled output — the trailer check catches that case
// independently of the .map-entry check.
const TRAILER_EXTENSIONS = ['.js', '.cjs', '.mjs', '.d.ts', '.d.cts', '.d.mts'];

// Non-vacuity floors (well below the real measured counts, so routine drift in
// package contents never trips them — only a broken/empty scan does):
//   8 workspaces is the exact publishable set; a lower count means a workspace
//   was skipped (npm pack failed silently, or WORKSPACES drifted from reality).
//   40 files is comfortably below the 59 trailer-candidate files the 8 packages pack today.
const MIN_PACKAGES = 8;
const MIN_FILES_CHECKED = 40;

/**
 * True when this module is the process entry point (see verify-no-control-bytes.mjs
 * for why a plain import.meta.url === argv[1] comparison is insufficient).
 * @param {string} metaUrl
 * @returns {boolean}
 */
function isMainModule(metaUrl) {
  const entry = process.argv[1];
  if (!entry) return false;
  return pathToFileURL(resolve(entry)).href === metaUrl;
}

/**
 * Extract `.map` file entries from a single `npm pack --dry-run --json`
 * listing (the parsed object for one package: `{ files: [{ path, size }], ... }`).
 *
 * @param {{ files: { path: string }[] }} listing
 * @returns {string[]} packed paths ending in `.map`
 */
export function findMapEntries(listing) {
  return listing.files
    .map((f) => f.path)
    .filter((p) => p.endsWith('.map'));
}

/**
 * Resolve a packed file's on-disk path, guarding against traversal outside
 * the workspace directory. A packed path is always relative (npm pack never
 * emits absolute or `..`-prefixed entries), but this guard fails closed
 * rather than trusting that invariant silently.
 *
 * @param {string} workspaceDir — absolute workspace directory
 * @param {string} packedPath — path as reported by npm pack (relative)
 * @returns {string|null} absolute path inside workspaceDir, or null if it escapes
 */
export function resolveSafePath(workspaceDir, packedPath) {
  const abs = resolve(workspaceDir, packedPath);
  if (abs !== workspaceDir && !abs.startsWith(workspaceDir + sep)) return null;
  return abs;
}

/**
 * Find `sourceMappingURL=` trailers in packed files.
 *
 * @param {{ workspace: string, workspaceDir: string, path: string }[]} entries
 *   packed file entries worth scanning (already filtered by extension)
 * @param {(absPath: string) => string} readFn — injected file reader (testable
 *   without touching disk)
 * @returns {{ workspace: string, path: string, reason?: string }[]} hits
 */
export function findTrailerHits(entries, readFn) {
  const hits = [];
  for (const entry of entries) {
    const safePath = resolveSafePath(entry.workspaceDir, entry.path);
    if (safePath === null) {
      hits.push({ workspace: entry.workspace, path: entry.path, reason: 'path escapes workspace directory' });
      continue;
    }
    const content = readFn(safePath);
    if (content.includes('sourceMappingURL=')) {
      hits.push({ workspace: entry.workspace, path: entry.path });
    }
  }
  return hits;
}

/**
 * @param {string} path
 * @returns {boolean}
 */
export function isTrailerCandidate(path) {
  return TRAILER_EXTENSIONS.some((ext) => path.endsWith(ext));
}

/**
 * Build the final pass/fail summary from accumulated results.
 *
 * @param {{ packageCount: number, fileCount: number, mapHits: { workspace: string, path: string }[], trailerHits: { workspace: string, path: string }[] }} totals
 * @returns {{ ok: boolean, message: string }}
 */
export function summarize(totals) {
  const errors = [];
  if (totals.packageCount < MIN_PACKAGES) {
    errors.push(`non-vacuity floor: expected >= ${MIN_PACKAGES} packages, checked ${totals.packageCount}`);
  }
  if (totals.fileCount < MIN_FILES_CHECKED) {
    errors.push(`non-vacuity floor: expected >= ${MIN_FILES_CHECKED} files, checked ${totals.fileCount}`);
  }
  for (const hit of totals.mapHits) {
    errors.push(`${hit.workspace}: ships dangling source map "${hit.path}"`);
  }
  for (const hit of totals.trailerHits) {
    errors.push(`${hit.workspace}: "${hit.path}" contains a sourceMappingURL trailer${hit.reason ? ` (${hit.reason})` : ''}`);
  }
  if (errors.length > 0) {
    return { ok: false, message: errors.join('\n') };
  }
  return {
    ok: true,
    message: `pack-contents gate: ${totals.packageCount} packages, ${totals.fileCount} files, 0 maps`,
  };
}

/**
 * Run `npm pack --dry-run --json -w <workspace>` and parse its output.
 * spawnSync is called with an argv array — no shell, no string interpolation.
 *
 * @param {string} workspace
 * @param {string} cwd
 * @returns {{ ok: true, listing: object } | { ok: false, error: string }}
 */
function packDryRun(workspace, cwd) {
  const r = spawnSync(
    'npm',
    ['pack', '--dry-run', '--json', '-w', workspace],
    { cwd, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024, timeout: 120_000 },
  );
  if (r.error) {
    return { ok: false, error: `npm pack -w ${workspace}: ${r.error.message}` };
  }
  if (r.status !== 0) {
    return { ok: false, error: `npm pack -w ${workspace} exited ${r.status}: ${r.stderr.trim()}` };
  }
  let parsed;
  try {
    parsed = JSON.parse(r.stdout);
  } catch (err) {
    return { ok: false, error: `npm pack -w ${workspace}: could not parse JSON output: ${err.message}` };
  }
  if (!Array.isArray(parsed) || parsed.length !== 1) {
    return { ok: false, error: `npm pack -w ${workspace}: expected a single-entry JSON array, got ${JSON.stringify(parsed).slice(0, 200)}` };
  }
  return { ok: true, listing: parsed[0] };
}

/**
 * Resolve a workspace's directory on disk from package.json's workspaces glob
 * roots, by asking npm directly (avoids re-deriving the workspaces list here).
 *
 * @param {string} workspace — npm workspace identifier (package name)
 * @param {string} cwd
 * @returns {string|null}
 */
function resolveWorkspaceDir(workspace, cwd) {
  const r = spawnSync(
    'npm',
    ['exec', '-w', workspace, '--', 'node', '-e', 'process.stdout.write(process.cwd())'],
    { cwd, encoding: 'utf8', timeout: 30_000 },
  );
  if (r.error || r.status !== 0) return null;
  const dir = r.stdout.trim();
  return dir.length > 0 ? dir : null;
}

function main() {
  const cwd = ROOT;
  const mapHits = [];
  const trailerHits = [];
  let fileCount = 0;
  let packageCount = 0;
  const hardErrors = [];

  for (const workspace of WORKSPACES) {
    const packed = packDryRun(workspace, cwd);
    if (!packed.ok) {
      hardErrors.push(packed.error);
      continue;
    }
    packageCount++;
    const { listing } = packed;

    for (const path of findMapEntries(listing)) {
      mapHits.push({ workspace, path });
    }

    const workspaceDir = resolveWorkspaceDir(workspace, cwd);
    if (workspaceDir === null) {
      hardErrors.push(`${workspace}: could not resolve workspace directory`);
      continue;
    }

    const candidates = listing.files
      .map((f) => f.path)
      .filter(isTrailerCandidate)
      .map((path) => ({ workspace, workspaceDir, path }));
    fileCount += candidates.length;

    const hits = findTrailerHits(candidates, (absPath) => readFileSync(absPath, 'utf8'));
    trailerHits.push(...hits);
  }

  if (hardErrors.length > 0) {
    console.error('✖ pack-contents gate: could not complete the scan:');
    for (const e of hardErrors) console.error(`  - ${e}`);
    process.exit(2);
  }

  const result = summarize({ packageCount, fileCount, mapHits, trailerHits });
  if (!result.ok) {
    console.error(`✖ pack-contents gate FAILED:\n${result.message}`);
    process.exit(1);
  }
  console.log(`✓ ${result.message}`);
  process.exit(0);
}

if (isMainModule(import.meta.url)) {
  main();
}
