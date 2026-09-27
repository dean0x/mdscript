import { lstat, open, realpath } from 'node:fs/promises';
import { constants, existsSync } from 'node:fs';
import { resolve, dirname, basename, join, relative, isAbsolute, parse, sep } from 'node:path';
import type { MdsError } from '../types.js';
import {
  escapePathForMessage,
  firstForbiddenChar,
  forbiddenCharMessage,
  importPathViolation,
  sanitizeControlCharsWire,
} from './path-chars.js';

// O_NOFOLLOW prevents the kernel from following a symlink at the final path
// component. Using it closes the TOCTOU window between lstat and open.
// On Windows, O_NOFOLLOW is not defined; fall back to 0 (no-op flag) and
// rely on a post-open lstat check instead.
const O_NOFOLLOW: number = (constants as Record<string, number>)['O_NOFOLLOW'] ?? 0;

const MAX_PATH_SEGMENTS = 256;
const MAX_IMPORT_DEPTH = 64;
/** The Rust engine's per-file cap (`mds::MAX_FILE_SIZE`), which NativeFs applies to every module it reads. */
const MAX_FILE_SIZE = 10 * 1024 * 1024;
const MAX_TRAVERSAL_DEPTH = 256;
// Maximum concurrent file opens while reading ahead of the walk. Keeps file
// descriptor usage predictable even when a module imports many siblings at once.
const MAX_CONCURRENT_OPENS = 16;
// How many of a module's imports are located and read ahead of the walk at once. Each
// import walked makes room for the next, so a module listing many imports holds a
// bounded number of pending reads rather than one per import.
const MAX_IMPORTS_READ_AHEAD = 2 * MAX_CONCURRENT_OPENS;
const PROJECT_ROOT_MARKERS = ['.git', '.mdsroot'] as const;
/** The path separators Rust's `std::path::is_separator` accepts on this platform. */
const PATH_SEPARATORS = sep === '\\' ? /[\\/]/ : /\//;
export const DEFAULT_MAX_MODULES = 256;
export const DEFAULT_MAX_AGGREGATE_SIZE = 10 * 1024 * 1024; // 10 MiB

/**
 * Cache from start-directory → project-root result. The project root is
 * invariant within a single build, so repeated calls from the same start
 * directory (one per Webpack loader invocation) skip the traversal entirely.
 * Each traversal performs up to MAX_TRAVERSAL_DEPTH × |markers| synchronous
 * I/O calls, which can block the event loop on deep trees or network FSes.
 */
const projectRootCache = new Map<string, string>();

/**
 * Walk up from a directory to find the project root.
 *
 * Looks for `.git` or `.mdsroot` markers — the same markers the Rust
 * NativeFs::find_project_root uses. Falls back to the given directory if no
 * marker is found within MAX_TRAVERSAL_DEPTH parent directories.
 *
 * Results are cached by start directory: the project root is invariant within
 * a build, so repeated calls incur only a single Map lookup after the first.
 *
 * ARCHITECTURE EXCEPTION: Uses synchronous `existsSync` despite the otherwise
 * fully-async module pattern. This is a deliberate trade-off: (a) the result
 * is cached so the sync traversal runs at most once per unique start directory
 * per process, and (b) keeping this function synchronous avoids propagating
 * `async` through every call site (including test utilities that call it
 * directly). The blocking window is bounded by MAX_TRAVERSAL_DEPTH (256) ×
 * |markers| (2) I/O calls on an uncached first call — acceptable for a
 * one-time startup cost on local filesystems.
 */
export function findProjectRoot(start: string): string {
  const normalized = resolve(start);
  const cached = projectRootCache.get(normalized);
  if (cached !== undefined) {
    return cached;
  }

  const result = _findProjectRootUncached(normalized);
  projectRootCache.set(normalized, result);
  return result;
}

/**
 * Clear the project root cache. Intended for use in tests only — production
 * code should never call this because the cache is an intentional correctness
 * optimization (project root is invariant within a build).
 *
 * @internal
 */
export function _clearProjectRootCacheForTesting(): void {
  projectRootCache.clear();
}

function _findProjectRootUncached(start: string): string {
  let dir = start;
  for (let i = 0; i < MAX_TRAVERSAL_DEPTH; i++) {
    for (const marker of PROJECT_ROOT_MARKERS) {
      if (existsSync(resolve(dir, marker))) {
        return dir;
      }
    }
    const parent = dirname(dir);
    if (parent === dir) {
      return start;
    }
    dir = parent;
  }
  return start;
}

/**
 * Cross-platform check that `candidate` is the project root itself or nested
 * within it. Uses `path.relative` rather than string prefix matching so it is
 * correct on Windows (backslash separators, drive letters, case-insensitive
 * filesystem) as well as POSIX: a candidate outside the root yields a relative
 * path that is either absolute or begins with `..`.
 */
function isWithinRoot(root: string, candidate: string): boolean {
  if (candidate === root) {
    return true;
  }
  const rel = relative(root, candidate);
  return rel.length > 0 && rel !== '..' && !rel.startsWith('..' + sep) && !isAbsolute(rel);
}

/**
 * Open a file descriptor with O_NOFOLLOW | O_RDONLY, translating the ELOOP error
 * the kernel emits when the path is a symlink into NativeFs's symlink refusal
 * (`symlinkError`), and every other failure into the error the Rust engine reports
 * at the same step — each keyed on `shown` rather than the resolved `absolutePath`
 * (R3 / CWE-209 — the raw Node error names the resolved filesystem path in its
 * message).
 *
 * NativeFs stats the final component (`symlink_metadata`) before it reads it and
 * maps any failure of that step to `mds::file_not_found`: so does this, when the
 * failed path cannot be `lstat`ed either — no such file (ENOENT; e.g. a
 * case-mismatched spelling on a case-sensitive volume, #408), a regular file where
 * a directory is expected (ENOTDIR), a name too long (ENAMETOOLONG), a directory
 * that cannot be searched (EACCES). A file that stats but cannot be opened (EACCES
 * on the file itself) failed at NativeFs's read step instead: `mds::io`.
 *
 * Module-level helper (not a closure) so that the caller's own try/catch only
 * handles post-open validation, keeping nesting shallow.
 */
async function openNoFollow(
  absolutePath: string,
  shown: string,
): Promise<Awaited<ReturnType<typeof open>>> {
  try {
    return await open(absolutePath, constants.O_RDONLY | O_NOFOLLOW);
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code === 'ELOOP') {
      throw symlinkError(shown);
    }
    throw await lstat(absolutePath).then(
      () => readError(shown, err),
      () => fileNotFoundError(shown),
    );
  }
}

/**
 * Close `handle`, reporting a failure as the read failure it is (`readError`), never
 * as Node's raw error.
 */
async function closeModule(handle: Awaited<ReturnType<typeof open>>, shown: string): Promise<void> {
  await handle.close().catch((err: unknown) => {
    throw readError(shown, err);
  });
}

// ---------------------------------------------------------------------------
// Path refusals shared with the Rust engine (#265)
// ---------------------------------------------------------------------------

/** The codes a scanner refusal carries — each the one the Rust engine reports for the same step. */
type PathErrorCode = 'mds::import' | 'mds::io' | 'mds::file_not_found' | 'mds::resource_limit';

/**
 * A path refusal carrying the code the Rust engine reports for the same input.
 * Each message is byte-identical to that engine error's message as the native
 * backend throws it, so the WASM backend's file operations fail exactly like
 * the native ones.
 */
type PathError = MdsError & { code: PathErrorCode };

/**
 * The `help` the Rust engine attaches to an error of each code
 * (crates/mds-core/src/error.rs). Only `mds::file_not_found` carries one; a code
 * absent here carries none on either backend (#414).
 */
const PATH_ERROR_HELP: Readonly<Partial<Record<PathErrorCode, string>>> = {
  'mds::file_not_found': 'check the file path and ensure the file exists',
};

/** Build a refusal with `code`, `message` and — only where the engine has one — its `help`. */
function pathError(code: PathErrorCode, message: string): PathError {
  const err = new Error(message) as PathError;
  err.code = code;
  const help = PATH_ERROR_HELP[code];
  if (help !== undefined) {
    err.help = help;
  }
  return err;
}

/** `mds::import`, with the `import error: ` prefix the Rust error's display adds. */
function importError(detail: string): PathError {
  return pathError('mds::import', `import error: ${detail}`);
}

/**
 * `mds::file_not_found`, matching Rust `MdsError::file_not_found`'s message
 * shape (`"file not found: {path}"`) exactly, keyed on `shown` — the path as
 * written — never the resolved filesystem path.
 *
 * `shown` has passed `entryPathError`/`importPathError` by the time a file is
 * looked for, so escaping it changes nothing; it is escaped anyway so no message
 * this module builds can carry a forbidden character.
 */
function fileNotFoundError(shown: string): PathError {
  return pathError('mds::file_not_found', `file not found: ${escapePathForMessage(shown)}`);
}

/**
 * `mds::io` for a file that resolves but cannot be read — NativeFs's `cannot read
 * …` I/O error — naming `shown`, the path as written, and `reason`: an errno name
 * (`EACCES`), never Node's message, which names the resolved absolute path. Native
 * names its root-relative display path and the OS error text instead, so the two
 * agree on the code, not on the message.
 */
function cannotReadError(shown: string, reason: string): PathError {
  return pathError(
    'mds::io',
    `cannot read ${escapePathForMessage(shown)}: ${escapePathForMessage(reason)}`,
  );
}

/** `cannotReadError` for a failed Node call, with its errno name as the reason. */
function readError(shown: string, err: unknown): PathError {
  const errno = (err as NodeJS.ErrnoException | undefined)?.code;
  return cannotReadError(shown, typeof errno === 'string' ? errno : 'I/O error');
}

/**
 * `mds::import` for a module whose final path component is a symlink — the refusal
 * NativeFs::check_symlink_named makes for an entry path and an import alike —
 * keyed on `shown`, the path as written.
 */
function symlinkError(shown: string): PathError {
  return importError(`symlinks are not allowed in imports: ${escapePathForMessage(shown)}`);
}

/**
 * `mds::import` for a module outside the project root — NativeFs's
 * `check_path_traversal` — keyed on `shown`, the import as written.
 */
function escapesProjectError(shown: string): PathError {
  return importError(`import path escapes project directory: "${escapePathForMessage(shown)}"`);
}

/** `mds::resource_limit`, with the `resource limit exceeded: ` prefix the Rust error's display adds. */
function resourceLimitError(detail: string): PathError {
  return pathError('mds::resource_limit', `resource limit exceeded: ${detail}`);
}

/** The Rust resolver's refusal of an import chain deeper than `MAX_IMPORT_DEPTH` modules. */
function importDepthError(): PathError {
  return importError(`import depth exceeds maximum of ${MAX_IMPORT_DEPTH} (possible deep chain)`);
}

/** The Rust resolver's module-count refusal, naming how many modules were resolved. */
function moduleCountError(maxModules: number, resolved: number): PathError {
  return resourceLimitError(`module count exceeds maximum of ${maxModules} (${resolved} modules resolved)`);
}

/** NativeFs's refusal of a module over `MAX_FILE_SIZE`, naming its root-relative path. */
function fileTooLargeError(size: number, display: string): PathError {
  return resourceLimitError(
    `file too large (${size} bytes, max ${MAX_FILE_SIZE} bytes): ${escapePathForMessage(display)}`,
  );
}

/**
 * The refusal of a path of more than `MAX_PATH_SEGMENTS` segments, if any — Rust
 * `check_segment_count`, which NativeFs runs on an entry path and an import string
 * as written and VirtualFs on an entry key: the non-empty segments other than `.`,
 * split on this platform's separators; `..` counts.
 */
function segmentCountError(path: string): PathError | undefined {
  const segments = path.split(PATH_SEPARATORS).filter((s) => s.length > 0 && s !== '.').length;
  return segments > MAX_PATH_SEGMENTS ? segmentCapError(path) : undefined;
}

/** `mds::resource_limit` for a path over `MAX_PATH_SEGMENTS` segments. */
function segmentCapError(path: string): PathError {
  return resourceLimitError(
    `import path exceeds maximum segment count (${MAX_PATH_SEGMENTS}): "${sanitizeControlCharsWire(path)}"`,
  );
}

/**
 * `path`'s parent directory and final name as Rust's `Path::parent` and
 * `Path::file_name` see them: repeated and trailing separators and `.` components do
 * not count, and `..` is kept as written — the OS applies it, after any symlink
 * before it, when the parent is canonicalized. `name` is `undefined` when the path
 * has no final name (`.`, `./`, a root); an empty parent is `.`, as Rust
 * `effective_parent` makes it.
 */
function nativeParentAndName(path: string): { parent: string; name: string | undefined } {
  const { root } = parse(path);
  const parts = path.slice(root.length).split(PATH_SEPARATORS).filter((p) => p.length > 0 && p !== '.');
  const name = parts.pop();
  const parent = root + parts.join(sep);
  return { parent: parent.length > 0 ? parent : '.', name };
}

/**
 * Canonicalize the directory `dir`, translating every failure — it does not exist
 * (ENOENT), a path component above it is a regular file (ENOTDIR), a symlink loop
 * (ELOOP), a name too long (ENAMETOOLONG), a directory that cannot be searched
 * (EACCES) — into the `mds::file_not_found` shape `fileNotFoundError` builds, keyed
 * on `shown`, never Node's error, which names the raw absolute path (R3 / CWE-209,
 * #408).
 *
 * Mirrors Rust `check_symlink_named`, whose `parent.canonicalize()` step maps
 * every canonicalize failure on the parent — it does not distinguish errno —
 * to `MdsError::file_not_found(shown)`. `realpath` resolves each `..` physically,
 * after the symlinks before it, as the OS does for NativeFs.
 */
async function canonicalDirectory(dir: string, shown: string): Promise<string> {
  try {
    return await realpath(dir);
  } catch {
    throw fileNotFoundError(shown);
  }
}

/** `canonicalDirectory` of `path`'s parent directory. */
async function realpathParent(path: string, shown: string): Promise<string> {
  return canonicalDirectory(dirname(path), shown);
}

/** The result of `promise`, or what it threw — never a rejection. */
type Settled<T> = { readonly ok: true; readonly value: T } | { readonly ok: false; readonly error: unknown };

function settle<T>(promise: Promise<T>): Promise<Settled<T>> {
  return promise.then(
    (value): Settled<T> => ({ ok: true, value }),
    (error: unknown): Settled<T> => ({ ok: false, error }),
  );
}

/** The value of `settled`, or a throw of what it failed with. */
function unwrap<T>(settled: Settled<T>): T {
  if (!settled.ok) {
    throw settled.error;
  }
  return settled.value;
}

/**
 * Run tasks at most `max` at a time, in the order they were submitted. Each task is
 * a thunk, started only once a slot is free.
 */
function concurrencyLimit(max: number): <T>(task: () => Promise<T>) => Promise<T> {
  let active = 0;
  const waiting: Array<() => void> = [];
  return async <T>(task: () => Promise<T>): Promise<T> => {
    if (active >= max) {
      await new Promise<void>((start) => waiting.push(start));
    } else {
      active += 1;
    }
    try {
      return await task();
    } finally {
      const next = waiting.shift();
      if (next === undefined) {
        active -= 1;
      } else {
        // The slot passes straight to the next task: `active` is unchanged.
        next();
      }
    }
  };
}

/**
 * The refusal for an entry path or virtual entry key, if any — mirrors Rust
 * `validate_entry_path` (empty, then NUL, then the rest of the forbidden class;
 * all `mds::io`).
 */
function entryPathError(path: string): PathError | undefined {
  if (path.length === 0) {
    return pathError('mds::io', 'entry path is empty');
  }
  if (path.includes('\0')) {
    return pathError('mds::io', `entry path contains null byte: "${escapePathForMessage(path)}"`);
  }
  const cp = firstForbiddenChar(path);
  return cp === undefined ? undefined : pathError('mds::io', forbiddenCharMessage('entry path', cp, path));
}

/**
 * The refusal for an import resolved within a directory, if any — mirrors Rust
 * `validate_relative_import`, which `VirtualFs::normalize_in_dir` runs (empty,
 * then NUL, then the rest of the forbidden class; all `mds::import`).
 */
function relativeImportError(relative: string): PathError | undefined {
  if (relative.length === 0) {
    return importError('import path is empty');
  }
  if (relative.includes('\0')) {
    return importError('import path contains null byte');
  }
  const cp = firstForbiddenChar(relative);
  return cp === undefined ? undefined : importError(forbiddenCharMessage('import path', cp, relative));
}

/**
 * The refusal for an import string as written in a module, if any — mirrors the
 * Rust resolver's `validate_import_path`, which runs before any filesystem
 * backend is called (relative form, then NUL, then the forbidden class).
 */
function importPathError(importPath: string): PathError | undefined {
  const violation = importPathViolation(importPath);
  if (violation === undefined) {
    return undefined;
  }
  switch (violation.kind) {
    case 'not-relative':
      return importError(
        `import path must be relative (start with './' or '../'): "${escapePathForMessage(importPath)}"`,
      );
    case 'null-byte':
      return importError('import path contains null byte');
    case 'forbidden-char':
      return importError(forbiddenCharMessage('import path', violation.codePoint, importPath));
    default: {
      const exhaustive: never = violation;
      throw new Error(`unknown import path violation: ${JSON.stringify(exhaustive)}`);
    }
  }
}

/**
 * The refusal for a resolved path that carries a forbidden character anywhere,
 * if any — mirrors Rust `reject_forbidden_in_path`. This is what catches a
 * hostile directory name the caller never wrote, reached through a symlinked
 * directory. `shown` is the path as written; the resolved one is never shown.
 */
function resolvedPathError(resolved: string, shown: string): PathError | undefined {
  const cp = firstForbiddenChar(resolved);
  return cp === undefined ? undefined : pathError('mds::io', forbiddenCharMessage('resolved path', cp, shown));
}

/**
 * The WASM engine calls the scanner makes — the WASM module's own exports, so every
 * check they make is the Rust engine's, never a TypeScript copy of it (#414).
 */
export interface ScannerEngine {
  /** The import paths a module's source names, in the order the resolver resolves them. */
  scanImports(source: string): string[];
  /**
   * A module file's text, checked as the native backend checks every file it reads,
   * or a throw of the native error: its bytes as NativeFs checks them
   * (`mds::check_module_bytes`: the per-file cap, then UTF-8), then its type as the
   * resolver checks it before parsing (`mds::check_module_type`: `mds::not_mds`).
   * `display` is the file's path below the project root, in its on-disk spelling;
   * `shown` is the path the caller typed to reach it, which a `not_mds` error names.
   */
  preflightModule(bytes: Uint8Array, display: string, shown: string): string;
}

export interface ModuleScannerOptions {
  /**
   * How many modules besides the entry a scan may resolve — the WASM engine takes
   * the entry plus this many. Default 256, the engine's own limit.
   */
  maxModules?: number;
  /**
   * The most bytes the entry and its imports may hold together — a guard of the
   * WASM backend's own, with no native counterpart. Default 10 MiB.
   */
  maxAggregateSize?: number;
}

export interface BuildModulesMapResult {
  entryFilename: string;
  /**
   * Flat map of virtual filename → source for the entry file and all its
   * transitive imports. The entry file itself is included, keyed by
   * `entryFilename`. Callers that pass `modules` as extra dependencies to the
   * WASM `build_modules()` function MUST extract and remove the entry source
   * before the call — leaving the entry key present causes
   * `mds::filename_collision` because `build_modules()` also inserts the entry
   * source under `filename`.
   */
  modules: Record<string, string>;
}

/**
 * Normalize a virtual module key the same way the Rust resolver does on VirtualFs:
 * `VirtualFs::normalize_in_dir(parent_dir(base), relative)` for an import, and
 * `VirtualFs::resolve_entry(relative)` (key unchanged) when `base` is empty.
 *
 * Given a base key (the key of the importing module) and a relative import path,
 * resolve the import path to a canonical slash-separated key.
 *
 * Every refusal is VirtualFs's, code and message alike (#265, #414): an entry key
 * that is empty, contains NUL or carries a forbidden path character (`mds::io`), or
 * has more than 256 segments (`mds::resource_limit`); an import with the same
 * defects (`mds::import`), one that climbs above the key space's root with `..` or
 * resolves to no key at all (`mds::import`), and one whose key would pass 256
 * segments (`mds::resource_limit`).
 *
 * MUST exactly mirror the Rust implementation to ensure import resolution matches.
 */
export function normalizeVirtualKey(base: string, relative: string): string {
  if (base.length === 0) {
    const entryErr = entryPathError(relative) ?? segmentCountError(relative);
    if (entryErr !== undefined) {
      throw entryErr;
    }
    // Root entry point — the key is used as is.
    return relative;
  }

  const importErr = relativeImportError(relative);
  if (importErr !== undefined) {
    throw importErr;
  }

  // Resolve relative to the directory portion of base (split on '/').
  const lastSlash = base.lastIndexOf('/');
  const baseDir = lastSlash >= 0 ? base.slice(0, lastSlash) : '';
  const segments: string[] = baseDir.length > 0
    ? baseDir.split('/').filter((s) => s.length > 0)
    : [];

  for (const part of relative.split('/')) {
    if (part === '' || part === '.') {
      // skip
    } else if (part === '..') {
      if (segments.length === 0) {
        throw escapesProjectError(relative);
      }
      segments.pop();
    } else {
      if (segments.length >= MAX_PATH_SEGMENTS) {
        throw segmentCapError(relative);
      }
      segments.push(part);
    }
  }

  if (segments.length === 0) {
    throw importError(`import path resolves to empty key: "${escapePathForMessage(relative)}"`);
  }

  return segments.join('/');
}

/** The virtual key of `path`: its path below `root`, slash-separated as VirtualFs keys are. */
function keyOf(root: string, path: string): string {
  return relative(root, path).split(sep).join('/');
}

/**
 * NativeFs's `symlink_metadata` step: a final component that does not resolve is
 * not found; one whose own file type is a symlink — a junction too, on Windows —
 * is refused. Nothing is opened.
 */
async function assertFileNotSymlink(path: string, shown: string): Promise<void> {
  let isSymlink: boolean;
  try {
    isSymlink = (await lstat(path)).isSymbolicLink();
  } catch {
    throw fileNotFoundError(shown);
  }
  if (isSymlink) {
    throw symlinkError(shown);
  }
}

/** A module located on disk as NativeFs resolves it, before anything in it is read. */
interface Located {
  /** The module's key in the engine's virtual module map. */
  readonly key: string;
  /** Its canonical directory joined with its name as written. */
  readonly path: string;
  /** Its canonical directory. */
  readonly dir: string;
}

/** A module file opened, checked and read. */
interface ReadModule {
  /** Its canonical path. */
  readonly resolved: string;
  /** Its size in bytes. */
  readonly size: number;
  /** Its text. */
  readonly content: string;
}

/** An import of a walked module: located, and read too when read ahead of the walk. */
interface ReadAhead {
  readonly located: Settled<Located>;
  readonly read: Settled<ReadModule> | undefined;
}

/** The outcome of a read-ahead that starts after the walk has already finished. */
const WALK_FINISHED: Settled<never> = { ok: false, error: undefined };

/**
 * Locate the entry file as NativeFs's `resolve_entry` does, before anything under it
 * is read: the entry path is validated as written (`mds::io`), its segments counted
 * (`mds::resource_limit`), and a path with no final name — `.`, a root, one ending in
 * `..` — is not found. Its parent directory is canonicalized as typed, so the OS
 * applies each `..` after the symlinks before it (never lexically), and the final
 * component is refused when missing or a symlink, then when the canonical path
 * carries a forbidden path character.
 */
async function locateEntry(entryPath: string): Promise<{ path: string; dir: string }> {
  const entryErr = entryPathError(entryPath) ?? segmentCountError(entryPath);
  if (entryErr !== undefined) {
    throw entryErr;
  }
  const { parent, name } = nativeParentAndName(entryPath);
  if (name === undefined || name === '..') {
    throw fileNotFoundError(entryPath);
  }
  const dir = await canonicalDirectory(parent, entryPath);
  const path = join(dir, name);
  await assertFileNotSymlink(path, entryPath);
  const hostileErr = resolvedPathError(path, entryPath);
  if (hostileErr !== undefined) {
    throw hostileErr;
  }
  return { path, dir };
}

/**
 * Recursively resolve an MDS file and all its imports into a flat modules map
 * suitable for passing to the WASM compile/check functions.
 *
 * The returned `entryFilename` is a **project-root-relative** slash path
 * (e.g. `"src/templates/foo.mds"`), computed via `path.relative(projectRoot,
 * absoluteEntry)`. This mirrors the virtual key used in the `modules` map and
 * is the value that must be passed as the `filename` argument to
 * `build_modules()` / `check()` on the WASM side.
 *
 * Note: prior to this change `entryFilename` was the basename of the entry
 * file. Callers that relied on the basename form must be updated to use the
 * relative path.
 *
 * Every refusal is the error the native backend throws for the same input — its
 * code, its message and its `help` — naming the path as written, never a resolved
 * absolute path (#265, #408, #414):
 * - an entry path that is empty, contains NUL or carries a forbidden path character
 *   (`mds::io`), and an import string that is not `./`/`../`-relative, contains NUL
 *   or carries one (`mds::import`), before the filesystem is touched;
 * - an entry path or import string of more than 256 segments, counted as written
 *   (`mds::resource_limit`);
 * - a module that cannot be resolved (`mds::file_not_found`): missing, blocked by a
 *   non-directory path component, a symlink loop, a name too long, a directory that
 *   cannot be searched, or a path with no final name (one ending in `..`);
 * - a module whose final path component is a symlink, judged by that component's own
 *   file type (O_NOFOLLOW open; `lstat` where O_NOFOLLOW is unavailable) (`mds::import`,
 *   `symlinks are not allowed in imports: <path as written>`). Symlinked parent
 *   directories are followed, as NativeFs follows them;
 * - a module outside the project root (discovered via .git/.mdsroot markers), checked
 *   lexically and on the canonical path (`mds::import`, `import path escapes project
 *   directory: "<import as written>"`);
 * - a module whose resolved path carries a forbidden path character — a hostile-named
 *   directory reached through a symlink (`mds::io`);
 * - an import chain deeper than 64 modules (`mds::import`); more modules than the
 *   engine takes, the entry plus `maxModules` (`mds::resource_limit`);
 * - a module that is not a regular file or cannot be read (`mds::io`, `cannot read
 *   <path as written>: <errno name>`);
 * - a file over the engine's 10 MiB per-file cap (`mds::resource_limit`) or whose
 *   bytes are not valid UTF-8 (`mds::io`), then a file that is neither a `.mds` file
 *   nor a `.md` file declaring `type: mds` (`mds::not_mds`, `not an MDS file: <path as
 *   typed>`, #417) — refused by the engine's own `preflightModule`, the checks the
 *   native backend makes on every file it reads, which also decodes the file (a
 *   leading byte-order mark kept). A file that is not an MDS file is refused before
 *   any import-like line in it is followed.
 *
 * A project may be rooted at the filesystem root, as on native.
 *
 * Modules are read concurrently — at most MAX_CONCURRENT_OPENS files open at once,
 * at most MAX_IMPORTS_READ_AHEAD of a module's imports and no more than
 * `maxAggregateSize` bytes read ahead of the walk — but checked in
 * the order the native resolver resolves them: depth first, each module's imports in
 * the order `scanImports` lists them, each step's checks in `resolve_by_key`'s order.
 * Of several faults, the one reported is the one the native backend reports.
 *
 * Differences from the native backend that remain (#414):
 * 1. A MISSING module outside the project root — named lexically, or through a
 *    symlinked directory — is refused as escaping it, where native reports it not
 *    found. Deliberate: containment is decided before the file is looked at, so the
 *    scanner is no existence oracle for files outside the project (U-SM30, U-SM32).
 * 2. The aggregate-size guard (`maxAggregateSize`) is the WASM backend's own; native
 *    has none. It judges a module only once native's own checks on it pass, so it
 *    never pre-empts a refusal native makes of that module (U-SM39).
 * 3. A `../` import from a project rooted at the filesystem root is refused as
 *    escaping it, where native resolves `/..` to `/` (#424).
 * 4. Module count: native refuses a module once 256 others are fully resolved, so a
 *    graph whose modules are still being resolved can hold more; the engine takes
 *    the entry plus 256, and a larger graph is refused here (#427).
 * 5. An import that leaves a symlinked directory through `..` is refused
 *    (`mds::import`): the engine resolves it by name, NativeFs on disk (#408).
 * 6. A module more than 256 directories below the project root is refused: the
 *    engine's key of it is capped at 256 segments, where native caps only the path
 *    as written.
 * 7. A `cannot read` message names the path as written and the errno name, where
 *    native names the root-relative path and the OS error text; the code agrees.
 * 8. Every module is read before the engine runs, so a refusal of a later import is
 *    reported before an error the engine raises at a point native reaches first: a
 *    compile error in a module native reads first, or a circular import native meets
 *    first (U-SM38).
 */
export async function buildModulesMap(
  entryPath: string,
  engine: ScannerEngine,
  options?: ModuleScannerOptions,
): Promise<BuildModulesMapResult> {
  const maxModules = options?.maxModules ?? DEFAULT_MAX_MODULES;
  const maxAggregateSize = options?.maxAggregateSize ?? DEFAULT_MAX_AGGREGATE_SIZE;

  const entry = await locateEntry(entryPath);
  // A project may be rooted at the filesystem root, as on native: containment in it
  // then admits every path, exactly as NativeFs's does.
  const projectRoot = findProjectRoot(entry.dir);
  // Virtual keys are always slash-separated to mirror Rust's VirtualFs.
  const entryFilename = keyOf(projectRoot, entry.path);

  const modules: Record<string, string> = {};
  /** Keys of modules walked to the end: the native resolver's module cache. */
  const completed = new Set<string>();
  /** Keys of modules being walked: the native resolver's stack of modules resolving. */
  const walking = new Set<string>();
  /** Keys a read-ahead has already read, or is reading. */
  const readAheadKeys = new Set<string>();
  const limit = concurrencyLimit(MAX_CONCURRENT_OPENS);
  let admitted = 0;
  let aggregateSize = 0;
  let readAheadSize = 0;
  let walkFinished = false;

  /**
   * Refuse an import whose module key names another directory than the one the
   * native backend reads it from.
   *
   * The WASM engine resolves an import by name, from the importing module's key.
   * NativeFs resolves it on disk, from the importing module's canonical directory,
   * where the OS applies each `..` after the symbolic links before it. The two
   * agree for every import except one that leaves a symlinked directory through
   * `..`: its key names the directory beside the link, while the file on disk sits
   * beside the link's target. Whichever file were stored under that key, the
   * engine would compile a module the native backend never reads (#408).
   */
  async function assertKeyMatchesDisk(
    importerDir: string,
    importPath: string,
    childKey: string,
  ): Promise<void> {
    // realpath() resolves each `..` physically, after the links before it — the
    // directory NativeFs reads from. A missing directory is file-not-found there.
    const onDisk = await realpathParent(importerDir + sep + importPath, importPath);
    // The directory the engine's key names. If it cannot be resolved at all, it
    // is not the directory above, and the import is refused below.
    let byName: string | undefined;
    try {
      byName = await realpath(dirname(join(projectRoot, ...childKey.split('/'))));
    } catch {
      byName = undefined;
    }
    if (byName !== onDisk) {
      throw importError(
        `import path leaves a symlinked directory through '..', which the WASM backend cannot resolve: "${escapePathForMessage(importPath)}"`,
      );
    }
  }

  /**
   * Locate an import of `importer` as the native resolver does — `validate_import_path`,
   * then NativeFs's `normalize_in_dir` — refusing it with native's error, except that
   * containment in the project root is decided before the file is looked at
   * (difference 1 above).
   */
  async function locateImport(importer: Located, importPath: string): Promise<Located> {
    const stringErr = importPathError(importPath) ?? segmentCountError(importPath);
    if (stringErr !== undefined) {
      throw stringErr;
    }
    const { name } = nativeParentAndName(importPath);
    // A path ending in `..` has no final name: NativeFs finds no file there.
    if (name === '..') {
      throw fileNotFoundError(importPath);
    }
    const childAbsolute = resolve(importer.dir, importPath);
    // Security: containment in the project root — lexically, before anything outside
    // the root is touched.
    if (!isWithinRoot(projectRoot, childAbsolute)) {
      throw escapesProjectError(importPath);
    }
    // An import of `./` names the importing module's own directory, which NativeFs
    // opens and fails to read (`mds::io`); the engine has no key for it.
    let key = '';
    if (name !== undefined) {
      key = normalizeVirtualKey(importer.key, importPath);
      await assertKeyMatchesDisk(importer.dir, importPath, key);
    }
    // The parent directory canonicalized (its symlinks followed), the final
    // component joined as written — the pattern of NativeFs::check_symlink_named.
    const dir = await realpathParent(childAbsolute, importPath);
    const path = join(dir, basename(childAbsolute));
    // Security: containment on the canonical path, so a symlinked directory cannot
    // lead outside the project root.
    if (!isWithinRoot(projectRoot, path)) {
      throw escapesProjectError(importPath);
    }
    await assertFileNotSymlink(path, importPath);
    // Security (#265): a directory the import never named, reached through a
    // symlink, can carry a forbidden path character.
    const hostileErr = resolvedPathError(path, importPath);
    if (hostileErr !== undefined) {
      throw hostileErr;
    }
    return { key, path, dir };
  }

  /**
   * Open a located module with O_NOFOLLOW and read it, as NativeFs's `read` does:
   * `mds::io` when it cannot be read, `file too large` over the per-file cap. Given
   * `admit`, it is read only when `admit` accepts its size, and `undefined` otherwise.
   *
   * O_NOFOLLOW makes open() fail with ELOOP when the final component was swapped for
   * a symlink after it was located, so no link is followed between the check and the
   * read. Where O_NOFOLLOW is unavailable (Windows) open() follows it, and `lstat` —
   * which reports a junction as a symlink too — refuses it before a byte is read.
   * Canonicalizing a non-symlink final component only respells its name; a canonical
   * path in another directory means the component was replaced by a link — refused as
   * NativeFs refuses it. The file type decides, never a comparison of the canonical
   * path with the path as written: on a case-insensitive volume realpath returns the
   * on-disk spelling, so `Entry.mds` for `entry.mds` differs from its canonical form
   * without being a symlink (#408).
   */
  async function readModule(located: Located, shown: string): Promise<ReadModule>;
  async function readModule(
    located: Located,
    shown: string,
    admit: (size: number) => boolean,
  ): Promise<ReadModule | undefined>;
  async function readModule(
    located: Located,
    shown: string,
    admit?: (size: number) => boolean,
  ): Promise<ReadModule | undefined> {
    const handle = await openNoFollow(located.path, shown);
    try {
      // `openNoFollow` above already succeeded, so the file existed a moment ago; a
      // failure here can only come from a concurrent change (TOCTOU). It is still
      // reported as the engine would report it: `lstat`/`realpath` are NativeFs's
      // stat and canonicalize steps (file-not-found), the fd-based `stat` is part of
      // reading the file.
      const [stats, linkStats, resolved] = await Promise.all([
        handle.stat().catch((err: unknown) => {
          throw readError(shown, err);
        }),
        lstat(located.path).catch(() => {
          throw fileNotFoundError(shown);
        }),
        realpath(located.path).catch(() => {
          throw fileNotFoundError(shown);
        }),
      ]);
      if (linkStats.isSymbolicLink() || dirname(resolved) !== located.dir) {
        throw symlinkError(shown);
      }
      // fstat on the opened fd: a module that is not a regular file (a directory,
      // device, FIFO or socket) fails NativeFs's read step (`mds::io`); a directory
      // is named by the errno reading it reports.
      if (!stats.isFile()) {
        throw cannotReadError(shown, stats.isDirectory() ? 'EISDIR' : 'not a regular file');
      }
      const display = keyOf(projectRoot, resolved);
      // Checked on the fstat size too, before a byte is read, so a file over the cap
      // when it is opened is never read into memory; one that grows past it while it
      // is read is refused by the engine's own check below.
      if (stats.size > MAX_FILE_SIZE) {
        throw fileTooLargeError(stats.size, display);
      }
      if (admit !== undefined && !admit(stats.size)) {
        return undefined;
      }
      const bytes = await handle.readFile().catch((err: unknown) => {
        throw readError(shown, err);
      });
      // The engine's own checks, in the native order — the per-file cap and UTF-8
      // (#414), then the file type (#417) — decode it: it refuses exactly the files the
      // native backend refuses, and a file that is not an MDS file is refused before
      // any import-like line in it is followed.
      return { resolved, size: bytes.length, content: engine.preflightModule(bytes, display, shown) };
    } finally {
      await closeModule(handle, shown);
    }
  }

  /**
   * Locate an import and, when its module is new and the read-ahead budget allows,
   * read it — ahead of the walk, which checks the outcome in the native order. Never
   * rejects: every failure is settled for the walk to report when it gets there.
   */
  async function readAhead(importer: Located, importPath: string): Promise<ReadAhead> {
    if (walkFinished) {
      return { located: WALK_FINISHED, read: undefined };
    }
    const located = await settle(locateImport(importer, importPath));
    if (!located.ok || walkFinished) {
      return { located, read: undefined };
    }
    const { key } = located.value;
    if (completed.has(key) || walking.has(key) || readAheadKeys.has(key)) {
      return { located, read: undefined };
    }
    readAheadKeys.add(key);
    const read = await settle(
      readModule(located.value, importPath, (size) => {
        if (readAheadSize + size > maxAggregateSize) {
          return false;
        }
        readAheadSize += size;
        return true;
      }),
    );
    if (!read.ok) {
      return { located, read };
    }
    // A module the read-ahead budget left unread is read by the walk itself.
    return { located, read: read.value === undefined ? undefined : { ok: true, value: read.value } };
  }

  /**
   * Walk the located module, reached as `shown` `depth` imports below the entry, then
   * its imports in order — the native resolver's `resolve_by_key`: a module already
   * resolved, or still resolving (a cycle, the engine's to report), is not read again,
   * and the depth and module-count limits apply before the file is read.
   */
  async function walk(
    located: Located,
    shown: string,
    depth: number,
    readAheadOutcome: Settled<ReadModule> | undefined,
  ): Promise<void> {
    const { key } = located;
    if (completed.has(key) || walking.has(key)) {
      return;
    }
    if (depth >= MAX_IMPORT_DEPTH) {
      throw importDepthError();
    }
    // Native refuses a module once `maxModules` others are fully resolved. The engine
    // takes the entry plus `maxModules`, which also caps a graph whose modules are
    // still resolving (difference 4 above).
    if (completed.size >= maxModules) {
      throw moduleCountError(maxModules, completed.size);
    }
    if (admitted > maxModules) {
      throw moduleCountError(maxModules, admitted);
    }
    admitted += 1;

    // A module no read-ahead read is read here, whatever its size: the per-file cap
    // bounds it.
    const read = readAheadOutcome === undefined
      ? await readModule(located, shown)
      : unwrap(readAheadOutcome);
    // The WASM backend's own guard, checked in walk order once the module has passed
    // every check native makes on it — the per-file cap, UTF-8 and the file type — so
    // it never reports a module native refuses, whichever reads the read-ahead budget
    // let run ahead of the walk (difference 2 above).
    aggregateSize += read.size;
    if (aggregateSize > maxAggregateSize) {
      throw resourceLimitError(`aggregate module size exceeds maximum of ${maxAggregateSize} bytes`);
    }
    modules[key] = read.content;

    // The next MAX_IMPORTS_READ_AHEAD imports are read ahead of the walk; each one
    // walked starts the next.
    const upcoming = engine.scanImports(read.content).values();
    const queued: Array<{ readonly importPath: string; readonly ahead: Promise<ReadAhead> }> = [];
    const refill = (): void => {
      while (queued.length < MAX_IMPORTS_READ_AHEAD) {
        const step = upcoming.next();
        if (step.done === true) {
          return;
        }
        const importPath = step.value;
        queued.push({ importPath, ahead: limit(() => readAhead(located, importPath)) });
      }
    };
    walking.add(key);
    refill();
    for (let item = queued.shift(); item !== undefined; item = queued.shift()) {
      refill();
      const { located: child, read: childRead } = await item.ahead;
      await walk(unwrap(child), item.importPath, depth + 1, childRead);
    }
    walking.delete(key);
    completed.add(key);
  }

  try {
    await walk({ key: entryFilename, path: entry.path, dir: entry.dir }, entryPath, 0, undefined);
  } finally {
    walkFinished = true;
  }

  return { entryFilename, modules };
}
