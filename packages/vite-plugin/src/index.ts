import { realpathSync } from 'node:fs';
import { resolve as resolvePath, sep } from 'node:path';
import type { MdsPluginOptions } from '@mdscript/bundler-utils';
import { createMdsTransformer, formatMdsError, cleanId, isMdsExtension } from '@mdscript/bundler-utils';

// Structural subset of Vite's PluginContext. We intentionally keep a narrow
// interface rather than importing `Plugin` from 'vite' because:
//   1. Vite's Plugin<A> extends rollup.Plugin<A> which pulls in ObjectHook,
//      HmrContext → ViteDevServer → hundreds of types we don't use.
//   2. handleHotUpdate uses legacy HmrContext (with full ViteDevServer), while
//      we only need { file, server.ws.send }. A structural type avoids fighting
//      the generic chain and keeps this file dependency-free at the type level.
// If Vite's Plugin API surface ever changes in a breaking way that affects the
// hooks below, TypeScript will catch it at build time via structural checking.
interface PluginTransformContext {
  warn(msg: string): void;
  addWatchFile(id: string): void;
}

interface VitePlugin {
  name: string;
  enforce?: 'pre' | 'post';
  buildStart?: (this: PluginTransformContext) => void | Promise<void>;
  transform?: (
    this: PluginTransformContext,
    code: string,
    id: string,
  ) => Promise<{ code: string; map: null } | null>;
  handleHotUpdate?: (ctx: {
    file: string;
    server: { ws: { send(payload: { type: string; path?: string }): void } };
    modules?: Array<{ id: string | null }>;
  }) => void | undefined | unknown[];
}

type Transformer = ReturnType<typeof createMdsTransformer>;

/**
 * Canonicalize a path for insertion into and lookup from the MDS-watched-paths
 * Set. Canonicalization is required because:
 *   - macOS symlinks: /tmp is a symlink to /private/tmp, so the same physical
 *     file can appear under both paths.
 *   - Windows: backslash vs forward-slash separators.
 *   - Vite may give us an id with query/hash suffixes, which cleanId strips.
 *
 * canon() is called BOTH when inserting (in transform) and when looking up (in
 * handleHotUpdate). Using the same function on both sides guarantees symmetry
 * (gap D / edge E-norm). This mirrors crates/mds-cli/src/watch.rs
 * event_is_relevant 3-layer matching (symlink resolution).
 *
 * Falls back to path.resolve when realpathSync throws (e.g. deleted file).
 */
function canon(p: string): string {
  // 1. Strip query/hash suffixes (Vite appends ?t=xxx, #xxx etc.)
  const clean = cleanId(p);
  // 2. Normalize OS separators to forward-slash so paths are comparable
  //    across platforms (especially relevant on Windows).
  const normalized = clean.split(sep).join('/');
  try {
    // 3. Resolve symlinks — handles /tmp → /private/tmp on macOS.
    return realpathSync(normalized).split(sep).join('/');
  } catch {
    // File was deleted or doesn't exist yet; fall back to absolute resolve.
    return resolvePath(normalized).split(sep).join('/');
  }
}

/**
 * Inject a pre-built transformer for testing without going through the real
 * @mdscript/mds import. Allows tests to provide a mock transformer that returns
 * controlled warnings, dependencies, and output.
 * FOR TESTING ONLY — throws unless NODE_ENV=test.
 */
let _testTransformer: Transformer | null = null;
export function _setTransformerForTesting(t: Transformer | null): void {
  if (process.env['NODE_ENV'] !== 'test') {
    throw new Error('_setTransformerForTesting is only allowed when NODE_ENV=test');
  }
  _testTransformer = t;
}

/**
 * Vite plugin that compiles `.mds` and `.md` (with `type: mds` frontmatter)
 * files into JavaScript modules. Runs with `enforce: 'pre'` so it intercepts
 * before Vite's default asset handling. On file change, triggers a full-page
 * reload via HMR (see comment on handleHotUpdate below for rationale).
 *
 * HMR strategy (G1 fix): maintains a closure-level Set of every canonicalized
 * path that the plugin has transformed, plus each of their declared @import
 * dependencies (from result.dependencies). handleHotUpdate triggers a
 * full-reload when:
 *   1. isMdsExtension(clean) — fast path for bare .mds files (no transform needed)
 *   2. transformed.has(canon(ctx.file)) — a file we previously compiled changed
 *   3. ctx.modules?.some(m => m.id && transformed.has(canon(m.id))) — a dep we
 *      are watching changed (transitive dep path)
 *
 * This covers:
 *   - .md files with `type: mds` frontmatter (AC-F6 / T-vite-md)
 *   - transitive @import dependency edits
 *   - the macOS /tmp → /private/tmp symlink trap (edge E-norm)
 */
export default function mdsPlugin(options?: MdsPluginOptions): VitePlugin {
  let transformer: Transformer | null = null;

  // Closure-level Set of canonicalized paths for all files the plugin has
  // transformed AND their declared @import dependencies. The Set is bounded by
  // the number of distinct MDS files + deps in the project (AC-P4 / T-P2).
  // Stale entries (deleted files) cause at most one extra reload CHECK, never
  // unbounded growth. No eviction is needed.
  const transformed = new Set<string>();

  return {
    name: 'mds',
    enforce: 'pre',

    async buildStart() {
      if (_testTransformer !== null) {
        transformer = _testTransformer;
        return;
      }
      const mds = await import('@mdscript/mds');
      transformer = createMdsTransformer(mds, options);
    },

    async transform(_, id) {
      if (transformer === null) return null;
      const clean = cleanId(id);
      const should = await transformer.shouldTransform(clean);
      if (!should) return null;

      try {
        const result = await transformer.transform(clean);
        // Track this file and all its declared @import deps in the Set.
        // canon() is called here (insert) and in handleHotUpdate (lookup) with
        // the same function — symmetry guarantees correct cross-referencing
        // even across symlinks (gap D / edge E-norm).
        transformed.add(canon(id));
        for (const dep of result.dependencies) {
          // result.dependencies are already absolute/canonical paths from the
          // mds compiler; we still run through canon() for OS-separator
          // normalization and symlink resolution.
          transformed.add(canon(dep));
          this.addWatchFile(dep);
        }
        for (const warning of result.warnings) {
          this.warn(warning);
        }
        return { code: result.code, map: null };
      } catch (err) {
        const formatted = formatMdsError(err, clean);
        // Vite expects thrown errors (not this.error()) for the error overlay.
        // Attach loc and id so Vite can display the error with position info.
        const error = Object.assign(new Error(formatted.message), {
          id: formatted.id,
          loc: formatted.line !== undefined
            ? { line: formatted.line, column: formatted.column ?? 0 }
            : undefined,
        });
        throw error;
      }
    },

    handleHotUpdate(ctx) {
      // Full-reload instead of granular HMR is intentional for v0.1.0.
      // MDS files export plain strings (no stateful React/Vue components),
      // so there is no module graph to hot-swap — a full reload is both safe
      // and correct. Targeted HMR (invalidating only the importing modules) is
      // a future optimisation once the module graph integration is validated.
      //
      // G1 fix: in addition to the fast-path isMdsExtension check, we also
      // trigger a full-reload when:
      //   - the file is in the `transformed` Set (covers .md+type:mds, deps)
      //   - any module in ctx.modules is in the `transformed` Set (transitive)
      const canonFile = canon(ctx.file);
      const isMdsFile = isMdsExtension(cleanId(ctx.file));
      const isTracked = transformed.has(canonFile);
      const hasTrackedModule = (ctx.modules ?? []).some(
        (m) => m.id != null && transformed.has(canon(m.id)),
      );

      if (isMdsFile || isTracked || hasTrackedModule) {
        ctx.server.ws.send({ type: 'full-reload' });
        return [];
      }
      return undefined;
    },
  };
}
