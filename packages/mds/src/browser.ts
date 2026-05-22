import type {
  BackendType,
  CheckResult,
  CompileOptions,
  CompileResult,
  FileOptions,
  InitOptions,
  MdsBackend,
} from './types.js';
import { createWasmBackend } from './backend/wasm.js';

export { isMdsError } from './types.js';
export type {
  CompileResult,
  CheckResult,
  CompileOptions,
  FileOptions,
  MdsErrorSpan,
  MdsError,
  BackendType,
  InitOptions,
} from './types.js';

let resolvedBackend: MdsBackend | undefined;
// Cached as the same Promise<void> object so concurrent init() calls return
// reference-equal promises. Reset on rejection so callers can retry;
// wasm.ts's MAX_INIT_RETRIES enforces a permanent failure bound.
let initVoidPromise: Promise<void> | null = null;

/**
 * Initialize the WASM backend. Must be called before compile/check in browser environments.
 *
 * Idempotent — safe to call multiple times. Concurrent calls receive the same
 * promise object (reference-equal), preventing double-init races. Delegates all
 * retry and race logic to the WASM adapter (MAX_INIT_RETRIES=3 in wasm.ts).
 */
export function init(options?: InitOptions): Promise<void> {
  if (resolvedBackend !== undefined) return Promise.resolve();
  if (initVoidPromise !== null) return initVoidPromise;
  initVoidPromise = createWasmBackend(options)
    .then((b) => {
      resolvedBackend = b;
    })
    .catch((err) => {
      // Reset so a subsequent call can retry after a transient failure.
      // wasm.ts's MAX_INIT_RETRIES ensures eventual permanent failure.
      initVoidPromise = null;
      throw err;
    });
  return initVoidPromise;
}

function assertInitialized(): MdsBackend {
  if (resolvedBackend === undefined) {
    throw new Error('@mds/mds: call init() before using compile/check in a browser environment');
  }
  return resolvedBackend;
}

/**
 * Compile an MDS source string to Markdown.
 * Requires init() to have been called and awaited first.
 */
export function compile(source: string, options?: CompileOptions): CompileResult {
  return assertInitialized().compile(source, options);
}

/**
 * Validate an MDS source string without rendering.
 * Requires init() to have been called and awaited first.
 */
export function check(source: string, options?: CompileOptions): CheckResult {
  return assertInitialized().check(source, options);
}

/**
 * Returns the active backend type. Always `'wasm'` in browser environments.
 */
export function getBackend(): BackendType {
  return 'wasm';
}

/**
 * Not available in browser environments.
 * @throws Always throws — use compile() with a pre-loaded source string instead.
 */
export function compileFile(_path: string, _options?: FileOptions): Promise<CompileResult> {
  return Promise.reject(
    new Error(
      '@mds/mds: compileFile() is not available in browser environments. ' +
      'Use compile() with a pre-loaded source string instead.',
    ),
  );
}

/**
 * Not available in browser environments.
 * @throws Always throws — use check() with a pre-loaded source string instead.
 */
export function checkFile(_path: string, _options?: FileOptions): Promise<CheckResult> {
  return Promise.reject(
    new Error(
      '@mds/mds: checkFile() is not available in browser environments. ' +
      'Use check() with a pre-loaded source string instead.',
    ),
  );
}
