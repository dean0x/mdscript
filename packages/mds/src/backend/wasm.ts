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
import { varsOpt } from '../util/options.js';

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

async function _init(options?: InitOptions): Promise<void> {
  // In Node.js: load the built WASM module from the mds-wasm pkg directory.
  // The WASM is built with `wasm-pack build --target nodejs`.
  const { createRequire } = await import('node:module');
  const require = createRequire(import.meta.url);

  // Try to load from the napi package's sibling pkg directory.
  // Fallback paths for different install scenarios.
  const candidates = [
    // Workspace: pkg is built next to mds-wasm crate
    new URL('../../../../crates/mds-wasm/pkg/mds_wasm.js', import.meta.url).pathname,
    // npm install scenario: mds-wasm might be a separate package
    'mds-wasm',
  ];

  let loadError: unknown;
  for (const candidate of candidates) {
    try {
      const mod = require(candidate) as WasmModule;
      // For nodejs target, wasm-pack generates a CJS module that is already
      // initialized (no need to call default()). If it has a default export
      // that is a function, call it for browser targets.
      if (typeof mod.default === 'function') {
        await mod.default(options?.wasmUrl);
      }
      wasmModule = mod;
      return;
    } catch (e) {
      loadError = e;
    }
  }

  throw new Error(
    `@mds/mds: failed to load WASM module. Build it first with: wasm-pack build crates/mds-wasm --target nodejs --out-dir pkg. ${String(loadError)}`,
  );
}

function assertInitialized(): WasmModule {
  if (wasmModule === undefined) {
    throw new Error(
      '@mds/mds: WASM backend not initialized. Call init() first.',
    );
  }
  return wasmModule;
}

/** Default compile/check options for the common no-vars path — avoids per-call allocation. */
const DEFAULT_COMPILE_OPTS = Object.freeze({ filename: 'input.mds', modules: {} as Record<string, string> });

/**
 * Create a WASM backend instance. Calls init() internally.
 */
export async function createWasmBackend(options?: InitOptions): Promise<MdsBackend> {
  await init(options);
  return {
    compile(source: string, options?: CompileOptions): CompileResult {
      const wasm = assertInitialized();
      const vars = varsOpt(options);
      return wasm.compile(source, vars !== undefined ? { ...DEFAULT_COMPILE_OPTS, ...vars } : DEFAULT_COMPILE_OPTS);
    },

    check(source: string, options?: CompileOptions): CheckResult {
      const wasm = assertInitialized();
      const vars = varsOpt(options);
      return wasm.check(source, vars !== undefined ? { ...DEFAULT_COMPILE_OPTS, ...vars } : DEFAULT_COMPILE_OPTS);
    },

    async compileFile(path: string, options?: FileOptions): Promise<CompileResult> {
      const wasm = assertInitialized();
      const { entryFilename, modules } = await buildModulesMap(path, (src) => wasm.scanImports(src));
      return wasm.compile(modules[entryFilename] ?? '', {
        filename: entryFilename,
        modules,
        ...varsOpt(options),
      });
    },

    async checkFile(path: string, options?: FileOptions): Promise<CheckResult> {
      const wasm = assertInitialized();
      const { entryFilename, modules } = await buildModulesMap(path, (src) => wasm.scanImports(src));
      return wasm.check(modules[entryFilename] ?? '', {
        filename: entryFilename,
        modules,
        ...varsOpt(options),
      });
    },

    getBackend(): BackendType {
      return 'wasm';
    },
  };
}
