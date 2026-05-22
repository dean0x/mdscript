import type {
  BackendType,
  MdsBackend,
  CompileResult,
  CheckResult,
  CompileOptions,
  FileOptions,
} from './types.js';

const rawBackend = process.env['MDS_BACKEND'];
const forceBackend: BackendType | undefined =
  rawBackend === 'native' || rawBackend === 'wasm' ? rawBackend : undefined;
if (rawBackend !== undefined && forceBackend === undefined) {
  console.warn(`@mds/mds: ignoring unknown MDS_BACKEND value "${rawBackend}"; expected "native" or "wasm"`);
}

let backend: MdsBackend;

if (forceBackend === 'wasm') {
  const { createWasmBackend } = await import('./backend/wasm.js');
  backend = await createWasmBackend();
} else {
  let nativeErr: unknown;
  try {
    const { createRequire } = await import('node:module');
    const require = createRequire(import.meta.url);
    const addon = require('mds-napi') as object;
    const { createNativeBackend } = await import('./backend/native.js');
    backend = createNativeBackend(addon as Parameters<typeof createNativeBackend>[0]);
  } catch (err) {
    nativeErr = err;
    if (forceBackend === 'native') {
      throw new Error(`MDS_BACKEND=native but native addon failed to load: ${String(err)}`);
    }
    try {
      console.warn('@mds/mds: native addon unavailable, falling back to WASM');
      const { createWasmBackend } = await import('./backend/wasm.js');
      backend = await createWasmBackend();
    } catch (wasmErr) {
      throw new Error(
        `@mds/mds: no backend available. Native: ${String(nativeErr)}. WASM: ${String(wasmErr)}`,
      );
    }
  }
}

/** Compile an MDS source string to Markdown. */
export function compile(source: string, options?: CompileOptions): CompileResult {
  return backend.compile(source, options);
}

/** Validate an MDS source string without rendering. */
export function check(source: string, options?: CompileOptions): CheckResult {
  return backend.check(source, options);
}

/** Compile an MDS file to Markdown, resolving `@import` directives relative to the file. */
export function compileFile(path: string, options?: FileOptions): Promise<CompileResult> {
  return backend.compileFile(path, options);
}

/** Validate an MDS file without rendering, resolving `@import` directives relative to the file. */
export function checkFile(path: string, options?: FileOptions): Promise<CheckResult> {
  return backend.checkFile(path, options);
}

/** Returns which backend is currently active: `'native'` or `'wasm'`. */
export function getBackend(): BackendType {
  return backend.getBackend();
}

/**
 * Pre-initialize the WASM module with a custom URL (browser-target WASM only).
 *
 * Backend selection in Node.js happens at import time via the `MDS_BACKEND`
 * environment variable — before user code can call `init()`. In Node.js, the
 * backend is already selected and loaded by the time this module is imported,
 * so `init()` is a no-op unless the WASM backend was chosen and you need to
 * supply a custom `wasmUrl` before the first compile call.
 *
 * The only meaningful option is `wasmUrl`, which is forwarded to the WASM
 * module initializer. Passing any other options has no effect.
 */
export { init } from './backend/wasm.js';
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
