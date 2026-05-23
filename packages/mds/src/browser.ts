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
// Cached while the init attempt is in-flight so concurrent init() calls share
// the same promise and don't trigger double-initialization. Reset to null on
// rejection, so that subsequent calls re-enter createWasmBackend() and reach
// wasm.ts's retry logic (MAX_INIT_RETRIES=3). Cleared to null permanently once
// resolvedBackend is set (resolvedBackend guard short-circuits first).
let initVoidPromise: Promise<void> | null = null;

/**
 * Reset singleton state for testing.
 *
 * FOR TESTING ONLY — allows tests to drive the retry path by clearing cached
 * state between calls.
 *
 * @internal
 */
export function _resetForTesting(): void {
  resolvedBackend = undefined;
  initVoidPromise = null;
}

/**
 * Initialize the WASM backend. Must be called before compile/check in browser environments.
 *
 * Idempotent — safe to call multiple times. Concurrent calls in flight share
 * the same promise, preventing double-init races. On transient failure the
 * cached promise is cleared so the next call can retry, delegating retry
 * counting and exhaustion to the WASM adapter (MAX_INIT_RETRIES=3 in wasm.ts).
 * Once the adapter permanently exhausts its retries, every subsequent call will
 * reject immediately (driven by wasm.ts, not by this layer).
 */
export function init(options?: InitOptions): Promise<void> {
  if (resolvedBackend !== undefined) return Promise.resolve();
  if (initVoidPromise !== null) return initVoidPromise;
  initVoidPromise = createWasmBackend(options).then((b) => {
    resolvedBackend = b;
  }).catch((err: unknown) => {
    // Clear so the next init() call re-enters createWasmBackend() and reaches
    // wasm.ts's retry / exhaustion logic rather than returning this stale
    // rejected promise.
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

/** Compile an MDS source string to Markdown. Requires init() to have been called and awaited first. */
export function compile(source: string, options?: CompileOptions): CompileResult {
  return assertInitialized().compile(source, options);
}

/** Validate an MDS source string without rendering. Requires init() to have been called and awaited first. */
export function check(source: string, options?: CompileOptions): CheckResult {
  return assertInitialized().check(source, options);
}

/** Returns the active backend type. Always `'wasm'` in browser environments. */
export function getBackend(): BackendType {
  return 'wasm';
}

/**
 * Not available in browser environments.
 * @throws Always — use compile() with a pre-loaded source string instead.
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
 * @throws Always — use check() with a pre-loaded source string instead.
 */
export function checkFile(_path: string, _options?: FileOptions): Promise<CheckResult> {
  return Promise.reject(
    new Error(
      '@mds/mds: checkFile() is not available in browser environments. ' +
      'Use check() with a pre-loaded source string instead.',
    ),
  );
}
