import type { MdsPluginOptions } from '@mds/bundler-utils';
import { LazyInit, createMdsTransformer, formatMdsError } from '@mds/bundler-utils';

// Hand-rolled rather than `import type { LoaderContext } from 'webpack'` because
// webpack uses a CJS `export =` shape that is awkward to import in a pure-ESM
// package and the full type is a large intersection of ~10 interfaces. The
// structural subset below captures exactly what this loader uses.
interface LoaderContext {
  resourcePath: string;
  async(): (err: Error | null, content?: string) => void;
  addDependency(dep: string): void;
  emitWarning(err: Error): void;
  getOptions(): MdsPluginOptions;
}

type Transformer = ReturnType<typeof createMdsTransformer>;

// NOTE: options are captured from the first call. Webpack loaders are
// stateless functions invoked per-file; options come from the webpack
// config and do not change across loader invocations within a single
// build. Multiple compiler instances with different options are not
// supported by a module-level singleton — use separate webpack processes
// in that scenario.
let lazy: LazyInit<Transformer> | null = null;

function getLazy(options: MdsPluginOptions): LazyInit<Transformer> {
  if (lazy === null) {
    lazy = new LazyInit(async () => {
      const mds = await import('@mds/mds');
      return createMdsTransformer(mds, options);
    });
  }
  return lazy;
}

export default async function mdsLoader(this: LoaderContext): Promise<void> {
  const callback = this.async();
  try {
    const options = this.getOptions();
    const t = await getLazy(options).get();
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
 * FOR TESTING ONLY — throws unless NODE_ENV=test.
 */
export function _resetForTesting(): void {
  if (process.env['NODE_ENV'] !== 'test') {
    throw new Error('_resetForTesting is only allowed when NODE_ENV=test');
  }
  lazy?.reset();
  lazy = null;
}

/**
 * Inject a pre-built transformer for testing without going through the real
 * @mds/mds import. Allows tests to provide a mock transformer that returns
 * controlled warnings, dependencies, and output. Pass null to tear down the
 * injected transformer (equivalent to calling _resetForTesting).
 * FOR TESTING ONLY — throws unless NODE_ENV=test.
 */
export function _setTransformerForTesting(t: Transformer | null): void {
  if (process.env['NODE_ENV'] !== 'test') {
    throw new Error('_setTransformerForTesting is only allowed when NODE_ENV=test');
  }
  if (t === null) {
    lazy?.reset();
    lazy = null;
    return;
  }
  lazy = new LazyInit(async () => t);
}
