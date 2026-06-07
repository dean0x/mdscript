import type { BackendType, CheckResult, CompileMessagesResult, CompileOptions, CompileResult, InitOptions, MdsBaseBackend } from './types.js';
import { initWasmBrowser, createWasmBackend } from './backend/wasm.js';

export { isMdsError } from './types.js';
export type {
  BackendType,
  CheckResult,
  CompileMessagesResult,
  CompileOptions,
  CompileResult,
  InitOptions,
  Message,
  MdsError,
  MdsErrorSpan,
} from './types.js';

let resolvedBackend: MdsBaseBackend | undefined;
// Cached while the init attempt is in-flight so concurrent init() calls share
// the same promise and don't trigger double-initialization. Reset to null on
// rejection, so that subsequent calls re-enter initWasmBrowser() and can retry.
// Cleared to null permanently once resolvedBackend is set (resolvedBackend guard
// short-circuits first).
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
 * Inject a pre-loaded WasmModule for testing without going through initWasmBrowser().
 *
 * FOR TESTING ONLY — allows Node.js test suites to exercise the browser entry
 * API surface without triggering a browser-only bundler import path.
 *
 * @internal
 */
export function _initWithModuleForTesting(mod: import('./backend/wasm.js').WasmModule): void {
  resolvedBackend = createWasmBackend(mod);
  initVoidPromise = null;
}

/**
 * Initialize the WASM backend. Must be called before compile/check in browser environments.
 *
 * Idempotent — safe to call multiple times. Concurrent calls in flight share
 * the same promise, preventing double-init races. On transient failure the
 * cached promise is cleared so the next call can retry, delegating retry
 * counting and exhaustion to initWasmBrowser().
 */
export function init(options?: InitOptions): Promise<void> {
  if (resolvedBackend !== undefined) return Promise.resolve();
  if (initVoidPromise !== null) return initVoidPromise;
  initVoidPromise = initWasmBrowser(options).then((mod) => {
    resolvedBackend = createWasmBackend(mod);
  }).catch((err: unknown) => {
    // Clear so the next init() call re-enters initWasmBrowser() rather than
    // returning this stale rejected promise.
    initVoidPromise = null;
    throw err;
  });
  return initVoidPromise;
}

function assertReady(): MdsBaseBackend {
  if (resolvedBackend === undefined) {
    throw new Error('@mdscript/mds: call await init() before using compile/check/compileMessages in a browser environment');
  }
  return resolvedBackend;
}

/** Compile an MDS source string to Markdown. Requires init() to have been called and awaited first. */
export function compile(source: string, options?: CompileOptions): CompileResult {
  return assertReady().compile(source, options);
}

/** Validate an MDS source string without rendering. Requires init() to have been called and awaited first. */
export function check(source: string, options?: CompileOptions): CheckResult {
  return assertReady().check(source, options);
}

/** Compile `@message` blocks in an MDS source string to structured chat messages. Requires init() to have been called and awaited first. */
export function compileMessages(source: string, options?: CompileOptions): CompileMessagesResult {
  return assertReady().compileMessages(source, options);
}

/** Returns the active backend type. Always `'wasm'` in browser environments. */
export function getBackend(): BackendType {
  return 'wasm';
}
