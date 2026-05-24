import { open, realpath } from 'node:fs/promises';
import { constants } from 'node:fs';
import { resolve, dirname, basename } from 'node:path';

// O_NOFOLLOW prevents the kernel from following a symlink at the final path
// component. Using it closes the TOCTOU window between lstat and open.
// On Windows, O_NOFOLLOW is not defined; fall back to 0 (no-op flag) and
// rely on a post-open lstat check instead.
const O_NOFOLLOW: number = (constants as Record<string, number>)['O_NOFOLLOW'] ?? 0;

const MAX_PATH_SEGMENTS = 256;
const MAX_IMPORT_DEPTH = 64;
export const DEFAULT_MAX_MODULES = 256;
export const DEFAULT_MAX_AGGREGATE_SIZE = 10 * 1024 * 1024; // 10 MiB

/**
 * Open a file descriptor with O_NOFOLLOW | O_RDONLY, translating the ELOOP /
 * ENOTDIR errors that the kernel emits when the path is a symlink into a clear
 * security error. All other OS errors are re-thrown unchanged.
 *
 * Module-level helper (not a closure) so that openAndValidateModule's own
 * try/catch only handles post-open validation, keeping nesting shallow.
 */
async function openNoFollow(absolutePath: string): Promise<Awaited<ReturnType<typeof open>>> {
  try {
    return await open(absolutePath, constants.O_RDONLY | O_NOFOLLOW);
  } catch (err) {
    const code = (err as NodeJS.ErrnoException).code;
    if (code === 'ELOOP' || code === 'ENOTDIR') {
      throw new Error(`security: symlink detected at ${absolutePath} — symlinks are not allowed`);
    }
    throw err;
  }
}

export interface ModuleScannerOptions {
  maxModules?: number;
  maxAggregateSize?: number;
}

export interface BuildModulesMapResult {
  entryFilename: string;
  modules: Record<string, string>;
}

/**
 * Normalize a virtual module key the same way VirtualFs::normalize() does in Rust.
 *
 * Given a base key (the key of the importing module) and a relative import path,
 * resolve the import path to a canonical slash-separated key.
 *
 * MUST exactly mirror the Rust implementation to ensure import resolution matches.
 */
export function normalizeVirtualKey(base: string, relative: string): string {
  if (relative.length === 0) {
    throw new Error('import path is empty');
  }
  if (relative.includes('\0')) {
    throw new Error('import path contains null byte');
  }

  if (base.length === 0) {
    // Root entry point — use key as-is, but still enforce the segment limit.
    const segmentCount = relative.split('/').filter((s) => s.length > 0 && s !== '.').length;
    if (segmentCount > MAX_PATH_SEGMENTS) {
      throw new Error(`import path exceeds maximum segment count of ${MAX_PATH_SEGMENTS}`);
    }
    return relative;
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
 * Security checks performed:
 * - Rejects symlinks (O_NOFOLLOW open; realpath check on Windows fallback)
 * - Rejects paths that escape the project root (entry file's directory)
 * - Rejects paths with null bytes or empty segments
 * - Enforces module count and aggregate size limits
 */
export async function buildModulesMap(
  entryPath: string,
  scanImports: (source: string) => string[],
  options?: ModuleScannerOptions,
): Promise<BuildModulesMapResult> {
  const maxModules = options?.maxModules ?? DEFAULT_MAX_MODULES;
  const maxAggregateSize = options?.maxAggregateSize ?? DEFAULT_MAX_AGGREGATE_SIZE;

  const absoluteEntry = resolve(entryPath);
  const projectRoot = dirname(absoluteEntry);
  const entryFilename = basename(absoluteEntry);

  // Security: entry file must not be at filesystem root — that would disable the
  // path traversal guard (projectRoot === '/' makes startsWith checks meaningless).
  if (projectRoot === '/' || projectRoot === '') {
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
    // Security: reject null bytes and empty paths.
    if (importPath.includes('\0')) {
      throw new Error('security: import path contains null byte');
    }
    if (importPath.trim().length === 0) {
      throw new Error('security: import path is empty');
    }

    const childAbsolute = resolve(absoluteDir, importPath);

    // Security: verify child is within project root.
    if (!childAbsolute.startsWith(projectRoot + '/') && childAbsolute !== projectRoot) {
      throw new Error(
        `security: import path escapes project root: ${childAbsolute} is outside ${projectRoot}`,
      );
    }

    return childAbsolute;
  }

  /**
   * Open a file with O_NOFOLLOW and validate its security properties (symlink check,
   * path confinement, regular-file check). Returns the open file handle and the
   * file's byte size from fstat.
   *
   * The caller is responsible for closing the handle (use try/finally).
   * Separating open+validate from read allows the aggregate size check to happen
   * before file content is loaded into memory, bounding worst-case memory use.
   *
   * Uses O_NOFOLLOW to eliminate the TOCTOU race window between validation and
   * content access. If the path is a symlink, O_NOFOLLOW causes open() to fail
   * with ELOOP, which we surface as a security error. On Windows (where O_NOFOLLOW=0),
   * a post-open realpath check is performed instead.
   */
  async function openAndValidateModule(
    absolutePath: string,
  ): Promise<{ handle: Awaited<ReturnType<typeof open>>; size: number }> {
    // Security: verify path is within project root before opening.
    if (!absolutePath.startsWith(projectRoot + '/') && absolutePath !== projectRoot) {
      throw new Error(
        `security: path escapes project root: ${absolutePath} is outside ${projectRoot}`,
      );
    }

    // O_NOFOLLOW | O_RDONLY: if absolutePath is a symlink the kernel rejects it
    // with ELOOP before our code reads a single byte — no TOCTOU window.
    const handle = await openNoFollow(absolutePath);

    try {
      const [stats, resolved] = await Promise.all([
        handle.stat(),
        realpath(absolutePath),
      ]);

      // fstat on the opened fd: verify it is a regular file (not a device,
      // directory, socket, etc.). Note: fstat never reports isSymbolicLink()
      // because it operates on the resolved fd, not the path — symlink
      // detection is handled by O_NOFOLLOW (ELOOP) and the realpath check below.
      if (!stats.isFile()) {
        throw new Error(`security: ${absolutePath} is not a regular file`);
      }

      // On platforms where O_NOFOLLOW=0 (e.g. Windows), the open() above did
      // not prevent symlink traversal. A post-open realpath comparison catches
      // a symlink that was in place at open time.
      if (resolved !== absolutePath) {
        throw new Error(
          `security: path ${absolutePath} resolved to unexpected location ${resolved} — possible symlink`,
        );
      }

      return { handle, size: stats.size };
    } catch (err) {
      await handle.close();
      throw err;
    }
  }

  async function scan(absolutePath: string, virtualKey: string, depth: number = 0): Promise<void> {
    // Reliability: bound recursion depth explicitly — maxModules limits total
    // nodes but not stack frames; a linear chain of 256 imports would create
    // 256 frames without this guard.
    if (depth > MAX_IMPORT_DEPTH) {
      throw new Error(
        `resource limit: import chain depth exceeds maximum of ${MAX_IMPORT_DEPTH}`,
      );
    }

    if (visited.has(absolutePath)) {
      return;
    }
    visited.add(absolutePath);

    // Resource limit: check module count immediately after marking visited so
    // the count is O(1) and there is no off-by-one from checking after the write.
    if (visited.size > maxModules) {
      throw new Error(
        `resource limit: module count exceeds maximum of ${maxModules}`,
      );
    }

    const { handle, size: fileSize } = await openAndValidateModule(absolutePath);

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

      content = await handle.readFile({ encoding: 'utf-8' });
    } finally {
      await handle.close();
    }

    modules[virtualKey] = content;

    const importPaths = scanImports(content);
    const absoluteDir = dirname(absolutePath);

    // Parallelize child reads at each level.
    await Promise.all(
      importPaths.map(async (importPath) => {
        const childAbsolute = validateImportPath(importPath, absoluteDir);

        // Compute virtual key using normalizeVirtualKey to mirror Rust's VirtualFs::normalize().
        const childVirtualKey = normalizeVirtualKey(virtualKey, importPath);

        await scan(childAbsolute, childVirtualKey, depth + 1);
      }),
    );
  }

  await scan(absoluteEntry, entryFilename);

  return { entryFilename, modules };
}
