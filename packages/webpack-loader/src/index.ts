import type { MdsPluginOptions } from '@mds/bundler-utils';
import { LazyInit, createMdsTransformer, formatMdsError } from '@mds/bundler-utils';

// WORKAROUND: When compiled to CJS, TypeScript rewrites `import()` to
// `require()`, breaking ESM-only packages like `@mds/mds`. This wrapper
// preserves native `import()` by creating a new Function at runtime — the
// compiler cannot see through the string literal.
// See: https://github.com/microsoft/TypeScript/issues/43329
//
// The wrapper is intentionally parameter-less (calls `import('@mds/mds')`
// directly) so that no arbitrary module ID can be passed through
// new Function — eliminating the latent code-loading vector.
//
// CSP caveat: new Function() is functionally equivalent to eval() for
// Content Security Policy purposes. Environments with `unsafe-eval` blocked
// will reject this call. Webpack loaders run in Node.js (no CSP by default),
// so this is safe for the intended use case. If you need to run this loader
// in a CSP-restricted environment, remove the CSP restriction for Node.js
// or switch to an ESM-only build pipeline.
// eslint-disable-next-line @typescript-eslint/no-implied-eval
const esmImport: () => Promise<unknown> = new Function(
  'return import("@mds/mds")',
) as () => Promise<unknown>;

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
let capturedOptions: MdsPluginOptions | null = null;

function getLazy(options: MdsPluginOptions, emitWarning: (err: Error) => void): LazyInit<Transformer> {
  if (lazy === null) {
    capturedOptions = options;
    lazy = new LazyInit(async () => {
      const importResult = esmImport();
      // Runtime validation: esmImport() must return a thenable (Promise-like).
      // new Function() bypasses TypeScript's type checker, so the return type
      // annotation is not enforced at runtime. A non-thenable here would cause
      // a silent hang rather than a clear error.
      if (
        importResult === null ||
        typeof importResult !== 'object' ||
        typeof (importResult as { then?: unknown }).then !== 'function'
      ) {
        throw new Error(
          'esmImport() did not return a thenable. The new Function() wrapper is broken in this environment.',
        );
      }
      const mds = await importResult as typeof import('@mds/mds');
      const mdsAny = mds as Record<string, unknown>;
      if (typeof mdsAny['compileFile'] !== 'function' || typeof mdsAny['init'] !== 'function') {
        throw new Error(
          '@mds/mds module shape is unexpected: compileFile and init must both be functions. ' +
          'Check that the installed version is compatible.',
        );
      }
      return createMdsTransformer(mds, options);
    });
  } else if (capturedOptions !== null && JSON.stringify(options) !== JSON.stringify(capturedOptions)) {
    emitWarning(new Error(
      'mds-webpack-loader: options changed between invocations but the transformer singleton ' +
      'was already initialised with the original options. The new options will be ignored. ' +
      'Use separate webpack processes for different option sets.',
    ));
  }
  return lazy;
}

export default async function mdsLoader(this: LoaderContext): Promise<void> {
  const callback = this.async();
  try {
    const options = this.getOptions();
    const t = await getLazy(options, this.emitWarning.bind(this)).get();
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
 */
export function _resetForTesting(): void {
  lazy?.reset();
  lazy = null;
  capturedOptions = null;
}

/**
 * Inject a pre-built transformer for testing without going through the real
 * @mds/mds import. Allows tests to provide a mock transformer that returns
 * controlled warnings, dependencies, and output. Pass null to tear down the
 * injected transformer (equivalent to calling _resetForTesting).
 */
export function _setTransformerForTesting(t: Transformer | null): void {
  if (t === null) {
    lazy?.reset();
    lazy = null;
    capturedOptions = null;
    return;
  }
  lazy = new LazyInit(async () => t);
}
