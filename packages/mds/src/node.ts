import type {
  BackendType,
  MdsBaseBackend,
  MdsNodeBackend,
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
  console.warn(`@mds/mds: ignoring unknown MDS_BACKEND value "${rawBackend}"; expected "native" or "wasm"`);
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
  return {
    ...base,

    async compileFile(path: string, options?: FileOptions): Promise<CompileResult> {
      const { entryFilename, modules } = await buildModulesMap(path, (src) => wasmModule.scanImports(src));
      return wasmModule.compile(modules[entryFilename] ?? '', fileOpts(entryFilename, modules, options));
    },

    async checkFile(path: string, options?: FileOptions): Promise<CheckResult> {
      const { entryFilename, modules } = await buildModulesMap(path, (src) => wasmModule.scanImports(src));
      return wasmModule.check(modules[entryFilename] ?? '', fileOpts(entryFilename, modules, options));
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
    const addon = require('mds-napi') as object;
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

  console.warn('@mds/mds: native addon unavailable, falling back to WASM');
  try {
    backend = await loadWasmNodeBackend(options);
  } catch (wasmErr) {
    throw new Error(
      `@mds/mds: no backend available. Native: ${nativeResult.error.message}. WASM: ${String(wasmErr)}`,
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
      '@mds/mds: call await init() before using compile/check/compileFile/checkFile/getBackend',
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
  CompileOptions,
  CompileResult,
  FileOptions,
  InitOptions,
  MdsError,
  MdsErrorSpan,
} from './types.js';
