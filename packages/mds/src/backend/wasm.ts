import type {
  BackendType,
  CheckResult,
  CompileOptions,
  CompileResult,
  FileOptions,
  InitOptions,
  MdsBackend,
} from '../types.js';
import { buildModulesMap } from '../util/module-scanner.js';

/**
 * Shape of the WASM module exports (built with --target nodejs).
 * The WASM module exports compile(source, options) and check(source, options).
 * options: { filename?, modules?, vars? }
 */
interface WasmModule {
  compile(source: string, options?: { filename?: string; modules?: Record<string, string>; vars?: Record<string, unknown> }): CompileResult;
  check(source: string, options?: { filename?: string; modules?: Record<string, string>; vars?: Record<string, unknown> }): CheckResult;
  scanImports(source: string): string[];
  default?: (input?: unknown) => Promise<void>;
}

let wasmModule: WasmModule | undefined;
// Promise cached BEFORE async work starts — prevents double-init race.
let initPromise: Promise<void> | null = null;
const MAX_INIT_RETRIES = 3;
let initFailures = 0;

/**
 * Reset all singleton state, optionally pre-seeding the failure counter.
 *
 * FOR TESTING ONLY — allows integration tests to exercise the retry-exhaustion
 * path without spawning a subprocess or driving N actual failures.
 *
 * @param failures - Number of failures to pre-seed. Defaults to 0 (full reset).
 *                   Pass MAX_INIT_RETRIES (3) to simulate exhaustion directly.
 * @internal
 */
export function _resetForTesting(failures = 0): void {
  wasmModule = undefined;
  initPromise = null;
  initFailures = failures;
}

/**
 * Initialize the WASM backend (idempotent singleton).
 *
 * Must be called before compile/check in browser environments.
 * In Node.js environments loaded via node.ts, this is called automatically.
 *
 * Concurrent calls share the same init promise. If init fails, the cached
 * promise is cleared so subsequent calls can retry, up to MAX_INIT_RETRIES
 * times. After that, every call throws immediately without re-attempting.
 */
export async function init(options?: InitOptions): Promise<void> {
  if (initPromise !== null) {
    return initPromise;
  }
  if (initFailures >= MAX_INIT_RETRIES) {
    throw new Error(
      `@mds/mds: WASM backend failed to initialize after ${MAX_INIT_RETRIES} attempts. Check that the WASM module is built and accessible.`,
    );
  }
  initPromise = _init(options).catch((err) => {
    // Reset so a subsequent call can retry after a transient failure.
    initFailures += 1;
    initPromise = null;
    throw err;
  });
  return initPromise;
}

/**
 * Attempt to load a single WASM candidate path.
 *
 * Returns the loaded module on success, or null if the candidate is not found
 * (MODULE_NOT_FOUND) or the loaded module does not match the expected shape.
 * Re-throws unexpected errors (OOM, corrupted WASM, init failures) so the
 * caller can surface them rather than silently discarding them.
 */
async function tryLoadCandidate(
  candidate: string,
  require: NodeRequire,
  wasmUrl: InitOptions['wasmUrl'],
): Promise<WasmModule | null> {
  let mod: unknown;
  try {
    mod = require(candidate);
  } catch (err) {
    if (isModuleNotFound(err)) return null;
    throw err;
  }

  // Validate the module shape at the boundary before trusting it as WasmModule.
  // compile and check are the minimum required exports; scanImports is used by
  // compileFile/checkFile but is intentionally not guarded here because the
  // current WASM build may omit it — a runtime error at the call site is
  // preferable to silently discarding an otherwise-valid module.
  if (
    typeof (mod as Record<string, unknown>).compile !== 'function' ||
    typeof (mod as Record<string, unknown>).check !== 'function'
  ) {
    return null;
  }

  const wasmMod = mod as WasmModule;
  // Browser targets expose a default() initializer; nodejs targets do not.
  if (typeof wasmMod.default === 'function') {
    await wasmMod.default(wasmUrl);
  }
  return wasmMod;
}

/** Returns true when an error indicates the required path simply does not exist. */
function isModuleNotFound(err: unknown): boolean {
  return (
    err instanceof Error &&
    (err as NodeJS.ErrnoException).code === 'MODULE_NOT_FOUND'
  );
}

/**
 * Internal initialization: locate and load the WASM module from known
 * candidate paths. Tries each path in order, stopping at the first success.
 * If a candidate triggers an unexpected error (not MODULE_NOT_FOUND), that
 * error is propagated immediately — callers should not swallow it.
 *
 * @internal
 */
async function _init(options?: InitOptions): Promise<void> {
  // In Node.js: load the built WASM module from the mds-wasm pkg directory.
  // The WASM is built with `wasm-pack build --target nodejs`.
  const { createRequire } = await import('node:module');
  const require = createRequire(import.meta.url);

  const candidates: readonly string[] = [
    // Workspace: pkg is built next to mds-wasm crate
    new URL('../../../../crates/mds-wasm/pkg/mds_wasm.js', import.meta.url).pathname,
    // Future npm package path: 'mds-wasm' is not yet published to npm and is
    // not listed in package.json dependencies. This candidate is forward-looking
    // — when the package is published, it will be resolvable here without code
    // changes. Until then, it is skipped silently (MODULE_NOT_FOUND).
    'mds-wasm',
  ];

  let lastError: Error | undefined;
  for (const candidate of candidates) {
    try {
      const mod = await tryLoadCandidate(candidate, require, options?.wasmUrl);
      if (mod !== null) {
        wasmModule = mod;
        return;
      }
    } catch (err) {
      // tryLoadCandidate re-throws non-MODULE_NOT_FOUND errors — capture the
      // last one so the diagnostic message can include it.
      lastError = err instanceof Error ? err : new Error(String(err));
    }
  }

  const cause = lastError !== undefined ? ` Caused by: ${lastError.message}` : '';
  throw new Error(
    `@mds/mds: failed to load WASM module. Build it first with: wasm-pack build crates/mds-wasm --target nodejs --out-dir pkg${cause}`,
  );
}

/** Return the initialized WASM module, or throw if init() has not completed. */
function assertInitialized(): WasmModule {
  if (wasmModule === undefined) {
    throw new Error(
      '@mds/mds: WASM backend not initialized. Call init() first.',
    );
  }
  return wasmModule;
}

/**
 * Deep-frozen default compile/check options for the common no-vars path.
 * Both the outer object and the nested modules map are frozen so that WASM
 * FFI cannot mutate shared state across calls.
 */
const DEFAULT_COMPILE_OPTS = Object.freeze({
  filename: 'input.mds',
  modules: Object.freeze({} as Record<string, string>),
});

/** Build the options object for compile/check, merging vars when present. */
function compileOpts(options?: CompileOptions) {
  const vars = options?.vars;
  return vars != null
    ? { filename: DEFAULT_COMPILE_OPTS.filename, modules: DEFAULT_COMPILE_OPTS.modules, vars }
    : DEFAULT_COMPILE_OPTS;
}

/** Build the options object for compileFile/checkFile, merging vars when present. */
function fileOpts(
  entryFilename: string,
  modules: Record<string, string>,
  options?: FileOptions,
) {
  const vars = options?.vars;
  return vars != null
    ? { filename: entryFilename, modules, vars }
    : { filename: entryFilename, modules };
}

/**
 * Create a WASM backend instance. Calls init() internally.
 */
export async function createWasmBackend(options?: InitOptions): Promise<MdsBackend> {
  await init(options);
  return {
    compile(source: string, options?: CompileOptions): CompileResult {
      const wasm = assertInitialized();
      return wasm.compile(source, compileOpts(options));
    },

    check(source: string, options?: CompileOptions): CheckResult {
      const wasm = assertInitialized();
      return wasm.check(source, compileOpts(options));
    },

    async compileFile(path: string, options?: FileOptions): Promise<CompileResult> {
      const wasm = assertInitialized();
      const { entryFilename, modules } = await buildModulesMap(path, (src) => wasm.scanImports(src));
      return wasm.compile(modules[entryFilename] ?? '', fileOpts(entryFilename, modules, options));
    },

    async checkFile(path: string, options?: FileOptions): Promise<CheckResult> {
      const wasm = assertInitialized();
      const { entryFilename, modules } = await buildModulesMap(path, (src) => wasm.scanImports(src));
      return wasm.check(modules[entryFilename] ?? '', fileOpts(entryFilename, modules, options));
    },

    getBackend(): BackendType {
      return 'wasm';
    },
  };
}
