import type { MdsPluginOptions } from '@mdscript/bundler-utils';
import { createMdsTransformer, formatMdsError, cleanId } from '@mdscript/bundler-utils';

// Structural subset of Rollup's PluginContext and Plugin. We intentionally keep
// narrow interfaces rather than importing `Plugin` from 'rollup' because:
//   1. Rollup's Plugin<A> extends OutputPlugin and Partial<PluginHooks> which
//      pulls in dozens of hook types (resolveId, load, renderChunk, …) unused here.
//   2. The structural types below are verified at build time via assignability
//      checks, so type drift is caught without the full import overhead.
// If Rollup's API surface changes in a breaking way, TypeScript will report it.
interface PluginContext {
  warn(msg: string): void;
  addWatchFile(id: string): void;
  error(msg: string, pos?: { line: number; column: number }): never;
}

interface RollupPlugin {
  name: string;
  buildStart?: (this: PluginContext) => void | Promise<void>;
  transform?: (
    this: PluginContext,
    code: string,
    id: string,
  ) => Promise<{ code: string; map: null } | null>;
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
 * Rollup plugin that compiles `.mds` and `.md` (with `type: mds` frontmatter)
 * files into JavaScript modules. Uses `this.error()` for build-time errors so
 * Rollup can display them with position information. Watch-mode dependencies
 * are registered via `this.addWatchFile()` so Rollup re-compiles on changes.
 */
export default function mdsPlugin(options?: MdsPluginOptions): RollupPlugin {
  let transformer: Transformer | null = null;

  return {
    name: 'mds',

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
        const pos = formatted.line !== undefined
          ? { line: formatted.line, column: formatted.column ?? 0 }
          : undefined;
        this.error(formatted.message, pos);
      }
    },
  };
}
