import type {
  BackendType,
  CheckOptions,
  CheckResult,
  CompileOptions,
  CompileResult,
  InitOptions,
  LintFileOptions,
  LintOptions,
  LintResult,
  MdsBaseBackend,
} from './types.js';
import { initWasmBrowser, createWasmBackend } from './backend/wasm.js';
import { assertKnownKeys } from './util/options.js';

export { isMdsError, LINT_RULE_NAMES } from './types.js';
export type {
  BackendType,
  CheckOptions,
  CheckResult,
  CompileOptions,
  CompileResult,
  InitOptions,
  LintDiagnostic,
  LintFileOptions,
  LintFileReport,
  LintOptions,
  LintResult,
  LintRuleName,
  LintSpan,
  MarkdownResult,
  Message,
  MessagesResult,
  MdsError,
  MdsErrorSpan,
  RuleSeverity,
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
 * Initialize the WASM backend. Must be called before compile/check/lint in browser environments.
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
    throw new Error('@mdscript/mds: call await init() before using compile/check/lint in a browser environment');
  }
  return resolvedBackend;
}

/** Compile an MDS source string. Returns a discriminated-union CompileResult (kind: 'markdown' | 'messages'). Requires init() to have been called and awaited first. */
export function compile(source: string, options?: CompileOptions): CompileResult {
  if (options != null) assertKnownKeys(options, 'compile');
  return assertReady().compile(source, options);
}

/** Validate an MDS source string without rendering. Requires init() to have been called and awaited first. */
export function check(source: string, options?: CheckOptions): CheckResult {
  if (options != null) assertKnownKeys(options, 'check');
  return assertReady().check(source, options);
}

/**
 * Lint an MDS source string. Returns a LintResult with per-rule findings.
 * Requires init() to have been called and awaited first.
 *
 * D-TS-07: `lintFile` is intentionally absent from the browser entry.
 * `MdsBaseBackend` has no `lintFile` — file operations require `node:fs` which
 * is unavailable in browser environments. Use `lintVirtual` to lint a
 * pre-loaded module map, or import from `@mdscript/mds` in Node.js to get
 * access to `lintFile`.
 */
export function lint(source: string, options?: LintOptions): LintResult {
  if (options != null) assertKnownKeys(options, 'lint');
  return assertReady().lint(source, options);
}

/**
 * Lint a multi-module virtual filesystem. Caller provides the full module map
 * and entry key. Returns a LintResult with per-rule findings.
 * Requires init() to have been called and awaited first.
 */
export function lintVirtual(
  modules: Record<string, string>,
  entry: string,
  options?: LintFileOptions,
): LintResult {
  if (options != null) assertKnownKeys(options, 'lintVirtual');
  return assertReady().lintVirtual(modules, entry, options);
}

/** Returns the active backend type. Always `'wasm'` in browser environments. */
export function getBackend(): BackendType {
  return 'wasm';
}
