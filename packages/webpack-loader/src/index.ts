import type { MdsPluginOptions } from '@mds/bundler-utils';
import { createMdsTransformer, formatMdsError } from '@mds/bundler-utils';

interface LoaderContext {
  resourcePath: string;
  async(): (err: Error | null, content?: string) => void;
  addDependency(dep: string): void;
  emitWarning(err: Error): void;
  getOptions(): MdsPluginOptions;
}

let transformer: ReturnType<typeof createMdsTransformer> | null = null;
let initPromise: Promise<void> | null = null;

async function ensureTransformer(options: MdsPluginOptions): Promise<NonNullable<typeof transformer>> {
  if (transformer !== null) return transformer;
  if (initPromise === null) {
    // NOTE: options are captured from the first call. Webpack loaders are
    // stateless functions invoked per-file; options come from the webpack
    // config and do not change across loader invocations within a single
    // build. Multiple compiler instances with different options are not
    // supported by a module-level singleton — use separate webpack processes
    // in that scenario.
    initPromise = import('@mds/mds')
      .then((mds) => {
        transformer = createMdsTransformer(mds, options);
      })
      .catch((err: unknown) => {
        // Reset so the next call can retry the dynamic import.
        initPromise = null;
        throw err;
      });
  }
  await initPromise;
  // After initPromise resolves, transformer is guaranteed non-null:
  // the .then() callback sets it before the promise resolves.
  return transformer!;
}

export default async function mdsLoader(this: LoaderContext): Promise<void> {
  const callback = this.async();
  try {
    const options = this.getOptions();
    const t = await ensureTransformer(options);
    const result = await t.transform(this.resourcePath);
    for (const dep of result.dependencies) {
      this.addDependency(dep);
    }
    for (const warning of result.warnings) {
      this.emitWarning(new Error(warning));
    }
    callback(null, result.code);
  } catch (err) {
    const formatted = formatMdsError(err, this.resourcePath);
    callback(new Error(formatted.message));
  }
}

/**
 * Reset singleton state for testing.
 * FOR TESTING ONLY — throws in production environments.
 */
export function _resetForTesting(): void {
  if (process.env['NODE_ENV'] === 'production') {
    throw new Error('_resetForTesting must not be called in production');
  }
  transformer = null;
  initPromise = null;
}
