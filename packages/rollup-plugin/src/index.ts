import type { MdsPluginOptions } from '@mds/bundler-utils';
import { createMdsTransformer, formatMdsError, cleanId } from '@mds/bundler-utils';

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

export default function mdsPlugin(options?: MdsPluginOptions): RollupPlugin {
  let transformer: ReturnType<typeof createMdsTransformer> | null = null;

  return {
    name: 'mds',

    async buildStart() {
      const mds = await import('@mds/mds');
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
