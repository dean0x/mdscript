import type {
  BackendType,
  CheckFileOptions,
  CheckOptions,
  CheckResult,
  CompileOptions,
  CompileResult,
  FileOptions,
  InitOptions,
  LintFileOptions,
  LintOptions,
  LintResult,
  MdsBaseBackend,
  MdsNodeBackend,
} from './types.js';
import { assertResultShape } from './backend/contract.js';
import { initWasmNode, createWasmBackend, fileOpts } from './backend/wasm.js';
import type { WasmModule } from './backend/wasm.js';
import { buildModulesMap } from './util/module-scanner.js';
import { assertKnownKeys } from './util/options.js';

// Read MDS_BACKEND at module scope — sync, deterministic, no I/O.
const rawBackend = process.env['MDS_BACKEND'];
const forceBackend: BackendType | undefined =
  rawBackend === 'native' || rawBackend === 'wasm' ? rawBackend : undefined;
if (rawBackend !== undefined && forceBackend === undefined) {
  console.warn(`@mdscript/mds: ignoring unknown MDS_BACKEND value "${rawBackend}"; expected "native" or "wasm"`);
}

// ---------------------------------------------------------------------------
// Module-level lazy-init state (no TLA)
// ---------------------------------------------------------------------------

let backend: MdsNodeBackend | undefined;
let initPromise: Promise<void> | null = null;

// ---------------------------------------------------------------------------
// Test reset
// ---------------------------------------------------------------------------

/**
 * Reset all singleton state for testing.
 *
 * FOR TESTING ONLY.
 *
 * @internal
 */
export function _resetForTesting(): void {
  backend = undefined;
  initPromise = null;
}

// ---------------------------------------------------------------------------
// File-ops wrapper
// ---------------------------------------------------------------------------

/**
 * Build the `mds::invalid_options` error for `basePath` on file-surface methods.
 *
 * Separated from the throw so that both the `wrapWithFileOps` guards (synchronous
 * throw inside an async function → rejected promise) and the public API guards
 * (`Promise.reject(fileBasePathError())` — maintains the sync-throw contract for
 * other errors while making basePath a proper promise rejection) can share one
 * message string, satisfying U-OV-27's byte-identical requirement (avoids PF-007).
 *
 * Message is byte-identical to napi parse_file_opts (lib.rs) so that U-OV-27
 * can assert runtime equality across both backends.
 */
function fileBasePathError(): Error & { code: string } {
  const err = new Error(
    'option "basePath" is not valid for compileFile/checkFile; ' +
    'the base directory is derived from the file path',
  ) as Error & { code: string };
  err.code = 'mds::invalid_options';
  return err;
}

/**
 * Wrap a MdsBaseBackend with file-based compile/check operations, producing
 * a MdsNodeBackend. The wasmModule is captured so compileFile/checkFile can
 * call wasm.scanImports() to resolve @import directives.
 *
 * buildModulesMap is imported here (Node-only), not in wasm.ts, so that
 * wasm.ts remains browser-safe.
 */
function wrapWithFileOps(
  base: MdsBaseBackend,
  wasmModule: WasmModule,
): MdsNodeBackend {
  /**
   * Build the modules map for a file entry point and extract the entry source,
   * removing it from the map. WASM's build_modules() treats `modules` as extra
   * dependencies and inserts the entry source separately under `filename` — if
   * the entry key is still present in `modules`, it throws mds::filename_collision.
   */
  async function prepareFileArgs(
    path: string,
    options: FileOptions | undefined,
  ): Promise<{ source: string; opts: ReturnType<typeof fileOpts> }> {
    const { entryFilename, modules } = await buildModulesMap(path, (src) => wasmModule.scanImports(src));
    const source = modules[entryFilename];
    if (source === undefined) {
      throw new Error(
        `buildModulesMap did not populate entry file "${entryFilename}" in modules map`,
      );
    }
    delete modules[entryFilename];
    return { source, opts: fileOpts(entryFilename, modules, options) };
  }

  return {
    ...base,

    async compileFile(path: string, options?: FileOptions): Promise<CompileResult> {
      // D-TS-06 guard: basePath is not valid for file operations on the WASM path.
      // The native path never enters wrapWithFileOps, so napi handles the rejection
      // there. BASEPATH_PASSTHROUGH lets basePath through assertKnownKeys; the guard
      // here fires on the WASM path before buildModulesMap runs (avoids PF-004).
      if ((options as unknown as { basePath?: string })?.basePath != null) {
        throw fileBasePathError();
      }
      const { source, opts } = await prepareFileArgs(path, options);
      const result: unknown = wasmModule.compile(source, opts);
      assertResultShape(result, 'compile');
      return result as CompileResult;
    },

    async checkFile(path: string, options?: CheckFileOptions): Promise<CheckResult> {
      // D-TS-06 guard: same reason as compileFile above.
      if ((options as unknown as { basePath?: string })?.basePath != null) {
        throw fileBasePathError();
      }
      // CheckFileOptions is a structural subset of FileOptions (only vars, no
      // sourceMap/sourcesContent), so the cast is safe: prepareFileArgs calls
      // fileCompileOpt which only picks defined keys; the absent fields resolve as
      // undefined and are not included in the returned opts object.
      const { source, opts } = await prepareFileArgs(path, options as FileOptions | undefined);
      const result: unknown = wasmModule.check(source, opts);
      assertResultShape(result, 'check');
      return result as CheckResult;
    },

    async lintFile(path: string, options?: LintFileOptions): Promise<LintResult> {
      // Use buildModulesMap so the WASM check gate can resolve @import chains,
      // matching the behaviour of the native lintFile (which uses NativeFs).
      // Note: entryFilename is project-root-relative (e.g. "templates/foo.mds"),
      // not just the basename — this differs from the native backend's filename
      // in the canonical JSON. Use lintVirtual for byte-identical cross-surface
      // comparison.
      const { entryFilename, modules } = await buildModulesMap(
        path,
        (src) => wasmModule.scanImports(src),
      );
      const entrySource = modules[entryFilename];
      if (entrySource === undefined) {
        throw new Error(
          `buildModulesMap did not populate entry file "${entryFilename}" in modules map`,
        );
      }
      // Build a copy of modules without the entry (lint() inserts it separately).
      const extraModules: Record<string, string> = { ...modules };
      delete extraModules[entryFilename];
      const lintOpts: {
        filename: string;
        modules?: Record<string, string>;
        vars?: Record<string, unknown>;
        rules?: Record<string, string>;
      } = { filename: entryFilename };
      if (Object.keys(extraModules).length > 0) lintOpts.modules = extraModules;
      if (options?.vars != null) lintOpts.vars = options.vars;
      if (options?.rules != null) lintOpts.rules = options.rules;
      const result: unknown = wasmModule.lint(entrySource, lintOpts);
      assertResultShape(result, 'lint');
      return result as LintResult;
    },
  };
}

// ---------------------------------------------------------------------------
// Backend loaders (decomposed from ensureBackend)
// ---------------------------------------------------------------------------

/**
 * Try to load the native (napi) backend. Returns null on failure.
 * Captures the error for diagnostics without throwing.
 */
async function loadNativeBackend(): Promise<{ backend: MdsNodeBackend; error: null } | { backend: null; error: Error }> {
  try {
    const { createRequire } = await import('node:module');
    const require = createRequire(import.meta.url);
    const addon = require('@mdscript/mds-napi') as object;
    const { createNativeBackend } = await import('./backend/native.js');
    const b = createNativeBackend(addon as Parameters<typeof createNativeBackend>[0]);
    return { backend: b, error: null };
  } catch (err) {
    return { backend: null, error: err instanceof Error ? err : new Error(String(err)) };
  }
}

/**
 * Load the WASM backend for Node.js. Always returns a MdsNodeBackend.
 * Throws if the WASM module cannot be loaded.
 */
async function loadWasmNodeBackend(options?: InitOptions): Promise<MdsNodeBackend> {
  const wasmModule = await initWasmNode(options);
  const base = createWasmBackend(wasmModule);
  return wrapWithFileOps(base, wasmModule);
}

// ---------------------------------------------------------------------------
// Lazy init orchestrator
// ---------------------------------------------------------------------------

/**
 * Ensure the backend is initialized, with promise deduplication.
 * Called by init() and is the single source of truth for backend selection.
 */
async function ensureBackend(options?: InitOptions): Promise<void> {
  if (forceBackend === 'wasm') {
    backend = await loadWasmNodeBackend(options);
    return;
  }

  if (forceBackend === 'native') {
    const result = await loadNativeBackend();
    if (result.backend === null) {
      throw new Error(`MDS_BACKEND=native but native addon failed to load: ${result.error.message}`);
    }
    backend = result.backend;
    return;
  }

  // Default: prefer native, fall back to WASM.
  const nativeResult = await loadNativeBackend();
  if (nativeResult.backend !== null) {
    backend = nativeResult.backend;
    return;
  }

  console.warn('@mdscript/mds: native addon unavailable, falling back to WASM');
  try {
    backend = await loadWasmNodeBackend(options);
  } catch (wasmErr) {
    throw new Error(
      `@mdscript/mds: no backend available. Native: ${nativeResult.error.message}. WASM: ${String(wasmErr)}`,
    );
  }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/**
 * Explicitly initialize the backend. Must be called and awaited before any
 * other export (compile, check, compileFile, checkFile, getBackend).
 *
 * Idempotent — safe to call multiple times. Concurrent calls share a single
 * promise, preventing double-initialization races.
 */
export function init(options?: InitOptions): Promise<void> {
  if (backend !== undefined) return Promise.resolve();
  if (initPromise !== null) return initPromise;
  initPromise = ensureBackend(options).catch((err: unknown) => {
    initPromise = null;
    throw err;
  });
  return initPromise;
}

function assertReady(): MdsNodeBackend {
  if (backend === undefined) {
    throw new Error(
      '@mdscript/mds: call await init() before using compile/check/compileFile/checkFile/getBackend',
    );
  }
  return backend;
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
 * Compile an MDS file, resolving @import directives relative to the file.
 * Returns a discriminated-union CompileResult. Requires init() to have been called and awaited first.
 *
 * Non-async: unknown option keys and pre-init errors throw synchronously (preserving the
 * existing contract verified by U-OV-12 and U-B11). The basePath guard returns
 * `Promise.reject(fileBasePathError())` so that callers using `assert.rejects()` or
 * `.catch()` receive a proper rejected promise rather than a synchronous throw.
 */
export function compileFile(path: string, options?: FileOptions): Promise<CompileResult> {
  if (options != null) assertKnownKeys(options, 'compileFile');
  // BASEPATH_PASSTHROUGH: assertKnownKeys skips basePath for file methods (issue #74).
  // Return a rejected promise (not a synchronous throw) so that the file-op async
  // contract is consistent: backend errors arrive as rejections, not sync throws.
  // wrapWithFileOps provides a redundant guard on the WASM path (avoids PF-004).
  if ((options as unknown as { basePath?: string })?.basePath != null) {
    return Promise.reject(fileBasePathError());
  }
  return assertReady().compileFile(path, options);
}

/**
 * Validate an MDS file without rendering, resolving @import directives relative to the file.
 * Only `vars` is forwarded; `basePath` and source-map options are not applicable to file
 * operations (the base directory is derived from the file path).
 * Requires init() to have been called and awaited first.
 *
 * Same async contract as compileFile: basePath guard returns `Promise.reject()`.
 */
export function checkFile(path: string, options?: CheckFileOptions): Promise<CheckResult> {
  if (options != null) assertKnownKeys(options, 'checkFile');
  // Same basePath guard — returns Promise.reject to preserve async contract.
  if ((options as unknown as { basePath?: string })?.basePath != null) {
    return Promise.reject(fileBasePathError());
  }
  return assertReady().checkFile(path, options);
}

/** Lint an MDS source string. Returns a LintResult with per-rule findings. Requires init() to have been called and awaited first. */
export function lint(source: string, options?: LintOptions): LintResult {
  if (options != null) assertKnownKeys(options, 'lint');
  return assertReady().lint(source, options);
}

/** Lint an MDS file, resolving @import directives relative to the file. Requires init() to have been called and awaited first. */
export function lintFile(path: string, options?: LintFileOptions): Promise<LintResult> {
  if (options != null) assertKnownKeys(options, 'lintFile');
  return assertReady().lintFile(path, options);
}

/** Lint a multi-module virtual filesystem. Caller provides the full module map and entry key. Requires init() to have been called and awaited first. */
export function lintVirtual(
  modules: Record<string, string>,
  entry: string,
  options?: LintFileOptions,
): LintResult {
  if (options != null) assertKnownKeys(options, 'lintVirtual');
  return assertReady().lintVirtual(modules, entry, options);
}

/** Returns which backend is currently active: 'native' or 'wasm'. Requires init() to have been called and awaited first. */
export function getBackend(): BackendType {
  return assertReady().getBackend();
}

// `LINT_RULE_NAMES` is exported here, not only from `index.ts`: the package
// `exports` map resolves `@mdscript/mds` to `dist/node.js` (Node) or
// `dist/browser.js`, and never to `dist/index.js` — a value re-exported only
// from `index.ts` is unreachable for consumers. The browser entry gains it with
// the browser lint surface; today it has no lint API to configure.
export { isMdsError, LINT_RULE_NAMES } from './types.js';
export type {
  BackendType,
  CheckFileOptions,
  CheckOptions,
  CheckResult,
  CompileOptions,
  CompileResult,
  FileOptions,
  InitOptions,
  LintDiagnostic,
  LintFileOptions,
  LintFileReport,
  LintOptions,
  LintResult,
  LintRuleName,
  LintSpan,
  RuleSeverity,
  MarkdownResult,
  Message,
  MessagesResult,
  MdsError,
  MdsErrorSpan,
} from './types.js';
