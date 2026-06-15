import type { MdsApi, MdsPluginOptions } from './types.js';
import { LazyInit } from './lazy-init.js';
import { createMdsTransformer } from './transform.js';
import { formatMdsError } from './errors.js';

// WORKAROUND: When compiled to CJS, TypeScript rewrites `import()` to
// `require()`, breaking ESM-only packages like `@mdscript/mds`. This wrapper
// preserves native `import()` by creating a new Function at runtime — the
// compiler cannot see through the string literal.
// See: https://github.com/microsoft/TypeScript/issues/43329
//
// The wrapper is intentionally parameter-less (calls `import('@mdscript/mds')`
// directly) so that no arbitrary module ID can be passed through
// new Function — eliminating the latent code-loading vector.
//
// CSP caveat: new Function() is functionally equivalent to eval() for
// Content Security Policy purposes. Environments with `unsafe-eval` blocked
// will reject this call. Webpack/Rspack loaders run in Node.js (no CSP by
// default), so this is safe for the intended use case.
// eslint-disable-next-line @typescript-eslint/no-implied-eval
const esmImport: () => Promise<unknown> = new Function(
  'return import("@mdscript/mds")',
) as () => Promise<unknown>;

// Hand-rolled rather than `import type { LoaderContext } from 'webpack'` because
// webpack/rspack use a CJS `export =` shape that is awkward to import in a
// pure-ESM package. This structural subset captures exactly what the loader uses.
export interface LoaderContext {
  resourcePath: string;
  async(): (err: Error | null, content?: string) => void;
  addDependency(dep: string): void;
  emitWarning(err: Error): void;
  getOptions(): MdsPluginOptions;
}

type Transformer = ReturnType<typeof createMdsTransformer>;

export interface MdsLoaderApi {
  /**
   * The loader function to export as the default from a webpack/rspack loader
   * package. Must be called with `this` bound to the LoaderContext.
   */
  loader(this: LoaderContext): Promise<void>;
  /**
   * Reset singleton state for testing. Call in beforeEach/afterEach to avoid
   * module-level singleton state leaking across tests (edge case E-singleton).
   */
  _resetForTesting(): void;
  /**
   * Inject a pre-built transformer for testing without going through the real
   * @mdscript/mds import. Allows tests to provide a mock transformer. Pass null
   * to tear down the injected transformer (equivalent to calling _resetForTesting).
   *
   * NOTE: this helper is intentionally UN-gated (no NODE_ENV check) — webpack
   * and rspack loaders expose it unconditionally, matching the original
   * webpack-loader behavior. Do not add a NODE_ENV==='test' gate here.
   */
  _setTransformerForTesting(t: Transformer | null): void;
}

/**
 * Factory that creates an independent MDS loader instance. Each call returns a
 * new set of `{loader, _resetForTesting, _setTransformerForTesting}` that close
 * over their own independent state — their own lazy-init singleton and their own
 * injected-transformer slot. Calling `createMdsLoader()` twice yields two
 * non-interfering loader instances.
 *
 * ### Options semantics
 * Options are captured from the first call. Subsequent calls with different
 * options emit a warning but continue using the original options. This matches
 * webpack/rspack semantics where loaders are stateless functions invoked per-file
 * and options come from the bundler config.
 *
 * ### CJS shim
 * The `new Function('return import("@mdscript/mds")')` CJS dynamic-import shim
 * is enclosed inside this factory, so each factory call shares the module-level
 * shim but the lazy-init state is independent per call.
 */
export function createMdsLoader(): MdsLoaderApi {
  // Per-instance state — independent for each createMdsLoader() call.
  let lazy: LazyInit<Transformer> | null = null;
  let capturedOptions: MdsPluginOptions | null = null;

  function getLazy(
    options: MdsPluginOptions,
    emitWarning: (err: Error) => void,
  ): LazyInit<Transformer> {
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
        const mds = (await importResult) as unknown as MdsApi;
        const mdsAny = mds as unknown as Record<string, unknown>;
        if (
          typeof mdsAny['compileFile'] !== 'function' ||
          typeof mdsAny['init'] !== 'function'
        ) {
          throw new Error(
            '@mdscript/mds module shape is unexpected: compileFile and init must both be functions. ' +
              'Check that the installed version is compatible.',
          );
        }
        return createMdsTransformer(mds, options);
      });
    } else if (
      capturedOptions !== null &&
      JSON.stringify(options) !== JSON.stringify(capturedOptions)
    ) {
      emitWarning(
        new Error(
          'mds-loader: options changed between invocations but the transformer singleton ' +
            'was already initialised with the original options. The new options will be ignored. ' +
            'Use separate webpack/rspack processes for different option sets.',
        ),
      );
    }
    return lazy;
  }

  async function loader(this: LoaderContext): Promise<void> {
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

  function _resetForTesting(): void {
    lazy?.reset();
    lazy = null;
    capturedOptions = null;
  }

  function _setTransformerForTesting(t: Transformer | null): void {
    if (t === null) {
      lazy?.reset();
      lazy = null;
      capturedOptions = null;
      return;
    }
    lazy = new LazyInit(async () => t);
  }

  return { loader, _resetForTesting, _setTransformerForTesting };
}
