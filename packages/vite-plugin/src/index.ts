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
  }) => void | undefined | unknown[];
}

type Transformer = ReturnType<typeof createMdsTransformer>;

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
 */
export default function mdsPlugin(options?: MdsPluginOptions): VitePlugin {
  let transformer: Transformer | null = null;

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
        for (const dep of result.dependencies) {
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
      const clean = cleanId(ctx.file);
      if (isMdsExtension(clean)) {
        ctx.server.ws.send({ type: 'full-reload' });
        return [];
      }
      return undefined;
    },
  };
}
