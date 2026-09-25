import { lstat, open, realpath } from 'node:fs/promises';
import { constants, existsSync } from 'node:fs';
import { resolve, dirname, basename, join, relative, isAbsolute, sep } from 'node:path';
import {
  escapePathForMessage,
  firstForbiddenChar,
  forbiddenCharMessage,
  importPathViolation,
} from './path-chars.js';

// O_NOFOLLOW prevents the kernel from following a symlink at the final path
// component. Using it closes the TOCTOU window between lstat and open.
// On Windows, O_NOFOLLOW is not defined; fall back to 0 (no-op flag) and
// rely on a post-open lstat check instead.
const O_NOFOLLOW: number = (constants as Record<string, number>)['O_NOFOLLOW'] ?? 0;

const MAX_PATH_SEGMENTS = 256;
const MAX_IMPORT_DEPTH = 64;
const MAX_TRAVERSAL_DEPTH = 256;
// Maximum concurrent file opens per fan-out level. Keeps file descriptor usage
// predictable even when a module imports many siblings at once.
const MAX_CONCURRENT_OPENS = 16;
const PROJECT_ROOT_MARKERS = ['.git', '.mdsroot'] as const;
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
 * Module-level helper (not a closure) so that openAndValidateModule's own
 * try/catch only handles post-open validation, keeping nesting shallow.
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

/**
 * A path refusal carrying the code the Rust engine reports for the same input.
 * Each message is byte-identical to that engine error's message as the native
 * backend throws it, so the WASM backend's file operations fail exactly like
 * the native ones.
 */
type PathError = Error & { code: 'mds::import' | 'mds::io' | 'mds::file_not_found' };

function pathError(code: PathError['code'], message: string): PathError {
  const err = new Error(message) as PathError;
  err.code = code;
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

/**
 * Canonicalize `path`'s parent directory, translating every failure — the parent
 * does not exist (ENOENT), a path component above it is a regular file (ENOTDIR),
 * a symlink loop (ELOOP), a name too long (ENAMETOOLONG), a directory that cannot
 * be searched (EACCES) — into the `mds::file_not_found` shape `fileNotFoundError`
 * builds, keyed on `shown`, never Node's error, which names the raw absolute path
 * (R3 / CWE-209, #408).
 *
 * Mirrors Rust `check_symlink_named`, whose `parent.canonicalize()` step maps
 * every canonicalize failure on the parent — it does not distinguish errno —
 * to `MdsError::file_not_found(shown)`.
 *
 * Shared by buildModulesMap's own entry-directory canonicalization and
 * openAndValidateModule's per-module one, so the translation is written once.
 */
async function realpathParent(path: string, shown: string): Promise<string> {
  try {
    return await realpath(dirname(path));
  } catch {
    throw fileNotFoundError(shown);
  }
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

export interface ModuleScannerOptions {
  maxModules?: number;
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
 * An entry key that is empty, contains NUL or carries a forbidden path character
 * (#265) is refused with `mds::io`; an import with the same defects with
 * `mds::import` — each with the message the Rust backend reports.
 *
 * MUST exactly mirror the Rust implementation to ensure import resolution matches.
 */
export function normalizeVirtualKey(base: string, relative: string): string {
  if (base.length === 0) {
    const entryErr = entryPathError(relative);
    if (entryErr !== undefined) {
      throw entryErr;
    }
    // Root entry point — use key as-is, but still enforce the segment limit.
    const segmentCount = relative.split('/').filter((s) => s.length > 0 && s !== '.').length;
    if (segmentCount > MAX_PATH_SEGMENTS) {
      throw new Error(`import path exceeds maximum segment count of ${MAX_PATH_SEGMENTS}`);
    }
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
        throw new Error('import path escapes project directory');
      }
      segments.pop();
    } else {
      if (segments.length >= MAX_PATH_SEGMENTS) {
        throw new Error(`import path exceeds maximum segment count of ${MAX_PATH_SEGMENTS}`);
      }
      segments.push(part);
    }
  }

  if (segments.length === 0) {
    throw new Error('import path resolves to empty key');
  }

  return segments.join('/');
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
 * Security checks performed:
 * - Rejects a module whose final path component is a symlink, judged by that
 *   component's own file type (O_NOFOLLOW open; `lstat` where O_NOFOLLOW is
 *   unavailable), with NativeFs's refusal (`mds::import`, `symlinks are not allowed
 *   in imports: <path as written>`). Symlinked parent directories are followed, as
 *   NativeFs does
 * - Rejects an import that leaves a symlinked directory through `..`
 *   (`mds::import`): the engine resolves it by name to another file than the
 *   one NativeFs reads (#408)
 * - Rejects paths that escape the project root (discovered via .git/.mdsroot
 *   markers), checked on the canonical path, with NativeFs's refusal
 *   (`mds::import`, `import path escapes project directory: "<import as written>"`)
 * - Rejects a module that is not a regular file — a directory, device, FIFO or
 *   socket — as unreadable (`mds::io`, `cannot read <path as written>: …`)
 * - Rejects, before the filesystem is touched and with the Rust engine's code and
 *   message: an entry path that is empty, contains NUL or carries a forbidden path
 *   character (`mds::io`), and an import string that is not `./`/`../`-relative,
 *   contains NUL or carries a forbidden path character (`mds::import`) (#265)
 * - Rejects a module whose resolved path carries a forbidden path character — a
 *   hostile-named directory reached through a symlink (`mds::io`, #265)
 * - Reports a filesystem failure with the code the Rust engine gives the same
 *   step, never Node's raw error, which names the resolved absolute path (#408):
 *   a module or directory that cannot be resolved — missing, blocked by a
 *   non-directory path component, a symlink loop, a name too long, a directory
 *   that cannot be searched — is `mds::file_not_found`; a file that resolves but
 *   cannot be read is `mds::io` (`cannot read <path as written>: <errno name>`)
 * - Enforces module count and aggregate size limits
 */
export async function buildModulesMap(
  entryPath: string,
  scanImports: (source: string) => string[],
  options?: ModuleScannerOptions,
): Promise<BuildModulesMapResult> {
  const maxModules = options?.maxModules ?? DEFAULT_MAX_MODULES;
  const maxAggregateSize = options?.maxAggregateSize ?? DEFAULT_MAX_AGGREGATE_SIZE;

  const entryErr = entryPathError(entryPath);
  if (entryErr !== undefined) {
    throw entryErr;
  }

  // Resolve the parent directory to its canonical form before computing the
  // project root and security boundaries, so an OS-level directory symlink
  // (e.g. macOS /var → /private/var) does not move the root.
  //
  // Only the PARENT directory is canonicalized, NOT the final path component,
  // which openAndValidateModule judges by its own file type — the pattern of
  // NativeFs::check_symlink_named (Rust).
  //
  // A parent directory that does not exist (ENOENT) or is not traversable
  // (ENOTDIR — a path component above it is a regular file, #408) is reported
  // as file-not-found, keyed on entryPath as written, rather than leaking this
  // raw, resolved realpath() error.
  const rawAbsoluteEntry = resolve(entryPath);
  const canonicalParentDir = await realpathParent(rawAbsoluteEntry, entryPath);
  const absoluteEntry = canonicalParentDir + sep + rawAbsoluteEntry.slice(rawAbsoluteEntry.lastIndexOf(sep) + 1);
  const projectRoot = findProjectRoot(dirname(absoluteEntry));
  // Virtual keys are always slash-separated to mirror Rust's VirtualFs; on
  // Windows `relative` yields backslashes, so normalize to '/'.
  const entryFilename = relative(projectRoot, absoluteEntry).split(sep).join('/');

  // Security: entry file must not be at filesystem root — that would disable the
  // path traversal guard (a root project dir makes containment checks meaningless).
  // `dirname(root) === root` is true exactly at a filesystem root on every
  // platform ('/', 'C:\\', '\\\\server\\share\\').
  if (projectRoot === '' || dirname(projectRoot) === projectRoot) {
    throw new Error('security: project root cannot be filesystem root');
  }

  const modules: Record<string, string> = {};
  const visited = new Set<string>();
  let aggregateSize = 0;

  /**
   * Validate a child import path string and resolve it to an absolute filesystem
   * path within the project root. Returns the resolved absolute path.
   */
  function validateImportPath(importPath: string, absoluteDir: string): string {
    // Security: classify the import string exactly as the Rust resolver will, so
    // it is refused before the filesystem is touched and with the error the
    // native backend throws for the same input.
    const importErr = importPathError(importPath);
    if (importErr !== undefined) {
      throw importErr;
    }

    const childAbsolute = resolve(absoluteDir, importPath);

    // Security: verify child is within project root — lexically, before anything
    // outside the root is touched. NativeFs resolves the file first, so for a
    // MISSING file outside the root it reports `file not found` where this refuses.
    if (!isWithinRoot(projectRoot, childAbsolute)) {
      throw escapesProjectError(importPath);
    }

    return childAbsolute;
  }

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
   * Open a file with O_NOFOLLOW and validate its security properties (symlink check,
   * path confinement, regular-file check). Returns the open file handle, the
   * file's byte size from fstat, and its canonical path.
   *
   * The caller is responsible for closing the handle (use try/finally).
   * Separating open+validate from read allows the aggregate size check to happen
   * before file content is loaded into memory, bounding worst-case memory use.
   *
   * Mirrors NativeFs::check_symlink_named (Rust): the parent directory is
   * canonicalized (its symlinks followed), the final component is joined as
   * written, and that component is refused when its own file type is a symlink.
   * The file type decides, never a comparison of the canonical path with the
   * path as written: on a case-insensitive volume (the macOS and Windows
   * default) realpath returns the on-disk spelling, so `Entry.mds` for
   * `entry.mds` differs from its canonical form without being a symlink (#408).
   *
   * O_NOFOLLOW makes open() fail with ELOOP on a symlinked final component, so no
   * link is followed between the check and the read. Where O_NOFOLLOW is
   * unavailable (Windows) open() follows it, and `lstat` — which reports a
   * junction as a symlink too — refuses it before a byte is read.
   *
   * `shown` is the path as written — the entry path the caller passed, or the
   * import string — and is what a refusal of the resolved path names.
   */
  async function openAndValidateModule(
    absolutePath: string,
    shown: string,
  ): Promise<{ handle: Awaited<ReturnType<typeof open>>; size: number; resolved: string }> {
    // See realpathParent: an import whose directory does not exist (or is not
    // traversable) is reported as file-not-found, keyed on `shown`.
    const canonicalParent = await realpathParent(absolutePath, shown);
    const joined = join(canonicalParent, basename(absolutePath));

    // Security (#265): no forbidden path character anywhere in the canonical
    // path — a directory the caller never named, reached through a symlink, can
    // carry one. Checked before anything under it is opened.
    const hostileErr = resolvedPathError(joined, shown);
    if (hostileErr !== undefined) {
      throw hostileErr;
    }

    // Security: containment is decided on the canonical path, so a symlinked
    // directory cannot lead outside the project root.
    if (!isWithinRoot(projectRoot, joined)) {
      throw escapesProjectError(shown);
    }

    // O_NOFOLLOW | O_RDONLY: if the final component is a symlink the kernel
    // rejects it with ELOOP before our code reads a single byte.
    const handle = await openNoFollow(joined, shown);

    try {
      // `openNoFollow` above already succeeded, so `joined` names a file that
      // existed a moment ago; a failure here can only come from a concurrent
      // change (TOCTOU). It is still reported as the engine would report it:
      // `lstat`/`realpath` are NativeFs's stat and canonicalize steps
      // (file-not-found), the fd-based `stat` is part of reading the file.
      const [stats, linkStats, resolved] = await Promise.all([
        handle.stat().catch((err: unknown) => {
          throw readError(shown, err);
        }),
        lstat(joined).catch(() => {
          throw fileNotFoundError(shown);
        }),
        realpath(joined).catch(() => {
          throw fileNotFoundError(shown);
        }),
      ]);

      // The final component's own file type — the check that stands in for
      // O_NOFOLLOW where the platform lacks it.
      if (linkStats.isSymbolicLink()) {
        throw symlinkError(shown);
      }

      // fstat on the opened fd: verify it is a regular file (not a device,
      // directory, socket, etc.). NativeFs fails such a module at its read step
      // (`mds::io`); a directory is named by the errno reading it reports.
      if (!stats.isFile()) {
        throw cannotReadError(shown, stats.isDirectory() ? 'EISDIR' : 'not a regular file');
      }

      // Canonicalizing a non-symlink final component only respells its name;
      // it never changes the directory. A canonical path in another directory
      // means the component was replaced by a link after the checks above —
      // refused as NativeFs refuses it, as a symlink.
      if (dirname(resolved) !== canonicalParent) {
        throw symlinkError(shown);
      }

      return { handle, size: stats.size, resolved };
    } catch (err) {
      await closeModule(handle, shown);
      throw err;
    }
  }

  async function scan(
    absolutePath: string,
    virtualKey: string,
    shown: string,
    depth: number = 0,
  ): Promise<void> {
    // Reliability: bound recursion depth explicitly — maxModules limits total
    // nodes but not stack frames; a linear chain of 256 imports would create
    // 256 frames without this guard.
    if (depth > MAX_IMPORT_DEPTH) {
      throw new Error(
        `resource limit: import chain depth exceeds maximum of ${MAX_IMPORT_DEPTH}`,
      );
    }

    // Keyed by virtual key, not by path: through a symlinked directory one file
    // can sit under two keys, and the engine looks up each of them (#408).
    if (visited.has(virtualKey)) {
      return;
    }
    visited.add(virtualKey);

    // Resource limit: check module count immediately after marking visited so
    // the count is O(1) and there is no off-by-one from checking after the write.
    if (visited.size > maxModules) {
      throw new Error(
        `resource limit: module count exceeds maximum of ${maxModules}`,
      );
    }

    const { handle, size: fileSize, resolved } = await openAndValidateModule(absolutePath, shown);

    let content: string;
    try {
      // Resource limit: check aggregate size using fstat metadata BEFORE reading
      // content into memory, so that a malicious file cannot force allocation of
      // content it knows will be rejected.
      // JS is single-threaded: the increment and guard below execute atomically
      // (no await between them), so concurrent scan() calls cannot interleave here.
      aggregateSize += fileSize;
      if (aggregateSize > maxAggregateSize) {
        throw new Error(
          `resource limit: aggregate module size exceeds maximum of ${maxAggregateSize} bytes`,
        );
      }

      content = await handle.readFile({ encoding: 'utf-8' }).catch((err: unknown) => {
        throw readError(shown, err);
      });
    } finally {
      await closeModule(handle, shown);
    }

    modules[virtualKey] = content;

    const importPaths = scanImports(content);
    // Imports resolve from the module's canonical directory, as NativeFs resolves
    // them from its canonical key.
    const absoluteDir = dirname(resolved);

    // Bounded-concurrency fan-out: limit simultaneous child opens to
    // MAX_CONCURRENT_OPENS to avoid exhausting file descriptors on modules
    // with many siblings. Each slot runs the next import as soon as it
    // finishes, so throughput is preserved for the common case.
    const queue = importPaths.slice();
    async function worker(): Promise<void> {
      let importPath: string | undefined;
      while ((importPath = queue.shift()) !== undefined) {
        const childAbsolute = validateImportPath(importPath, absoluteDir);
        // Compute virtual key using normalizeVirtualKey to mirror Rust's VirtualFs::normalize_in_dir().
        const childVirtualKey = normalizeVirtualKey(virtualKey, importPath);
        await assertKeyMatchesDisk(absoluteDir, importPath, childVirtualKey);
        await scan(childAbsolute, childVirtualKey, importPath, depth + 1);
      }
    }
    const slots = Math.min(MAX_CONCURRENT_OPENS, importPaths.length);
    await Promise.all(Array.from({ length: slots }, worker));
  }

  await scan(absoluteEntry, entryFilename, entryPath);

  return { entryFilename, modules };
}
