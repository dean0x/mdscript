/**
 * Shared backend contract for @mdscript/mds.
 *
 * This module is the single source of truth for:
 *   1. The canonical set of method names each backend must expose.
 *   2. Runtime method-presence validation.
 *   3. Shallow per-call return-shape validation for compile/check results.
 *
 * Design constraints (AC-PERF-04):
 *   - Validation is shallow (top-level field types only) and O(1) — no array traversal.
 *   - NEVER iterate or index elements of warnings/dependencies/messages arrays.
 *     Array.isArray() is the only permitted operation on those fields.
 *   - Extra fields on a valid result are tolerated (zero behavior change).
 *   - Errors use the `mds::` code prefix consistent with the rest of the package.
 */

// ---------------------------------------------------------------------------
// Canonical method manifest
// ---------------------------------------------------------------------------

/**
 * Base backend methods — shared by the browser-safe MdsBaseBackend and all
 * Node.js backends. WASM module exports also belong here (plus scanImports).
 * compile and check are the only synchronous source-string operations.
 */
export const BASE_METHODS = ['compile', 'check'] as const;

/**
 * Node-only file-based methods. These extend BASE_METHODS on MdsNodeBackend
 * and on the native addon (NapiAddon).
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
 * Throws on the first missing or non-function member. The throw style matches
 * the historical validateWasmShape: a plain `Error` with an actionable message
 * naming the first missing member (not an `mds::` code error, because this
 * check fires before the backend is trusted to produce compiler errors).
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
 * The result shapes produced by backend methods.
 * Used as a discriminant for assertResultShape.
 *
 * 'compile' covers both MarkdownResult and MessagesResult — the function
 * branches on `result.kind` to validate the correct shape variant.
 */
export type ResultKind = 'compile' | 'check';

// ---------------------------------------------------------------------------
// Shallow return-shape validator (AC-PERF-04 — O(1), no element access)
// ---------------------------------------------------------------------------

/**
 * Assert that `result` has the correct top-level field types for the given `kind`.
 *
 * Validation is SHALLOW and O(1): only top-level field types are checked.
 * Array elements are NEVER accessed (DoS-safe). A Proxy wrapping warnings,
 * dependencies, or messages must observe zero numeric-index reads.
 *
 * For kind='compile', branches on `result.kind`:
 *   kind='markdown': assert output is string; assert messages is absent; assert
 *                    warnings and dependencies are arrays.
 *   kind='messages': assert messages is array; assert output is absent; assert
 *                    warnings and dependencies are arrays.
 *   unknown/missing kind: throws mds::invalid_backend_result.
 *
 * For kind='check':
 *   assert warnings is array.
 *
 * Extra fields on a valid result are silently tolerated.
 * Throws an Error with code `mds::invalid_backend_result` on mismatch.
 */
export function assertResultShape(result: unknown, kind: ResultKind): void {
  if (result === null || typeof result !== 'object') {
    throw makeShapeError(kind, 'result is not an object');
  }

  const r = result as Record<string, unknown>;

  switch (kind) {
    case 'compile': {
      // Branch on the discriminant kind field in the result.
      const resultKind = r['kind'];
      if (resultKind === 'markdown') {
        if (typeof r['output'] !== 'string') {
          throw makeShapeError(kind, `kind='markdown': "output" must be a string, got ${typeof r['output']}`);
        }
        if ('messages' in r) {
          throw makeShapeError(kind, `kind='markdown': inactive field "messages" must be absent`);
        }
        if (!Array.isArray(r['warnings'])) {
          throw makeShapeError(kind, `kind='markdown': "warnings" must be an array`);
        }
        if (!Array.isArray(r['dependencies'])) {
          throw makeShapeError(kind, `kind='markdown': "dependencies" must be an array`);
        }
      } else if (resultKind === 'messages') {
        if (!Array.isArray(r['messages'])) {
          throw makeShapeError(kind, `kind='messages': "messages" must be an array`);
        }
        if ('output' in r) {
          throw makeShapeError(kind, `kind='messages': inactive field "output" must be absent`);
        }
        if (!Array.isArray(r['warnings'])) {
          throw makeShapeError(kind, `kind='messages': "warnings" must be an array`);
        }
        if (!Array.isArray(r['dependencies'])) {
          throw makeShapeError(kind, `kind='messages': "dependencies" must be an array`);
        }
      } else {
        throw makeShapeError(
          kind,
          `unknown or missing "kind" field: expected "markdown" or "messages", got ${JSON.stringify(resultKind)}`,
        );
      }
      break;
    }
    case 'check': {
      if (!Array.isArray(r['warnings'])) {
        throw makeShapeError(kind, '"warnings" must be an array');
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
