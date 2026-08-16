import type {
  BackendType,
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
} from '../types.js';
import { assertResultShape, validateBackendMethods, WASM_EXPORTS } from './contract.js';
import { forwardOpts } from '../util/options.js';

/**
 * Shape of the WASM module exports (built with wasm-pack).
 * The WASM module exports compile, check, lint, lintVirtual, and scanImports.
 * options shapes mirror the Rust wasm-bindgen signatures.
 *
 * Exported so callers can type-annotate pre-loaded modules passed to
 * createWasmBackend().
 */
export interface WasmModule {
  compile(source: string, options?: {
    filename?: string;
    modules?: Record<string, string>;
    vars?: Record<string, unknown>;
    sourceMap?: boolean;
    sourcesContent?: boolean;
  }): unknown;
  check(source: string, options?: { filename?: string; modules?: Record<string, string>; vars?: Record<string, unknown> }): unknown;
  /** Lint a source string. Returns the canonical lint JSON object. */
  lint(source: string, options?: {
    filename?: string;
    modules?: Record<string, string>;
    vars?: Record<string, unknown>;
    rules?: Record<string, string>;
  }): unknown;
  /** Lint a multi-module virtual filesystem. Returns the canonical lint JSON object. */
  lintVirtual(modules: Record<string, string>, entry: string, options?: {
    vars?: Record<string, unknown>;
    rules?: Record<string, string>;
  }): unknown;
  scanImports(source: string): string[];
  default?: (input?: unknown) => Promise<void>;
}

// ---------------------------------------------------------------------------
// Node.js init state
// ---------------------------------------------------------------------------

// Promise cached BEFORE async work starts — prevents double-init race.
let cachedNodePromise: Promise<WasmModule> | null = null;
const MAX_INIT_RETRIES = 3;
let nodeFailures = 0;

// ---------------------------------------------------------------------------
// Browser init state
// ---------------------------------------------------------------------------

let cachedBrowserPromise: Promise<WasmModule> | null = null;
const MAX_BROWSER_RETRIES = 3;
let browserFailures = 0;

// ---------------------------------------------------------------------------
// Test reset
// ---------------------------------------------------------------------------

/**
 * Reset all singleton state, optionally pre-seeding failure counters.
 *
 * FOR TESTING ONLY — allows integration tests to exercise the retry-exhaustion
 * path without spawning a subprocess or driving N actual failures.
 *
 * @param failures - Node.js failures to pre-seed. Defaults to 0 (full reset).
 *                   Pass MAX_INIT_RETRIES (3) to simulate Node.js exhaustion.
 * @param browserFailuresCount - Browser failures to pre-seed. Defaults to 0.
 *                   Pass MAX_BROWSER_RETRIES (3) to simulate browser exhaustion.
 * @internal
 */
export function _resetForTesting(failures = 0, browserFailuresCount = 0): void {
  cachedNodePromise = null;
  nodeFailures = failures;
  cachedBrowserPromise = null;
  browserFailures = browserFailuresCount;
}

// ---------------------------------------------------------------------------
// Shape validation helper
// ---------------------------------------------------------------------------

/**
 * Attempt to load a single WASM candidate path (Node.js only).
 *
 * Returns the loaded module on success, or null if the candidate is not found
 * (MODULE_NOT_FOUND). Throws if the loaded module does not match the expected
 * WasmModule shape (missing compile/check/scanImports). Re-throws unexpected
 * errors (OOM, corrupted WASM, init failures) so the caller can surface them
 * rather than silently discarding them.
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

  // Validate the module shape before trusting it as WasmModule.
  // validateWasmShape throws with an actionable message naming the missing member,
  // which the caller captures as lastError for the final diagnostic.
  validateWasmShape(mod);

  // Browser targets expose a default() initializer; nodejs targets do not.
  if (typeof mod.default === 'function') {
    await mod.default(wasmUrl);
  }
  return mod;
}

/** Returns true when an error indicates the required path simply does not exist. */
function isModuleNotFound(err: unknown): boolean {
  return (
    err instanceof Error &&
    (err as NodeJS.ErrnoException).code === 'MODULE_NOT_FOUND'
  );
}

/**
 * Validate that a dynamically loaded module matches the WasmModule shape.
 *
 * Delegates to the shared validateBackendMethods from contract.ts so the
 * canonical method manifest (WASM_EXPORTS) is the single source of truth.
 *
 * Exported so tests can exercise shape validation directly without going
 * through the full WASM init path.
 */
export function validateWasmShape(mod: unknown): asserts mod is WasmModule {
  validateBackendMethods(
    mod,
    WASM_EXPORTS,
    'WASM module',
  );
}

// ---------------------------------------------------------------------------
// Node.js init
// ---------------------------------------------------------------------------

/**
 * Initialize the WASM backend for Node.js environments.
 *
 * Idempotent — safe to call multiple times. Concurrent calls share the same
 * promise. If initialization fails, the cached promise is cleared so subsequent
 * calls can retry, up to MAX_INIT_RETRIES times. After exhaustion, every call
 * throws immediately without re-attempting.
 *
 * Does NOT import node:module at module scope — the import is deferred to this
 * async function so that the module can be imported in environments where
 * node:module is unavailable.
 */
export async function initWasmNode(options?: InitOptions): Promise<WasmModule> {
  if (cachedNodePromise !== null) {
    return cachedNodePromise;
  }
  if (nodeFailures >= MAX_INIT_RETRIES) {
    throw new Error(
      `@mdscript/mds: WASM backend failed to initialize after ${MAX_INIT_RETRIES} attempts. Check that the WASM module is built and accessible.`,
    );
  }
  cachedNodePromise = _initNode(options).catch((err) => {
    // Reset so a subsequent call can retry after a transient failure.
    nodeFailures += 1;
    cachedNodePromise = null;
    throw err;
  });
  return cachedNodePromise;
}

/**
 * Internal Node.js initialization: locate and load the WASM module from known
 * candidate paths. Tries each path in order, stopping at the first success.
 *
 * @internal
 */
async function _initNode(options?: InitOptions): Promise<WasmModule> {
  // Deferred import — not at module scope so browser-targeting bundlers can
  // tree-shake this function and avoid bundling node:module.
  const { createRequire } = await import('node:module');
  const require = createRequire(import.meta.url);

  const candidates: readonly string[] = [
    // Workspace dev path: pkg is built next to the mds-wasm crate. Tried first so
    // local development and CI use the freshly built artifact without a publish.
    new URL('../../../../crates/mds-wasm/pkg/mds_wasm.js', import.meta.url).pathname,
    // Published package: resolved for installed consumers via @mdscript/mds's
    // dependency on @mdscript/mds-wasm. Skipped silently (MODULE_NOT_FOUND) in dev
    // when only the workspace path above is present.
    '@mdscript/mds-wasm',
  ];

  let lastError: Error | undefined;
  for (const candidate of candidates) {
    try {
      const mod = await tryLoadCandidate(candidate, require, options?.wasmUrl);
      if (mod !== null) {
        return mod;
      }
    } catch (err) {
      // tryLoadCandidate re-throws non-MODULE_NOT_FOUND errors — capture the
      // last one so the diagnostic message can include it.
      lastError = err instanceof Error ? err : new Error(String(err));
    }
  }

  const cause = lastError !== undefined ? ` Caused by: ${lastError.message}` : '';
  throw new Error(
    `@mdscript/mds: failed to load WASM module. Build it first with: wasm-pack build crates/mds-wasm --target nodejs --out-dir pkg${cause}`,
  );
}

// ---------------------------------------------------------------------------
// Browser init
// ---------------------------------------------------------------------------

/**
 * Initialize the WASM backend for browser environments.
 *
 * Accepts a wasmUrl from InitOptions (required in browser unless the WASM
 * module is bundled with a default export). Does NOT import node:module or
 * node:fs — safe for browser/edge bundlers.
 *
 * Concurrent calls share the same promise. If initialization fails, the cached
 * promise is cleared so the next call can retry (simpler than Node.js — no
 * candidate list, so exhaustion means the wasmUrl itself is wrong).
 */
export async function initWasmBrowser(options?: InitOptions): Promise<WasmModule> {
  if (cachedBrowserPromise !== null) {
    return cachedBrowserPromise;
  }
  if (browserFailures >= MAX_BROWSER_RETRIES) {
    throw new Error(
      `@mdscript/mds: WASM browser backend failed to initialize after ${MAX_BROWSER_RETRIES} attempts. ` +
      `Ensure '@mdscript/mds-wasm' is bundled or provide a valid wasmUrl option.`,
    );
  }
  cachedBrowserPromise = _initBrowser(options).catch((err) => {
    // Reset so a subsequent call can retry after a transient failure.
    browserFailures += 1;
    cachedBrowserPromise = null;
    throw err;
  });
  return cachedBrowserPromise;
}

/**
 * Internal browser initialization: dynamically import the WASM module and
 * call its default initializer with the wasmUrl.
 *
 * @internal
 */
async function _initBrowser(options?: InitOptions): Promise<WasmModule> {
  // Dynamic import — bundler resolves '@mdscript/mds-wasm' or the caller provides
  // the module. In browser environments, the bundler inlines the WASM module at
  // build time. TypeScript cannot resolve the package's browser export at compile
  // time, so the shape is validated with validateWasmShape below.
  let imported: unknown;
  try {
    // eslint-disable-next-line @typescript-eslint/ban-ts-comment
    // @ts-ignore — '@mdscript/mds-wasm' is resolved by the bundler at build time
    imported = await import('@mdscript/mds-wasm');
  } catch (err) {
    throw new Error(
      `@mdscript/mds: failed to load WASM module in browser environment. ` +
      `Ensure '@mdscript/mds-wasm' is bundled or provide a wasmUrl option. Caused by: ${String(err)}`,
    );
  }
  // validateWasmShape throws a descriptive error naming the missing member —
  // no need to catch here; its errors are already actionable.
  validateWasmShape(imported);
  const wasmMod = imported;

  if (typeof wasmMod.default !== 'function') {
    throw new Error(
      '@mdscript/mds: WASM module missing default() initializer. ' +
      'Build with: wasm-pack build crates/mds-wasm --target web --out-dir pkg',
    );
  }

  try {
    await wasmMod.default(options?.wasmUrl);
  } catch (err) {
    // Detect CSP/fetch errors and provide actionable guidance.
    const msg = err instanceof Error ? err.message : String(err);
    if (
      msg.includes('Content Security Policy') ||
      msg.includes('unsafe-eval') ||
      msg.includes('wasm-unsafe-eval') ||
      msg.includes('fetch')
    ) {
      throw new Error(
        `@mdscript/mds: WASM initialization blocked — check your Content Security Policy. ` +
        `Add 'wasm-unsafe-eval' to script-src. Original: ${msg}`,
      );
    }
    throw err;
  }

  return wasmMod;
}

// ---------------------------------------------------------------------------
// Sync factory
// ---------------------------------------------------------------------------

/**
 * Deep-frozen default compile/check options for the common no-vars path.
 * Both the outer object and the nested modules map are frozen so that WASM
 * FFI cannot mutate shared state across calls.
 */
const DEFAULT_COMPILE_OPTS = Object.freeze({
  filename: 'input.mds',
  modules: Object.freeze({} as Record<string, string>),
});

/**
 * Extended options accepted by the internal WASM `compile` entry point.
 *
 * D-TS-06: extends `Omit<CompileOptions, 'basePath'>`, not `CompileOptions`
 * directly. After `CompileOptions` gained `basePath` (fix #180), inheriting it
 * here would silently widen the internal WASM input type. The WASM module has no
 * filesystem access, so `basePath` is explicitly excluded from this internal type.
 * The basePath guard in `createWasmBackend.compile/check/lint` fires BEFORE this
 * type is used, so no runtime leak is possible (avoids PF-004).
 *
 * `filename` and `modules` are WASM-internal: callers who go through
 * `createWasmBackend` directly (e.g. `wrapWithFileOps`, CF-SM1 test) and supply
 * these keys explicitly get the expected WASM behaviour. No public method accepts them.
 */
interface _WasmCompileInput extends Omit<CompileOptions, 'basePath'> {
  filename?: string;
  modules?: Record<string, string>;
}

/** Build the options object for compile, merging vars and source-map options when present. */
function compileOpts(
  options?: _WasmCompileInput,
): {
  filename: string;
  modules: Record<string, string>;
  vars?: Record<string, unknown>;
  sourceMap?: boolean;
  sourcesContent?: boolean;
} {
  // D-TS-06: forward only the compileFile-surface keys (vars, sourceMap, sourcesContent)
  // via METHOD_KEYS.compileFile. basePath is excluded from _WasmCompileInput and the
  // caller guards against it before reaching here; forwardOpts cannot include it.
  const extra = forwardOpts(options, 'compileFile');
  const filename = options?.filename ?? DEFAULT_COMPILE_OPTS.filename;
  const modules = options?.modules ?? DEFAULT_COMPILE_OPTS.modules;
  if (extra == null && filename === DEFAULT_COMPILE_OPTS.filename && modules === DEFAULT_COMPILE_OPTS.modules) {
    return DEFAULT_COMPILE_OPTS;
  }
  return { filename, modules, ...extra };
}

/** Build the options object for check, merging vars when present. */
function checkOpts(
  options?: CheckOptions,
): { filename: string; modules: Record<string, string>; vars?: Record<string, unknown> } {
  // D-TS-06: forward only the check-surface keys via METHOD_KEYS.check, which is
  // derived from CheckOptions — the same interface as this function's parameter.
  // METHOD_KEYS.check includes basePath, but forwardOpts only forwards keys whose
  // value is != null; the caller already threw when basePath was non-null, so
  // basePath is never included in the forwarded object. Using 'check' (not
  // 'checkFile') keeps the key list tied to CheckOptions so a future key added to
  // that interface automatically propagates here — eliminating the PF-004 / #180
  // topology where METHOD_KEYS.check gained a key but this call forwarded via a
  // different surface's list and silently dropped it.
  const extra = forwardOpts(options, 'check');
  return extra != null
    ? { filename: DEFAULT_COMPILE_OPTS.filename, modules: DEFAULT_COMPILE_OPTS.modules, ...extra }
    : DEFAULT_COMPILE_OPTS;
}

/** Build the options object for compileFile, merging vars and source-map options when present. */
export function fileOpts(
  entryFilename: string,
  modules: Record<string, string>,
  options?: FileOptions,
): {
  filename: string;
  modules: Record<string, string>;
  vars?: Record<string, unknown>;
  sourceMap?: boolean;
  sourcesContent?: boolean;
} {
  // Use forwardOpts for the user-visible keys (vars, sourceMap, sourcesContent)
  // so METHOD_KEYS.compileFile is the single authoritative source for what is forwarded.
  const extra = forwardOpts(options, 'compileFile');
  return extra != null
    ? { filename: entryFilename, modules, ...extra }
    : { filename: entryFilename, modules };
}

/**
 * Throw `mds::invalid_options` when a caller passes `basePath` to a WASM-backend
 * string-surface method (compile, check, lint).
 *
 * OD-1: the WASM backend has no filesystem access, so a non-null `basePath` cannot
 * be honoured. Throwing instead of silently ignoring surfaces the misconfiguration
 * rather than linting / compiling a partially-resolved module graph (avoids PF-004).
 *
 * AC-P3-10: the message MUST NOT contain "filename" or "modules" (internal WASM
 * keys the public API never exposes). Verified by the PF-013-controlled negative
 * assertion in U-WB22: the raw wasmModule.compile call DOES contain those strings,
 * proving the negative assertion can detect the leak if the guard is removed.
 */
function throwWasmBasePathError(): never {
  const err = new Error(
    'option "basePath" is not supported by the WASM backend (no filesystem access); ' +
    'set MDS_BACKEND=native to use the native backend',
  ) as Error & { code: string };
  err.code = 'mds::invalid_options';
  throw err;
}

/**
 * Create a WASM backend instance from a pre-initialized WasmModule.
 *
 * Synchronous factory — mirrors createNativeBackend(addon) pattern.
 * Returns MdsBaseBackend (compile, check, lint, lintVirtual, getBackend)
 * without file operations.
 * File operations (compileFile, checkFile, lintFile) are added in node.ts
 * via wrapWithFileOps().
 */
export function createWasmBackend(wasmModule: WasmModule): MdsBaseBackend {
  return {
    compile(source: string, options?: CompileOptions): CompileResult {
      // OD-1: reject basePath rather than silently ignoring it (avoids PF-004).
      if (options?.basePath != null) throwWasmBasePathError();
      const result: unknown = wasmModule.compile(source, compileOpts(options as _WasmCompileInput));
      assertResultShape(result, 'compile');
      return result as CompileResult;
    },

    check(source: string, options?: CheckOptions): CheckResult {
      // OD-1: reject basePath rather than silently ignoring it.
      if (options?.basePath != null) throwWasmBasePathError();
      const result: unknown = wasmModule.check(source, checkOpts(options));
      assertResultShape(result, 'check');
      return result as CheckResult;
    },

    lint(source: string, options?: LintOptions): LintResult {
      // OD-1: reject basePath rather than silently ignoring it.
      if (options?.basePath != null) throwWasmBasePathError();
      // forwardOpts uses METHOD_KEYS.lint (vars, rules only after the guard above
      // ensures basePath is null/undefined and thus excluded by != null check).
      const result: unknown = wasmModule.lint(
        source,
        forwardOpts(options, 'lint'),
      );
      assertResultShape(result, 'lint');
      return result as LintResult;
    },

    lintVirtual(
      modules: Record<string, string>,
      entry: string,
      options?: LintFileOptions,
    ): LintResult {
      // forwardOpts uses METHOD_KEYS.lintVirtual (vars, rules).
      const result: unknown = wasmModule.lintVirtual(
        modules,
        entry,
        forwardOpts(options, 'lintVirtual'),
      );
      assertResultShape(result, 'lint');
      return result as LintResult;
    },

    getBackend(): BackendType {
      return 'wasm';
    },
  };
}
