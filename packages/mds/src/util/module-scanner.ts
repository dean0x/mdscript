// Naming in this file: `shown` is a path as the caller wrote it, raw until it enters a
// message (the engine calls it `typed`); `display` is a path below the project root, in
// its on-disk spelling.
import { isUtf8 } from 'node:buffer';
import { lstat, open, realpath } from 'node:fs/promises';
import { constants, existsSync } from 'node:fs';
import { resolve, dirname, basename, join, relative, isAbsolute, parse, sep } from 'node:path';
import { isMdsError, type MdsError } from '../types.js';
import type { ImportRecord, WasmModule } from '../backend/wasm.js';
import {
  escapePathForMessage,
  firstForbiddenChar,
  forbiddenCharMessage,
  importPathViolation,
  sanitizeControlCharsWire,
} from './path-chars.js';
import { slidingWindow } from './sliding-window.js';

// O_NOFOLLOW prevents the kernel from following a symlink at the final path
// component. Using it closes the TOCTOU window between lstat and open.
// On Windows, O_NOFOLLOW is not defined; fall back to 0 (no-op flag) and
// rely on a post-open lstat check instead.
const O_NOFOLLOW: number = (constants as Record<string, number>)['O_NOFOLLOW'] ?? 0;

const MAX_PATH_SEGMENTS = 256;
const MAX_IMPORT_DEPTH = 64;
/** The Rust engine's per-file cap (`mds::MAX_FILE_SIZE`), which NativeFs applies to every module it reads. */
const MAX_FILE_SIZE = 10 * 1024 * 1024;
// Match the Rust engine's alias limits before the scanner adds an entry.
const MAX_MODULE_ALIASES = 65_536;
const MAX_MODULE_ALIASES_SIZE = 10 * 1024 * 1024;
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
 * Open a file descriptor with O_NOFOLLOW | O_RDONLY. Map ELOOP to `symlinkError(shown)`.
 * For other open errors, use `lstat` to distinguish the two NativeFs error codes.
 * If `lstat` fails, report `mds::file_not_found` with `shown` as written.
 * If `lstat` succeeds, report `mds::io` with the root-relative `display` and an errno name.
 * Do not use the raw Node error text.
 *
 * The caller's try/catch handles only checks after the open.
 */
async function openNoFollow(
  absolutePath: string,
  shown: string,
  display: string,
): Promise<Awaited<ReturnType<typeof open>>> {
  try {
    return await open(absolutePath, constants.O_RDONLY | O_NOFOLLOW);
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code === 'ELOOP') {
      throw symlinkError(shown);
    }
    throw await lstat(absolutePath).then(
      () => readError(display, err),
      () => fileNotFoundError(shown),
    );
  }
}

/** The smallest a read buffer grows by (Node's own `readFile` chunk). */
const MIN_READ_GROWTH = 64 * 1024;

/**
 * Read `handle` from its start to its end or to `limit` bytes, whichever comes first,
 * into a buffer of at most `limit` bytes: it starts at `sizeHint` plus one byte (room to
 * see the end of a file whose size is known without growing) and grows by at most
 * doubling, capped at `limit` — so a file that grows while it is read is never held
 * whole, and no more than `limit` bytes of it reach the engine (#428), as native's
 * `NativeFs::read` holds no more.
 *
 * Every pass either grows the buffer, which it does at most once per doubling up to
 * `limit`, or reads, and every read that returns a byte brings `length` closer to
 * `limit`; a read that returns none ends it. So the loop is bounded.
 */
async function readAtMost(
  handle: Awaited<ReturnType<typeof open>>,
  limit: number,
  sizeHint: number,
): Promise<Uint8Array> {
  let buffer = Buffer.alloc(Math.min(sizeHint + 1, limit));
  let length = 0;
  while (length < limit) {
    if (length === buffer.length) {
      const grown = Buffer.alloc(Math.min(Math.max(buffer.length * 2, MIN_READ_GROWTH), limit));
      buffer.copy(grown, 0, 0, length);
      buffer = grown;
    }
    const { bytesRead } = await handle.read(buffer, length, buffer.length - length, length);
    if (bytesRead === 0) {
      break;
    }
    length += bytesRead;
  }
  return buffer.subarray(0, length);
}

/**
 * Close `handle`, reporting a failure as the read failure it is (`readError`), never
 * as Node's raw error.
 */
async function closeModule(handle: Awaited<ReturnType<typeof open>>, display: string): Promise<void> {
  await handle.close().catch((err: unknown) => {
    throw readError(display, err);
  });
}

// ---------------------------------------------------------------------------
// Path refusals shared with the Rust engine (#265)
// ---------------------------------------------------------------------------

/** The codes a scanner refusal carries — each the one the Rust engine reports for the same step. */
type PathErrorCode = 'mds::import' | 'mds::io' | 'mds::file_not_found' | 'mds::resource_limit';

/**
 * A path refusal with a Rust engine error code. The differences in `buildModulesMap`
 * explain when the scanner selects a different refusal or uses a different message.
 * A `cannot read` message uses a root-relative path. Its reason can differ.
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

/**
 * What `importError` and `fileNotFoundError` build a refusal's message from — the detail
 * after its prefix, the path as written — for `withImportContext` to rebuild the refusal
 * as the resolver rebuilds it once an import has reached it, never read back out of the
 * message. Kept beside the error, not on it, so a refusal has exactly the fields the
 * engine's error has (#414).
 */
type RefusalParts =
  | { readonly code: 'mds::import'; readonly detail: string }
  | { readonly code: 'mds::file_not_found'; readonly shown: string };

const refusalParts = new WeakMap<Error, RefusalParts>();

// A spanless frontmatter refusal must not gain an outer import's span as it propagates.
const notMdsContextSeen = new WeakSet<Error>();

/** `mds::import`, with the `import error: ` prefix the Rust error's display adds. */
function importError(detail: string): PathError {
  const err = pathError('mds::import', `import error: ${detail}`);
  refusalParts.set(err, { code: 'mds::import', detail });
  return err;
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
  const err = pathError('mds::file_not_found', `file not found: ${escapePathForMessage(shown)}`);
  refusalParts.set(err, { code: 'mds::file_not_found', shown });
  return err;
}

/**
 * `err`, thrown while the import `record` was resolved — by the import itself or by a
 * module below it — with the context the Rust resolver adds at that step
 * (`attach_import_span` and `attach_frontmatter_index`, #414). A spanless
 * `preflightModule` not-MDS refusal gains only its immediate import's span. Otherwise,
 * only a refusal built here (`refusalParts`) that points into no source yet gains any:
 * - through a frontmatter import, `mds::file_not_found` becomes `mds::import`
 *   (`file not found: "<path>" (in frontmatter imports[<i>])`), and an `mds::import`
 *   error not already placed in a frontmatter gets ` (in frontmatter imports[<i>])`;
 * - through `@extends`, `@import` or `@export … from`, `mds::file_not_found` names
 *   the import as written and points at its directive.
 * Every other engine error passes through unchanged.
 */
function withImportContext(err: unknown, record: ImportRecord): unknown {
  if (isMdsError(err) && err.code === 'mds::not_mds' && err.span === undefined && !notMdsContextSeen.has(err)) {
    notMdsContextSeen.add(err);
    if (record.span !== null) {
      return Object.assign(new Error(err.message), err, { span: { ...record.span } });
    }
    return err;
  }
  const parts = isMdsError(err) && err.span === undefined ? refusalParts.get(err) : undefined;
  if (parts === undefined) {
    return err;
  }
  if (record.frontmatterIndex !== null) {
    const where = ` (in frontmatter imports[${record.frontmatterIndex}])`;
    if (parts.code === 'mds::file_not_found') {
      return importError(`file not found: "${escapePathForMessage(parts.shown)}"${where}`);
    }
    return parts.detail.includes('in frontmatter') ? err : importError(`${parts.detail}${where}`);
  }
  if (record.span !== null && parts.code === 'mds::file_not_found') {
    const located = fileNotFoundError(record.path);
    located.span = { ...record.span };
    return located;
  }
  return err;
}

/**
 * Report `mds::io` for a file that resolves but cannot be read. Display its path
 * below the project root, or `.` for the root. For a Node error, use its errno name
 * or `I/O error`, not the raw error text, which can include an absolute path.
 */
function cannotReadError(display: string, reason: string): PathError {
  return pathError(
    'mds::io',
    `cannot read ${escapePathForMessage(display)}: ${escapePathForMessage(reason)}`,
  );
}

/**
 * `cannotReadError` for a module that is not a regular file — a directory, device,
 * FIFO or socket — with the reason NativeFs gives it: `not a regular file` (#428).
 */
function notRegularFileError(display: string): PathError {
  return cannotReadError(display, 'not a regular file');
}

/** `cannotReadError` for a failed Node call, with its errno name as the reason. */
function readError(display: string, err: unknown): PathError {
  const errno = (err as NodeJS.ErrnoException | undefined)?.code;
  return cannotReadError(display, typeof errno === 'string' ? errno : 'I/O error');
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

/** The shared core module-count refusal; the WASM differential pins this literal. */
function moduleCountError(): PathError {
  return resourceLimitError('module count exceeds maximum of 256 modules besides the entry');
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
 * not count, and `..` is kept as written — the OS applies it when the parent is
 * canonicalized: on POSIX after any symlink before it, on Windows lexically, before
 * any link is followed, as it does for NativeFs. `name` is `undefined` when the path
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
 * NativeFs's refusal of a resolved path that is not valid UTF-8 (`key_of`), keyed on
 * `shown`, the path as written: no string names such a path, and the one closest to it
 * names another (#414).
 */
function notUtf8Error(shown: string): PathError {
  return pathError('mds::io', `resolved path is not valid UTF-8: "${escapePathForMessage(shown)}"`);
}

/** What a path resolves to (`resolvePath`). */
type Resolution =
  | { readonly kind: 'resolved'; readonly path: string }
  | { readonly kind: 'unresolved' }
  | { readonly kind: 'not-utf8' };

/**
 * `path` resolved as the OS resolves it (`realpath`), taken in the exact bytes the OS
 * gives. Node's default decoding of them replaces each byte that is not valid UTF-8
 * with U+FFFD, and that string names another path — a "twin" none of the checks here
 * has seen (#414). So bytes that are not valid UTF-8 are reported as such, never
 * decoded: no string names them.
 */
async function resolvePath(path: string): Promise<Resolution> {
  let bytes: Buffer;
  try {
    bytes = await realpath(path, { encoding: 'buffer' });
  } catch {
    return { kind: 'unresolved' };
  }
  return isUtf8(bytes) ? { kind: 'resolved', path: bytes.toString('utf8') } : { kind: 'not-utf8' };
}

/**
 * Canonicalize `path`, translating every failure — it does not exist (ENOENT), a path
 * component above it is a regular file (ENOTDIR), a symlink loop (ELOOP), a name too
 * long (ENAMETOOLONG), a directory that cannot be searched (EACCES) — into the
 * `mds::file_not_found` shape `fileNotFoundError` builds, keyed on `shown`, never
 * Node's error, which names the raw absolute path (R3 / CWE-209, #408). A canonical
 * path that is not valid UTF-8 — a directory reached through a symlink, or the working
 * directory a relative path starts from — is refused (`notUtf8Error`) before anything
 * below it is looked at: every file there would be reached through a string that names
 * another.
 *
 * Mirrors Rust `check_symlink_named`, whose `parent.canonicalize()` step maps
 * every canonicalize failure on the parent — it does not distinguish errno —
 * to `MdsError::file_not_found(shown)`. `realpath` resolves each `..` as the OS does
 * for NativeFs: physically, after the symlinks before it, on POSIX; lexically, before
 * any link is followed, on Windows.
 */
async function canonicalPath(path: string, shown: string): Promise<string> {
  const resolution = await resolvePath(path);
  switch (resolution.kind) {
    case 'resolved':
      return resolution.path;
    case 'unresolved':
      throw fileNotFoundError(shown);
    case 'not-utf8':
      throw notUtf8Error(shown);
    default: {
      const exhaustive: never = resolution;
      throw new Error(`unknown resolution: ${JSON.stringify(exhaustive)}`);
    }
  }
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
 * The two WASM engine calls the scanner makes, the WASM module's own exports (#414):
 * listing a module's imports, and checking a file's bytes and type. What those two check
 * is the Rust engine's own code. The scanner's other checks — of paths, symlinks, the
 * project root and the limits, and the context an import's error gains — are TypeScript
 * mirrors of the native backend's, held to it by the native-vs-WASM differentials.
 */
export type ScannerEngine = Pick<WasmModule, 'scanImportRecords' | 'preflightModule'>;

export interface ModuleScannerOptions {
  /**
   * How many distinct modules besides the entry a scan may admit, including modules
   * still being resolved. Default 256, the engine's own limit.
   */
  maxModules?: number;
  /**
   * The most bytes the entry and its imports may hold together — a guard of the
   * WASM backend's own, with no native counterpart. Default 10 MiB.
   */
  maxAggregateSize?: number;
}

export interface BuildModulesMapResult {
  /** The entry's key in `modules`: its path below the project root, in its on-disk spelling. */
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
  /**
   * Map each lexical alias key to its canonical module key for `moduleAliases`.
   * An alias can name a file other than its lexical key.
   * Safety therefore depends on the walk seeing every import that the engine resolves.
   * A new import form must update `scan_import_records` and the walk together.
   */
  aliases: Record<string, string>;
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

/**
 * The canonical path of `path`, the file `name` in the canonical directory `dir` —
 * NativeFs's `check_symlink_named`, which canonicalizes the joined path once the final
 * component is known not to be a symlink. It is not found when it no longer resolves,
 * and refused when it is not valid UTF-8 (`canonicalPath`); a canonical path in another
 * directory means the component was replaced by a link meanwhile, refused as NativeFs
 * refuses it. On a case-insensitive volume the result carries the file's on-disk
 * spelling (#408).
 */
async function canonicalFile(path: string, dir: string, shown: string): Promise<string> {
  const resolved = await canonicalPath(path, shown);
  if (dirname(resolved) !== dir) {
    throw symlinkError(shown);
  }
  return resolved;
}

/** A module located on disk as NativeFs resolves it, before anything in it is read. */
interface Located {
  /**
   * The module's key in the engine's virtual module map: its canonical path below the
   * project root — its on-disk spelling, as native keys a module by its canonical path.
   */
  readonly key: string;
  /**
   * The key the engine resolves the import to by name — an alias, in
   * `VirtualFs::with_aliases`' terms — before the alias maps it to `key` (#414); the
   * entry's is `key`.
   */
  readonly aliasKey: string;
  /** Its canonical directory joined with its name as written. */
  readonly path: string;
  /** Its canonical directory. */
  readonly dir: string;
  /** Its canonical path. */
  readonly resolved: string;
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

/**
 * The byte budget reads ahead of the walk draw on, counted in the bytes read: a file may
 * change size between its fstat and its read.
 */
interface ReadBudget {
  /** Whether a module whose fstat reports `size` bytes is read; if so, `size` is charged. */
  admit(size: number): boolean;
  /** Charge what a read took beyond the size admitted: negative when the file shrank. */
  charge(bytes: number): void;
}

/** An import of a walked module: located, and read too when read ahead of the walk. */
interface ReadAhead {
  readonly located: Settled<Located>;
  readonly read: Settled<ReadModule> | undefined;
}

/**
 * The outcome of a read-ahead that starts after the walk has already finished, which
 * nothing unwraps: the walk is over. Should anything ever do so, it throws this Error,
 * which says what happened, never `undefined`.
 */
const WALK_FINISHED: Settled<never> = {
  ok: false,
  error: new Error('@mdscript/mds: internal error: an import read ahead after its scan had finished was used'),
};

/**
 * Unwrap `WALK_FINISHED`, as nothing in the scan ever does — for the test that pins what
 * that throws. Intended for use in tests only.
 *
 * @internal
 */
export function _unwrapWalkFinishedForTesting(): never {
  return unwrap(WALK_FINISHED);
}

/**
 * Locate the entry file as NativeFs's `resolve_entry` does, before anything under it
 * is read: the entry path is validated as written (`mds::io`), its segments counted
 * (`mds::resource_limit`), and a path with no final name — `.`, a root, one ending in
 * `..` — is not found. Its parent directory is canonicalized as typed — a relative one
 * from the working directory the OS holds, never from a string of it — so the OS
 * applies each `..` as it does for NativeFs (after the symlinks before it on POSIX,
 * lexically on Windows), never the scanner itself, and it is refused when its canonical
 * path is not valid UTF-8. The final component is refused when missing or a symlink,
 * then canonicalized, and refused when its canonical path carries a forbidden path
 * character.
 */
async function locateEntry(entryPath: string): Promise<{ path: string; dir: string; resolved: string }> {
  const entryErr = entryPathError(entryPath) ?? segmentCountError(entryPath);
  if (entryErr !== undefined) {
    throw entryErr;
  }
  const { parent, name } = nativeParentAndName(entryPath);
  if (name === undefined || name === '..') {
    throw fileNotFoundError(entryPath);
  }
  const dir = await canonicalPath(parent, entryPath);
  const path = join(dir, name);
  await assertFileNotSymlink(path, entryPath);
  const resolved = await canonicalFile(path, dir, entryPath);
  const hostileErr = resolvedPathError(resolved, entryPath);
  if (hostileErr !== undefined) {
    throw hostileErr;
  }
  return { path, dir, resolved };
}

function hasDotDotAfterName(shown: string): boolean {
  let seenName = false;
  for (const part of shown.split(PATH_SEPARATORS)) {
    if (part === '..' && seenName) {
      return true;
    }
    if (part !== '' && part !== '.' && part !== '..') {
      seenName = true;
    }
  }
  return false;
}

function aliasConflictError(shown: string): PathError {
  return importError(
    `two imports name different files by one module key, which the WASM backend cannot resolve: "${escapePathForMessage(shown)}"`,
  );
}

/**
 * Refuse a key that names two files or an alias target beyond the segment cap.
 * An alias can name a file other than its lexical key.
 * Safety therefore depends on the walk seeing every import that the engine resolves.
 * A new import form must update `scan_import_records` and the walk together.
 *
 * @internal
 */
export function aliasRefusal(
  modules: Readonly<Record<string, string>>,
  aliases: ReadonlyMap<string, string>,
  { key, aliasKey }: Pick<Located, 'key' | 'aliasKey'>,
  shown: string,
  dotDotAliases: ReadonlySet<string>,
): PathError | undefined {
  if (aliases.has(key)) {
    return aliasConflictError(shown);
  }
  if (aliasKey === '' || aliasKey === key) {
    return undefined;
  }
  if (Object.hasOwn(modules, aliasKey)) {
    return aliasConflictError(shown);
  }
  const known = aliases.get(aliasKey);
  if (known !== undefined && known !== key) {
    return dotDotAliases.has(aliasKey) || hasDotDotAfterName(shown)
      ? aliasConflictError(shown)
      : symlinkError(shown);
  }
  // The walk checks the module itself before this cap.
  return key.split('/').length > MAX_PATH_SEGMENTS ? segmentCapError(shown) : undefined;
}

/**
 * Locate an import of `importer` within `projectRoot`. See `buildModulesMap` for
 * differences from native resolution. Check the path as written, then check the
 * canonical path. Use the canonical path for the module key, not the lexical alias.
 */
async function locateImport(projectRoot: string, importer: Located, importPath: string): Promise<Located> {
  const stringErr = importPathError(importPath) ?? segmentCountError(importPath);
  if (stringErr !== undefined) {
    throw stringErr;
  }
  const { parent, name } = nativeParentAndName(importPath);
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
  // `./` names the importer's directory. The engine has no key for it.
  const aliasKey = name === undefined ? '' : normalizeVirtualKey(importer.key, importPath);
  // Keep `..` for the OS. Use the same component split for the parent and the name.
  // A final `/.` does not make the file its own parent.
  const parentPath = name === undefined ? dirname(childAbsolute) : importer.dir + sep + parent;
  const dir = await canonicalPath(parentPath, importPath);
  const path = join(dir, name ?? basename(childAbsolute));
  // Security: containment on the canonical path, so a symlinked directory cannot
  // lead outside the project root.
  if (!isWithinRoot(projectRoot, path)) {
    throw escapesProjectError(importPath);
  }
  await assertFileNotSymlink(path, importPath);
  const resolved = await canonicalFile(path, dir, importPath);
  // Security (#265): a directory the import never named, reached through a
  // symlink, can carry a forbidden path character — and so can the on-disk name of
  // a file the import spelled differently.
  const hostileErr = resolvedPathError(resolved, importPath);
  if (hostileErr !== undefined) {
    throw hostileErr;
  }
  const key = name === undefined ? '' : keyOf(projectRoot, resolved);
  return { key, aliasKey, path, dir, resolved };
}

/**
 * Open a located module of the project rooted at `projectRoot` with O_NOFOLLOW and
 * read it, as NativeFs's `read` does — `mds::io` when it cannot be read, `file too
 * large` over the per-file cap — and check it with the `engine`. Given a `budget`, it is
 * read only when the budget admits its size, and `undefined` otherwise; the budget is
 * then charged what the read took beyond that size.
 *
 * O_NOFOLLOW makes open() fail with ELOOP when the final component was swapped for
 * a symlink after it was located, so no link is followed between the check and the
 * read. Where O_NOFOLLOW is unavailable (Windows) open() follows it, and `lstat` —
 * which reports a junction as a symlink too — refuses it before a byte is read.
 * Canonicalizing a non-symlink final component only respells its name; a canonical
 * path other than the one it was located at — the one its key names — means the
 * component was replaced meanwhile, by a link or another file — refused as NativeFs
 * refuses a replaced component. The file type decides, never a comparison of the canonical
 * path with the path as written: on a case-insensitive volume realpath returns the
 * on-disk spelling, so `Entry.mds` for `entry.mds` differs from its canonical form
 * without being a symlink (#408).
 */
async function readModule(
  projectRoot: string,
  engine: ScannerEngine,
  located: Located,
  shown: string,
): Promise<ReadModule>;
async function readModule(
  projectRoot: string,
  engine: ScannerEngine,
  located: Located,
  shown: string,
  budget: ReadBudget,
): Promise<ReadModule | undefined>;
async function readModule(
  projectRoot: string,
  engine: ScannerEngine,
  located: Located,
  shown: string,
  budget?: ReadBudget,
): Promise<ReadModule | undefined> {
  const display = keyOf(projectRoot, located.resolved) || '.';
  // A module that is not a regular file is refused before it is opened, as NativeFs
  // refuses it: opening a FIFO nobody writes to blocks (#428). A symlink is left to
  // the open below, which refuses it without following it.
  const before = await lstat(located.path).catch(() => {
    throw fileNotFoundError(shown);
  });
  if (!before.isFile() && !before.isSymbolicLink()) {
    throw notRegularFileError(display);
  }
  const handle = await openNoFollow(located.path, shown, display);
  try {
    // `openNoFollow` above already succeeded, so the file existed a moment ago; a
    // failure here can only come from a concurrent change (TOCTOU). It is still
    // reported as the engine would report it: `lstat`/`realpath` are NativeFs's
    // stat and canonicalize steps (file-not-found), the fd-based `stat` is part of
    // reading the file.
    const [stats, linkStats, resolution] = await Promise.all([
      handle.stat().catch((err: unknown) => {
        throw readError(display, err);
      }),
      lstat(located.path).catch(() => {
        throw fileNotFoundError(shown);
      }),
      resolvePath(located.path),
    ]);
    if (resolution.kind === 'unresolved') {
      throw fileNotFoundError(shown);
    }
    // A canonical path that is not valid UTF-8 is not the one located, which is.
    if (linkStats.isSymbolicLink() || resolution.kind === 'not-utf8' || resolution.path !== located.resolved) {
      throw symlinkError(shown);
    }
    // Checked again on the opened fd, as NativeFs checks its opened file: another
    // file may have taken the module's place since it was checked above.
    if (!stats.isFile()) {
      throw notRegularFileError(display);
    }
    // Checked on the fstat size too, before a byte is read, so a file over the cap
    // when it is opened is never read into memory; one that grows past it while it
    // is read is read to one byte past the cap and no further (#428), and refused by
    // the engine's own check below.
    if (stats.size > MAX_FILE_SIZE) {
      throw fileTooLargeError(stats.size, display);
    }
    if (budget !== undefined && !budget.admit(stats.size)) {
      return undefined;
    }
    const bytes = await readAtMost(handle, MAX_FILE_SIZE + 1, stats.size).catch((err: unknown) => {
      throw readError(display, err);
    });
    budget?.charge(bytes.length - stats.size);
    // The engine's own checks, in the native order — the per-file cap and UTF-8
    // (#414), then the file type (#417) — decode it: it refuses exactly the files the
    // native backend refuses, and a file that is not an MDS file is refused before
    // any import-like line in it is followed.
    return { resolved: located.resolved, size: bytes.length, content: engine.preflightModule(bytes, display, shown) };
  } finally {
    await closeModule(handle, display);
  }
}

/**
 * Recursively resolve an MDS file and all its imports into a flat modules map
 * suitable for passing to the WASM compile/check functions.
 *
 * The returned `entryFilename` is a **project-root-relative** slash path
 * (e.g. `"src/templates/foo.mds"`), computed via `path.relative(projectRoot,
 * canonicalEntry)`. This mirrors the virtual key used in the `modules` map and
 * is the value that must be passed as the `filename` argument to
 * `build_modules()` / `check()` on the WASM side.
 *
 * Every module is keyed by its canonical path below the project root — its on-disk
 * spelling, as the native backend keys a module by its canonical path — so the engine
 * names it so in `dependencies`, `sourceMap.sources` and a cycle's text, whatever
 * spelling reached it. An import the engine resolves by name to another key — a case
 * variant on a case-insensitive volume, or a path through a symbolically linked
 * directory — is listed in `aliases`, which the engine's `moduleAliases` option maps
 * to the module's key (#414).
 *
 * The checks below mirror the native checks for the same step. See the differences
 * below for exceptions to the error code, message, `help` or order.
 * Path errors use the path as written. File read and byte errors use the path below
 * the project root. Neither uses the resolved absolute path (#265, #414).
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
 *   `symlinks are not allowed in imports: <path as written>`, #408). The scanner
 *   follows links in parent directories and applies the containment checks;
 * - a module outside the project root (discovered via .git/.mdsroot markers), checked
 *   lexically and on the canonical path (`mds::import`, `import path escapes project
 *   directory: "<import as written>"`);
 * - a module whose resolved path carries a forbidden path character — a hostile-named
 *   directory reached through a symlink (`mds::io`) — or is not valid UTF-8 (`mds::io`,
 *   `resolved path is not valid UTF-8: "<path as written>"`): no string names such a
 *   path, and the one closest to it names another, which is never read;
 * - an import chain deeper than 64 modules (`mds::import`); more than `maxModules`
 *   distinct modules besides the entry, including modules still being resolved
 *   (`mds::resource_limit`);
 * - a module that is not a regular file — a directory, device, FIFO or socket —
 *   before the open (`mds::io`, `cannot read <root-relative path>: not a regular
 *   file`, #428), and one that cannot be read (`mds::io`,
 *   `cannot read <root-relative path>: <errno name>`);
 * - a file over the engine's 10 MiB per-file cap (`mds::resource_limit`) or whose
 *   bytes are not valid UTF-8 (`mds::io`), then a file that is neither a `.mds` file
 *   nor a `.md` file declaring `type: mds` (`mds::not_mds`, `not an MDS file: <path as
 *   typed>`, #417) — refused by the engine's own `preflightModule`, the checks the
 *   native backend makes on every file it reads, which also decodes the file (a
 *   leading byte-order mark kept). A file that is not an MDS file is refused before
 *   any import-like line in it is followed.
 *
 * The scanner uses `scanImportRecords` for import context (#414, #423).
 * A missing target of `@import`, `@extends` or `@export … from` gets the directive's
 * `span` if the error has no span. A missing frontmatter import becomes `mds::import`
 * with `file not found: "<path>" (in frontmatter imports[<i>])`.
 * An `mds::import` refusal there gets the same frontmatter suffix.
 *
 * The filesystem root can be a project root. The scanner still checks the lexical
 * key root (difference 3).
 *
 * The scanner can open at most MAX_CONCURRENT_OPENS (16) read-ahead files plus one walk file.
 * It checks at most MAX_IMPORTS_READ_AHEAD imports per module ahead of the walk.
 * It admits up to `maxAggregateSize` bytes ahead of the walk and charges the actual bytes read.
 * A file can grow after admission, but one read holds at most MAX_FILE_SIZE + 1 bytes.
 * The scanner starts at most one read ahead per canonical module key.
 * The walk checks modules depth first, in the order `scanImportRecords` lists their
 * imports for an `@extends` base or for a module in its own right. The first scanner
 * refusal wins. The first error can differ from native as described below.
 * The scan waits for all its reads to stop before it returns or throws.
 *
 * Differences from the native backend that remain (#414):
 * 1. A MISSING module outside the project root — named lexically, or through a
 *    symlinked directory — is refused as escaping it, where native reports it not
 *    found. Deliberate: containment is decided before the file is looked at, so the
 *    scanner is no existence oracle for files outside the project (U-SM30, U-SM32).
 * 2. The aggregate-size guard (`maxAggregateSize`) is the WASM backend's own; native
 *    has none. It judges a module only once native's own checks on it pass, so it
 *    never pre-empts a refusal native makes of that module (U-SM39).
 * 3. The lexical key cannot climb above its root with `..`, even if the resolved
 *    file is inside the project. This includes `/..`, which native resolves to `/`
 *    (#424), and a `link/..` path that climbs above the project root as written (#432).
 *    Deliberate: check lexical containment before disk resolution. To match `/..`,
 *    the engine would need new public API, a spec change, and more WASM bytes
 *    (U-S16, U-SM34, U-SM52).
 * 4. On POSIX, `link/..` can use an alias because the OS applies `..` after the link.
 *    On Windows, both backends apply `..` lexically for disk access. The WASM key
 *    splits only at `/`, so `./link\..\z.mds` and `./a\b/../z.mds` can use aliases too.
 *    The scanner refuses a conflict on either platform. The alias map applies to
 *    every import, so one key cannot name two files. Difference 3 still applies
 *    above the key root (#432; U-SM51 to U-SM55, U-SM58 to U-SM60).
 * 5. WASM caps the entry key, the resolved import key and the canonical alias
 *    target at 256 segments (`mds::resource_limit`). Native caps paths as written.
 *    The alias-target check never pre-empts a refusal of that module itself.
 *    Deliberate: keep an explicit bound on key resolution (U-S16, U-SM56).
 * 6. A `cannot read` reason can be an errno name instead of native OS error text.
 *    Node's error message can include an absolute path (U-SM28, U-SM57).
 *    For a POSIX name with a backslash, native shows `/` and WASM keeps `\` (U-SM62).
 * 7. The scanner reads modules before the engine evaluates them. A later import
 *    refusal can precede an earlier compile error or cycle that native reports.
 *    Deliberate: equal error order would require engine evaluation per module.
 *    This changes the selected error, not the compiled output (U-SM38).
 * 8. The scanner limits distinct aliases to 65,536 entries and 10 MiB of UTF-8 text.
 *    Each entry adds the byte length of its key and target. The scanner refuses an
 *    entry over either limit before it adds the entry (`mds::resource_limit`).
 *    The engine checks the same limits. Native has no alias map (#414; U-SM63, U-SM64).
 * 9. The scanner refuses a directory whose canonical path is not valid UTF-8
 *    before it checks a file below it. Native can report a missing file, a final
 *    symlink, an escape, or a forbidden character first. Deliberate: a string
 *    cannot name that directory exactly. To match native order, each path check
 *    would need a string and a byte form. A check on one form does not prove the
 *    other (U-SM48, Linux CI).
 */
export async function buildModulesMap(
  entryPath: string,
  engine: ScannerEngine,
  options?: ModuleScannerOptions,
): Promise<BuildModulesMapResult> {
  const maxModules = options?.maxModules ?? DEFAULT_MAX_MODULES;
  const maxAggregateSize = options?.maxAggregateSize ?? DEFAULT_MAX_AGGREGATE_SIZE;

  const entry = await locateEntry(entryPath);
  // The filesystem root can be the project root. The lexical key check still applies.
  const projectRoot = findProjectRoot(entry.dir);
  // Virtual keys are always slash-separated to mirror Rust's VirtualFs.
  const entryFilename = keyOf(projectRoot, entry.resolved);

  const modules: Record<string, string> = {};
  /** The lexical key of each alias and its canonical target. */
  const aliases = new Map<string, string>();
  let aliasBytes = 0;
  /** Alias keys whose first import has a `..` segment after a name. */
  const dotDotAliases = new Set<string>();
  /** Keys of modules walked to the end: the native resolver's module cache. */
  const completed = new Set<string>();
  /** Keys of modules being walked: the native resolver's stack of modules resolving. */
  const walking = new Set<string>();
  /**
   * Each key a read-ahead has read, or is reading, and that read's outcome: at most one
   * read-ahead per module, whichever walked module's imports reach it first.
   */
  const readAheadReads = new Map<string, Promise<Settled<ReadModule | undefined>>>();
  const limit = concurrencyLimit(MAX_CONCURRENT_OPENS);
  /** Every read-ahead started and not yet settled. */
  const pending = new Set<Promise<ReadAhead>>();
  let admitted = 0;
  let aggregateSize = 0;
  let readAheadSize = 0;
  let walkFinished = false;

  /**
   * The read-ahead budget: `maxAggregateSize` bytes, each module admitted by its fstat
   * size and charged by the bytes read. Between the two, a read may take more than it
   * was admitted for — up to the per-file cap plus one byte — so at most
   * MAX_CONCURRENT_OPENS reads of MAX_FILE_SIZE + 1 bytes each are held beyond the
   * budget. Once the walk has finished, a read-ahead still under way reads nothing more.
   */
  const readAheadBudget: ReadBudget = {
    admit(size) {
      if (walkFinished || readAheadSize + size > maxAggregateSize) {
        return false;
      }
      readAheadSize += size;
      return true;
    },
    charge(bytes) {
      readAheadSize += bytes;
    },
  };

  /**
   * Locate an import and, when its module is new and the read-ahead budget allows,
   * read it — ahead of the walk, which checks the outcome in the native order. Never
   * rejects: every failure is settled for the walk to report when it gets there.
   */
  async function readAhead(importer: Located, importPath: string): Promise<ReadAhead> {
    if (walkFinished) {
      return { located: WALK_FINISHED, read: undefined };
    }
    const located = await settle(locateImport(projectRoot, importer, importPath));
    if (!located.ok || walkFinished) {
      return { located, read: undefined };
    }
    const { key } = located.value;
    if (completed.has(key) || walking.has(key) || readAheadReads.has(key)) {
      return { located, read: undefined };
    }
    const reading = settle(readModule(projectRoot, engine, located.value, importPath, readAheadBudget));
    readAheadReads.set(key, reading);
    const read = await reading;
    if (!read.ok) {
      return { located, read };
    }
    // A module the read-ahead budget left unread is read by the walk itself.
    return { located, read: read.value === undefined ? undefined : { ok: true, value: read.value } };
  }

  /**
   * Start the read-ahead of the import `importPath` of `importer`, kept in `pending`
   * until it settles.
   */
  function startReadAhead(importer: Located, importPath: string): Promise<ReadAhead> {
    const ahead = limit(() => readAhead(importer, importPath));
    pending.add(ahead);
    const forget = (): void => {
      pending.delete(ahead);
    };
    ahead.then(forget, forget);
    return ahead;
  }

  /**
   * The module `key` as a read ahead of the walk read it, when one did — reached through
   * another module's imports. A read that failed, or that the budget declined, is left
   * for the walk to make itself, so its error names the import the walk reached it by.
   */
  async function readAheadRead(key: string): Promise<ReadModule | undefined> {
    const read = await readAheadReads.get(key);
    return read?.ok === true ? read.value : undefined;
  }

  /** Check the alias before the engine can use it for any import. */
  function checkAndRecordAlias(located: Located, shown: string): void {
    const refusal = aliasRefusal(modules, aliases, located, shown, dotDotAliases);
    if (refusal !== undefined) {
      throw refusal;
    }
    const { key, aliasKey } = located;
    if (aliasKey === '' || aliasKey === key || aliases.has(aliasKey)) {
      return;
    }
    const count = aliases.size + 1;
    if (count > MAX_MODULE_ALIASES) {
      throw pathError(
        'mds::resource_limit',
        `options.moduleAliases exceeds maximum alias count of ${MAX_MODULE_ALIASES} (${count} provided)`,
      );
    }
    const bytes = aliasBytes + Buffer.byteLength(aliasKey) + Buffer.byteLength(key);
    if (bytes > MAX_MODULE_ALIASES_SIZE) {
      throw pathError(
        'mds::resource_limit',
        `options.moduleAliases aggregate size exceeds maximum of ${MAX_MODULE_ALIASES_SIZE} bytes`,
      );
    }
    aliasBytes = bytes;
    aliases.set(aliasKey, key);
    if (hasDotDotAfterName(shown)) {
      dotDotAliases.add(aliasKey);
    }
  }

  /**
   * Admit a distinct module `depth` imports below the entry to the walk. Check the
   * import depth, then the count, as native does. `admitted` includes the entry and
   * every module still being resolved; permit the entry plus `maxModules` others.
   */
  function admit(depth: number): void {
    if (depth >= MAX_IMPORT_DEPTH) {
      throw importDepthError();
    }
    if (admitted > maxModules) {
      throw moduleCountError();
    }
    admitted += 1;
  }

  /**
   * Walk the located module, reached as `shown` `depth` imports below the entry, then
   * its imports in the order the native resolver resolves them — its `resolve_by_key`:
   * a module already resolved, or still resolving (a cycle, the engine's to report), is
   * not read again, and the module is admitted (`admit`) before the file is read. The
   * engine lists its imports in the order the resolver resolves them for a module
   * reached as the `@extends` base of another (`asBase`) or for itself. An error
   * resolving an import is reported with the context the resolver adds to it
   * (`withImportContext`).
   */
  async function walk(
    located: Located,
    shown: string,
    depth: number,
    readAheadOutcome: Settled<ReadModule> | undefined,
    asBase: boolean,
  ): Promise<void> {
    const { key } = located;
    if (completed.has(key) || walking.has(key)) {
      checkAndRecordAlias(located, shown);
      return;
    }
    admit(depth);

    // A module its own import's read-ahead did not read is taken from the read-ahead of
    // another import of it, or read here, whatever its size: the per-file cap bounds it.
    const read = readAheadOutcome === undefined
      ? (await readAheadRead(key)) ?? (await readModule(projectRoot, engine, located, shown))
      : unwrap(readAheadOutcome);
    // The WASM backend's own guard, checked in walk order once the module has passed
    // every check native makes on it — the per-file cap, UTF-8 and the file type — so
    // it never reports a module native refuses, whichever reads the read-ahead budget
    // let run ahead of the walk (difference 2 in the list above).
    aggregateSize += read.size;
    if (aggregateSize > maxAggregateSize) {
      throw resourceLimitError(`aggregate module size exceeds maximum of ${maxAggregateSize} bytes`);
    }
    checkAndRecordAlias(located, shown);
    modules[key] = read.content;

    const records = engine.scanImportRecords(read.content, asBase);
    walking.add(key);
    // The next MAX_IMPORTS_READ_AHEAD imports are read ahead of the walk; each one
    // walked starts the next.
    const imports = slidingWindow(records, MAX_IMPORTS_READ_AHEAD, (record) =>
      startReadAhead(located, record.path),
    );
    for (const [record, ahead] of imports) {
      const { located: child, read: childRead } = await ahead;
      try {
        await walk(unwrap(child), record.path, depth + 1, childRead, record.kind === 'extends');
      } catch (err) {
        throw withImportContext(err, record);
      }
    }
    walking.delete(key);
    completed.add(key);
  }

  try {
    const entryLocated = { key: entryFilename, aliasKey: entryFilename, ...entry };
    await walk(entryLocated, entryPath, 0, undefined, false);
  } finally {
    // A walk that throws leaves read-aheads under way, holding open files and about to
    // call into the engine. Each stops at its next step and is waited for, so none
    // outlives this call; none rejects, so the walk's own error is the one thrown.
    walkFinished = true;
    await Promise.allSettled(pending);
  }

  return { entryFilename, modules, aliases: Object.fromEntries(aliases) };
}
