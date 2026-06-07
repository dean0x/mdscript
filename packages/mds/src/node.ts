import type {
  BackendType,
  MdsBaseBackend,
  MdsNodeBackend,
  CompileMessagesResult,
  CompileResult,
  CheckResult,
  CompileOptions,
  FileOptions,
  InitOptions,
} from './types.js';
import { initWasmNode, createWasmBackend, fileOpts } from './backend/wasm.js';
import type { WasmModule } from './backend/wasm.js';
import { buildModulesMap } from './util/module-scanner.js';

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
      const { source, opts } = await prepareFileArgs(path, options);
      return wasmModule.compile(source, opts);
    },

    async checkFile(path: string, options?: FileOptions): Promise<CheckResult> {
      const { source, opts } = await prepareFileArgs(path, options);
      return wasmModule.check(source, opts);
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
      '@mdscript/mds: call await init() before using compile/check/compileMessages/compileFile/checkFile/getBackend',
    );
  }
  return backend;
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

/** Compile an MDS file to Markdown, resolving @import directives relative to the file. Requires init() to have been called and awaited first. */
export function compileFile(path: string, options?: FileOptions): Promise<CompileResult> {
  return assertReady().compileFile(path, options);
}

/** Validate an MDS file without rendering, resolving @import directives relative to the file. Requires init() to have been called and awaited first. */
export function checkFile(path: string, options?: FileOptions): Promise<CheckResult> {
  return assertReady().checkFile(path, options);
}

/** Returns which backend is currently active: 'native' or 'wasm'. Requires init() to have been called and awaited first. */
export function getBackend(): BackendType {
  return assertReady().getBackend();
}

export { isMdsError } from './types.js';
export type {
  BackendType,
  CheckResult,
  CompileMessagesResult,
  CompileOptions,
  CompileResult,
  FileOptions,
  InitOptions,
  Message,
  MdsError,
  MdsErrorSpan,
} from './types.js';
