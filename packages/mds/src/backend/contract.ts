/**
 * Shared backend contract for @mdscript/mds.
 *
 * This module is the single source of truth for:
 *   1. The canonical set of method names each backend must expose.
 *   2. Runtime method-presence validation (generalised from wasm.ts validateWasmShape).
 *   3. Shallow per-call return-shape validation for compile/check/compileMessages results.
 *
 * Design constraints (from PR-A1):
 *   - Validation is shallow (top-level field types only) and O(1) — no array traversal.
 *   - Extra fields on a valid result are tolerated (zero behavior change).
 *   - Errors use the `mds::` code prefix consistent with the rest of the package.
 *   - Adding a new method (e.g. PR-A2's compileMessagesFile) requires a single edit here.
 */

// ---------------------------------------------------------------------------
// Canonical method manifest
// ---------------------------------------------------------------------------

/**
 * Base backend methods — shared by the browser-safe MdsBaseBackend and all
 * Node.js backends. WASM module exports also belong here (plus scanImports).
 */
export const BASE_METHODS = ['compile', 'check', 'compileMessages'] as const;

/**
 * Node-only file-based methods. These extend BASE_METHODS on MdsNodeBackend
 * and on the native addon (NapiAddon).
 *
 * PR-A2 will add 'compileMessagesFile' here in exactly one place.
 */
export const NODE_METHODS = ['compileFile', 'checkFile'] as const;

/**
 * WASM module exports — BASE_METHODS plus the import scanner needed for
 * JS-side file resolution.
 */
export const WASM_EXPORTS = [...BASE_METHODS, 'scanImports'] as const;

export type BaseMethodName = (typeof BASE_METHODS)[number];
export type NodeMethodName = (typeof NODE_METHODS)[number];
export type WasmExportName = (typeof WASM_EXPORTS)[number];

// ---------------------------------------------------------------------------
// Method-presence validator
// ---------------------------------------------------------------------------

/**
 * Assert that every name in `methodNames` is a function on `obj`.
 *
 * Generalises the WASM shape check in wasm.ts so both backends go through the
 * same validation path. Throws on the first missing or non-function member.
 *
 * The throw style matches `validateWasmShape` in wasm.ts: a plain `Error` with
 * an actionable message naming the first missing member (not an `mds::` code
 * error, because this check fires before the backend is trusted to produce
 * compiler errors).
 *
 * @param obj         - The object to inspect (typically an addon or wasm module).
 * @param methodNames - Names that must exist as functions on `obj`.
 * @param context     - Short label for the error message, e.g. "WASM module" or "native addon".
 */
export function validateBackendMethods(
  obj: unknown,
  methodNames: readonly string[],
  context: string,
): void {
  const m = obj as Record<string, unknown>;
  for (const name of methodNames) {
    if (typeof m[name] !== 'function') {
      throw new Error(
        `@mdscript/mds: ${context} is missing required export "${name}".`,
      );
    }
  }
}

// ---------------------------------------------------------------------------
// Return-shape kinds
// ---------------------------------------------------------------------------

/**
 * The three result shapes produced by backend methods.
 * Used as a discriminant for assertResultShape.
 */
export type ResultKind = 'compile' | 'check' | 'compileMessages';

// ---------------------------------------------------------------------------
// Shallow return-shape validator
// ---------------------------------------------------------------------------

/**
 * Assert that `result` has the correct top-level field types for the given `kind`.
 *
 * Validation is SHALLOW and O(1): only top-level field *types* are checked.
 * Array elements are never iterated (DoS-safe).
 * Extra fields on a valid result are silently tolerated.
 *
 * Expected shapes:
 *   compile        → { output: string, warnings: array, dependencies: array }
 *   check          → { warnings: array }
 *   compileMessages → { messages: array, warnings: array, dependencies: array }
 *
 * Throws an Error with code `mds::invalid_backend_result` on mismatch.
 */
export function assertResultShape(result: unknown, kind: ResultKind): void {
  if (result === null || typeof result !== 'object') {
    throw makeShapeError(kind, 'result is not an object');
  }

  const r = result as Record<string, unknown>;

  switch (kind) {
    case 'compile': {
      if (typeof r['output'] !== 'string') {
        throw makeShapeError(kind, `"output" must be a string, got ${typeof r['output']}`);
      }
      if (!Array.isArray(r['warnings'])) {
        throw makeShapeError(kind, '"warnings" must be an array');
      }
      if (!Array.isArray(r['dependencies'])) {
        throw makeShapeError(kind, '"dependencies" must be an array');
      }
      break;
    }
    case 'check': {
      if (!Array.isArray(r['warnings'])) {
        throw makeShapeError(kind, '"warnings" must be an array');
      }
      break;
    }
    case 'compileMessages': {
      if (!Array.isArray(r['messages'])) {
        throw makeShapeError(kind, '"messages" must be an array');
      }
      if (!Array.isArray(r['warnings'])) {
        throw makeShapeError(kind, '"warnings" must be an array');
      }
      if (!Array.isArray(r['dependencies'])) {
        throw makeShapeError(kind, '"dependencies" must be an array');
      }
      break;
    }
    default: {
      // Exhaustiveness guard — TypeScript narrows `kind` to `never` here.
      const _exhaustive: never = kind;
      throw new Error(`@mdscript/mds: unknown result kind "${String(_exhaustive)}"`);
    }
  }
}

// ---------------------------------------------------------------------------
// Internal helper
// ---------------------------------------------------------------------------

function makeShapeError(kind: ResultKind, detail: string): Error {
  const err = new Error(
    `@mdscript/mds: backend returned unexpected shape for "${kind}": ${detail}`,
  ) as Error & { code: string };
  err.code = 'mds::invalid_backend_result';
  return err;
}
