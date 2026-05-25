import type { MdsPluginOptions } from '@mds/bundler-utils';
import { createMdsTransformer, formatMdsError, cleanId, isMdsExtension } from '@mds/bundler-utils';

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

export default function mdsPlugin(options?: MdsPluginOptions): VitePlugin {
  let transformer: ReturnType<typeof createMdsTransformer> | null = null;

  return {
    name: 'mds',
    enforce: 'pre',

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
      const clean = cleanId(ctx.file);
      if (isMdsExtension(clean)) {
        ctx.server.ws.send({ type: 'full-reload' });
        return [];
      }
      return undefined;
    },
  };
}
