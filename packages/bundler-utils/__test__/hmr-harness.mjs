/**
 * Shared HMR test harness for bundler plugin specs.
 *
 * Pure ESM, node: builtins ONLY — imports NO bundler. Each spec passes its
 * own driver in (Vite, Rollup, Rspack etc.) so this file remains agnostic.
 *
 * This file is a helper, NOT a test suite. It MUST NOT match *.spec.mjs
 * so the test runner does not pick it up as a test file.
 *
 * ## Platform gating (decision D5)
 *
 * HMR filesystem-event tests are gated to Linux in CI because:
 *  - macOS FSEvents does not surface read-access events (PF-006) and has
 *    higher latency, making timing-sensitive HMR tests unreliable.
 *  - Windows uses a different notify backend.
 *  - Linux inotify (after the PF-006 fix in 6b7f2fe) is the reference platform.
 *
 * Set MDS_HMR=1 to force-enable on any platform (local debugging only).
 *
 * ## Reliability rules
 *
 * - All polling loops have a fixed upper bound (maxAttempts). No unbounded while(true).
 * - No sleep() calls — all waiting uses polling with bounded retries.
 * - ADR-014: @import dependency files are written BEFORE the entry file
 *   (mirrors watch.rs deps-before-entry order to ensure watchers see deps first).
 *
 * @module hmr-harness
 */

import { mkdirSync, writeFileSync, readFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';

// ---------------------------------------------------------------------------
// Platform gate — exported so specs can conditionally skip (decision D5)
// ---------------------------------------------------------------------------

/**
 * True when HMR filesystem-event tests should run.
 * Enabled on Linux (inotify) or when MDS_HMR=1 is set (developer override).
 */
export const HMR_ENABLED =
  process.platform === 'linux' || process.env.MDS_HMR === '1';

// ---------------------------------------------------------------------------
// createTempMdsProject
// ---------------------------------------------------------------------------

/**
 * Write a temporary project of MDS files under os.tmpdir().
 *
 * Files are written in the order they appear in the `files` array.
 * Per ADR-014, callers MUST list @import dependency files BEFORE the entry
 * file so watchers see dependencies registered before the entry is compiled.
 *
 * @param {Record<string, string>} files - Map of relative filename → content.
 *   Files are created in iteration order (object insertion order).
 * @returns {{ dir: string, paths: Record<string, string>, cleanup: () => void }}
 *   - `dir`: absolute path of the temp directory
 *   - `paths`: map of the same keys to their absolute paths
 *   - `cleanup`: call in afterEach / finally to remove the temp directory
 */
export function createTempMdsProject(files) {
  const dir = join(tmpdir(), `mds-hmr-${process.pid}-${Date.now()}`);
  mkdirSync(dir, { recursive: true });

  /** @type {Record<string, string>} */
  const paths = {};

  for (const [name, content] of Object.entries(files)) {
    const abs = join(dir, name);
    writeFileSync(abs, content, 'utf8');
    paths[name] = abs;
  }

  function cleanup() {
    try {
      rmSync(dir, { recursive: true, force: true });
    } catch {
      // Best-effort cleanup — ignore errors on Windows where files may be locked
    }
  }

  return { dir, paths, cleanup };
}

// ---------------------------------------------------------------------------
// editFile
// ---------------------------------------------------------------------------

/**
 * Overwrite the content of a file. Used to simulate edits in HMR tests.
 *
 * @param {string} filePath - Absolute path to the file.
 * @param {string} content - New file content.
 */
export function editFile(filePath, content) {
  writeFileSync(filePath, content, 'utf8');
}

// ---------------------------------------------------------------------------
// withTimeout
// ---------------------------------------------------------------------------

/**
 * Race a promise against a deadline. Rejects with a descriptive error if the
 * promise does not settle within `ms` milliseconds.
 *
 * @template T
 * @param {Promise<T>} promise
 * @param {number} ms - Timeout in milliseconds.
 * @param {string} [label] - Optional label for the error message.
 * @returns {Promise<T>}
 */
export function withTimeout(promise, ms, label = 'operation') {
  /** @type {ReturnType<typeof setTimeout>} */
  let timer;
  const timeout = new Promise((_resolve, reject) => {
    timer = setTimeout(
      () => reject(new Error(`withTimeout: ${label} timed out after ${ms}ms`)),
      ms,
    );
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

// ---------------------------------------------------------------------------
// waitFor
// ---------------------------------------------------------------------------

/**
 * Poll `predicate()` until it returns true or the deadline is exceeded.
 * All loops have a fixed upper bound (maxAttempts) — no unbounded while(true).
 *
 * @param {() => boolean | Promise<boolean>} predicate - Returns true when done.
 * @param {{ timeoutMs?: number, intervalMs?: number, label?: string }} [opts]
 * @returns {Promise<void>} Resolves when predicate returns true.
 * @throws {Error} When deadline is exceeded.
 */
export async function waitFor(predicate, opts = {}) {
  const { timeoutMs = 5000, intervalMs = 50, label = 'condition' } = opts;
  const maxAttempts = Math.ceil(timeoutMs / intervalMs);

  for (let attempt = 0; attempt < maxAttempts; attempt++) {
    const result = await predicate();
    if (result) return;
    // Bounded sleep between polls: use a Promise+setTimeout so we don't block
    // the event loop, but each sleep is a single fixed interval (not recursive).
    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  }

  throw new Error(
    `waitFor: ${label} did not become true within ${timeoutMs}ms ` +
      `(${maxAttempts} attempts at ${intervalMs}ms intervals)`,
  );
}

// ---------------------------------------------------------------------------
// waitForContent
// ---------------------------------------------------------------------------

/**
 * Poll a file until its content satisfies a predicate.
 * Bounded by `maxAttempts` (derived from timeoutMs / intervalMs).
 *
 * @param {string} filePath - Absolute path to the file to read.
 * @param {(content: string) => boolean} contentPredicate - Returns true when satisfied.
 * @param {{ timeoutMs?: number, intervalMs?: number, label?: string }} [opts]
 * @returns {Promise<string>} Resolves with the file content that satisfied the predicate.
 * @throws {Error} When deadline is exceeded.
 */
export async function waitForContent(filePath, contentPredicate, opts = {}) {
  const { timeoutMs = 5000, intervalMs = 50, label = 'file content' } = opts;
  const maxAttempts = Math.ceil(timeoutMs / intervalMs);

  let lastContent = '';
  for (let attempt = 0; attempt < maxAttempts; attempt++) {
    try {
      lastContent = readFileSync(filePath, 'utf8');
      if (contentPredicate(lastContent)) return lastContent;
    } catch {
      // File may not exist yet — keep polling (bounded)
    }
    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  }

  throw new Error(
    `waitForContent: ${label} at ${filePath} did not satisfy predicate within ` +
      `${timeoutMs}ms (${maxAttempts} attempts). Last content: ${lastContent.slice(0, 200)}`,
  );
}
