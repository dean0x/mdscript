import { existsSync } from 'node:fs';
import { resolve, relative, dirname, isAbsolute, sep } from 'node:path';

// DUPLICATED deliberately from @mdscript/mds `src/util/module-scanner.ts`
// (findProjectRoot): a static import of ESM-only @mdscript/mds from this
// package breaks the CJS build (dist-cjs) with ERR_REQUIRE_ESM, and threading
// a root through every plugin was rejected — Vite's `config.root` is the app
// root, not the project root, and the other bundlers have no equivalent.
// Keep the walk semantics in sync with module-scanner.ts (and with the Rust
// NativeFs::find_project_root): same markers, same bounded traversal.
const PROJECT_ROOT_MARKERS = ['.git', '.mdsroot'] as const;
const MAX_TRAVERSAL_DEPTH = 256;

// Windows drive-qualified prefix (`C:`, `C:/`, `C:\` — after separator
// unification only the first two forms remain).
const DRIVE_QUALIFIED_RE = /^[A-Za-z]:($|\/)/;

// Windows verbatim (extended-length) prefixes as produced by
// `std::fs::canonicalize` on Windows — `\\?\C:\...` (drive) and
// `\\?\UNC\server\share\...` (UNC). Forward-slash variants (`//?/...`) are
// accepted too, mirroring core's strip, which runs after separator unification.
const VERBATIM_UNC_PREFIX_RE = /^[\\/][\\/]\?[\\/]UNC[\\/]/i;
const VERBATIM_PREFIX_RE = /^[\\/][\\/]\?[\\/]/;

/**
 * Strip the Windows verbatim (extended-length) prefix from a path, if present.
 *
 * The native backend reports dependencies as canonical paths; on Windows,
 * `std::fs::canonicalize` returns them in verbatim form (`\\?\D:\...`).
 * `path.relative` cannot bridge a verbatim path and a non-verbatim root (no
 * common component — it returns the absolute `to` path), so without this strip
 * every native dependency degrades to its basename in the emitted metadata and
 * the functional watch dependency diverges from the WASM backend's form.
 *
 * Mirrors `path_to_unified` in `crates/mds-core/src/source_path.rs`. One
 * deliberate divergence: core drops the `\\?\UNC\` prefix entirely (it builds
 * component lists), while this rewrites it to `\\` so the result remains a
 * functional absolute UNC path for the watch sink.
 */
export function stripWindowsVerbatimPrefix(p: string): string {
  if (VERBATIM_UNC_PREFIX_RE.test(p)) {
    return `\\\\${p.slice('\\\\?\\UNC\\'.length)}`;
  }
  if (VERBATIM_PREFIX_RE.test(p)) {
    return p.slice('\\\\?\\'.length);
  }
  return p;
}

/**
 * Cache from start-directory → project root. The project root is invariant
 * within a single build, so repeated calls from the same directory (one per
 * transform) skip the bounded synchronous traversal entirely.
 */
const projectRootCache = new Map<string, string>();

/**
 * Walk up from a directory to find the project root (`.git` / `.mdsroot`
 * marker), falling back to the start directory when no marker is found within
 * MAX_TRAVERSAL_DEPTH parents.
 *
 * ARCHITECTURE EXCEPTION: synchronous `existsSync` in an otherwise-async
 * package — the result is cached per start directory, so the bounded blocking
 * traversal runs at most once per unique directory per process (same
 * trade-off as module-scanner.ts in @mdscript/mds).
 */
export function findProjectRoot(start: string): string {
  const normalized = resolve(start);
  const cached = projectRootCache.get(normalized);
  if (cached !== undefined) {
    return cached;
  }

  let dir = normalized;
  let result = normalized;
  for (let i = 0; i < MAX_TRAVERSAL_DEPTH; i++) {
    if (PROJECT_ROOT_MARKERS.some((marker) => existsSync(resolve(dir, marker)))) {
      result = dir;
      break;
    }
    const parent = dirname(dir);
    if (parent === dir) {
      break;
    }
    dir = parent;
  }
  projectRootCache.set(normalized, result);
  return result;
}

/**
 * Resolve a compiler-reported dependency to an absolute path.
 *
 * The native backend emits absolute canonical paths (passed through after
 * stripping any Windows verbatim `\\?\` prefix, so both backends agree on the
 * same non-verbatim form); the WASM backend emits project-root-relative POSIX
 * paths, which are resolved against `root`. TransformResult.dependencies is a
 * FUNCTIONAL watch input — bundlers resolve relative paths against cwd in
 * `addWatchFile`/`addDependency` — so it must always carry absolute paths.
 */
export function toAbsoluteDependency(root: string, dep: string): string {
  const stripped = stripWindowsVerbatimPrefix(dep);
  return isAbsolute(stripped) ? stripped : resolve(root, stripped);
}

/**
 * Convert an absolute dependency path to a project-root-relative POSIX path
 * for the emitted `metadata` literal (which lands in production bundles —
 * absolute host paths there are an information leak).
 *
 * Semantic source of truth: `relativize_source` in
 * `crates/mds-core/src/source_path.rs` — this is its reduced TS mirror for
 * the metadata sink (inputs are compiler-produced filesystem paths, not
 * arbitrary hostile strings, so only the guards reachable here are mirrored):
 * separators are unified to `/`, and a path that escapes the root, stays
 * absolute, or is drive-qualified degrades to its basename (never `../`,
 * never a host prefix). Ultimate fallback is `"source"`, as in core.
 */
export function toRootRelativePosix(root: string, absPath: string): string {
  // Strip verbatim prefixes first (core's step 3 / path_to_unified), then
  // unify separators (core's step 2): a backslash-separated segment must be
  // visible to the escape/drive guards below. Same deliberate trade-off as
  // relativize_source — a literal `\` in a POSIX filename becomes `/`.
  const rel = relative(stripWindowsVerbatimPrefix(root), stripWindowsVerbatimPrefix(absPath))
    .split('\\')
    .join('/')
    .split(sep)
    .join('/');
  if (rel === '' || rel === '..' || rel.startsWith('../') || rel.startsWith('/') || DRIVE_QUALIFIED_RE.test(rel)) {
    return basenameFallback(absPath);
  }
  return rel;
}

/** Last non-`.`/`..` component of the separator-unified path; `"source"` when none. */
function basenameFallback(p: string): string {
  const comps = p
    .split('\\')
    .join('/')
    .split('/')
    .filter((c) => c !== '' && c !== '.' && c !== '..');
  const last = comps[comps.length - 1];
  return last !== undefined && last !== '' ? last : 'source';
}
