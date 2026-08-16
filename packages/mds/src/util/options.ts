import type {
  CheckFileOptions,
  CheckOptions,
  CompileOptions,
  FileOptions,
  LintFileOptions,
  LintOptions,
} from '../types.js';

// ── keysOf helper ──────────────────────────────────────────────────────────────

/**
 * Returns the keys of `T` as a readonly string array.
 *
 * The `witness` parameter must supply `true` for every key of `T` — this
 * binds the returned list to the interface at compile time. If `T` gains a
 * new key the witness literal must be updated; failing to do so is a compile
 * error, not a silent omission.
 *
 * @example
 * ```typescript
 * keysOf<LintOptions>({ basePath: true, vars: true, rules: true })
 * // → readonly ['basePath', 'vars', 'rules']
 * ```
 */
function keysOf<T extends object>(witness: Record<keyof T & string, true>): readonly string[] {
  return Object.keys(witness);
}

// ── Public method name union ───────────────────────────────────────────────────

/**
 * All public wrapper method names that accept an options object.
 *
 * The literal union is the compile-time guard for {@link assertKnownKeys}: any
 * call site that passes a method name not in this union is a type error, so a
 * newly-added method that is not yet registered is caught at build time rather
 * than silently disabling validation.
 */
export type MethodName =
  | 'compile'
  | 'check'
  | 'compileFile'
  | 'checkFile'
  | 'lint'
  | 'lintFile'
  | 'lintVirtual';

// ── Per-method key table ───────────────────────────────────────────────────────

/**
 * Allowed option keys per public wrapper method.
 *
 * Each list is derived via {@link keysOf} from the corresponding public option
 * interface, binding the table to the interface at compile time. When a method's
 * accepted options change, the witness object in the {@link keysOf} call must be
 * updated, or the call becomes a type error.
 *
 * CONSTRAINT: key ORDER within each witness literal is load-bearing — it
 * determines the `recognised keys are: …` list that {@link assertKnownKeys}
 * emits, which U-OV-14 byte-compares against the napi addon's output. Do not
 * reorder without updating the expected strings in that test.
 *
 * Reconciliation against napi option parsers (`crates/mds-napi/src/lib.rs`):
 * - `compile` / `check`: `basePath` is now in the public types (#180 fix) and in
 *   the key list; napi's `parse_compile_opts` / `parse_check_opts` accept it.
 * - `compileFile` / `checkFile`: `basePath` is NOT in the key list. It is passed
 *   through without wrapper interception so the backend can emit its own purpose-built
 *   error ("not valid for compileFile/checkFile; the base directory is derived from
 *   the file path"). See {@link BASEPATH_PASSTHROUGH} and issue #74.
 * - `lint`, `lintFile`, `lintVirtual`: key lists match napi exactly.
 */
const METHOD_KEYS: Readonly<Record<MethodName, readonly string[]>> = {
  compile:     keysOf<CompileOptions>({ basePath: true, vars: true, sourceMap: true, sourcesContent: true }),
  check:       keysOf<CheckOptions>({ basePath: true, vars: true }),
  compileFile: keysOf<FileOptions>({ vars: true, sourceMap: true, sourcesContent: true }),
  checkFile:   keysOf<CheckFileOptions>({ vars: true }),
  lint:        keysOf<LintOptions>({ basePath: true, vars: true, rules: true }),
  lintFile:    keysOf<LintFileOptions>({ vars: true, rules: true }),
  lintVirtual: keysOf<LintFileOptions>({ vars: true, rules: true }),
};

/**
 * Methods for which `basePath` is passed through to the backend without wrapper
 * interception (issue #74). The backend emits a purpose-built actionable error
 * for these methods ("not valid for compileFile/checkFile; the base directory is
 * derived from the file path") rather than the generic "unknown option key" format.
 *
 * KNOWN RESIDUAL (OD-5): napi also has purpose-built `basePath` messages for
 * `lintFile` ("not valid for lintFile; …") and `lintVirtual`, but those two methods
 * are deliberately NOT in this set. The wrapper intercepts them first and emits the
 * generic `unknown option key "basePath"; recognised keys are: vars, rules` form, so
 * the wrapper and napi messages diverge for that one input. This is intentional and
 * locked in by U-OV-7 and U-OV-13; the generic message still names the offending key
 * and is a hard error either way. Widening this set would change those two messages
 * and is deferred rather than bundled into #180/#215/#213.
 */
const BASEPATH_PASSTHROUGH: ReadonlySet<MethodName> = new Set<MethodName>([
  'compileFile',
  'checkFile',
]);

// ── Main validator ─────────────────────────────────────────────────────────────

/**
 * Assert that every key in `options` is in the allowed list for `method`.
 *
 * Uses the same message format as `format_unknown_keys_error` in
 * `crates/mds-core/src/options.rs`. For all methods except `compileFile` and
 * `checkFile`, the wrapper and napi produce byte-identical messages for the same
 * unknown key. For `compileFile` and `checkFile`, `basePath` is not intercepted
 * here — it is passed through so the backend can emit its own purpose-built error
 * (issue #74).
 *
 * The `method` parameter is typed as the {@link MethodName} literal union —
 * passing an unrecognised method name is a compile-time error, not a silent no-op.
 *
 * Throws `Error & { code: 'mds::invalid_options' }` — satisfies `isMdsError`.
 *
 * @param options - The caller-supplied options object (never null/undefined; guard before calling).
 * @param method  - Public method name, restricted to {@link MethodName} at compile time.
 */
export function assertKnownKeys(options: object, method: MethodName): void {
  // hasOwnProperty prevents prototype-chain resolution for callers that bypass
  // the MethodName compile-time union via a runtime cast (e.g. 'toString' as MethodName).
  if (!Object.prototype.hasOwnProperty.call(METHOD_KEYS, method)) return;
  const known = METHOD_KEYS[method];
  const unknowns = Object.keys(options).filter((k) => {
    // basePath is passed through for file-based methods so the backend emits its
    // own purpose-built error rather than this generic rejection (issue #74).
    if (BASEPATH_PASSTHROUGH.has(method) && k === 'basePath') return false;
    return !known.includes(k);
  });
  if (unknowns.length === 0) return;
  const recognised = known.join(', ');
  let message: string;
  if (unknowns.length === 1) {
    message = `unknown option key "${unknowns[0]}"; recognised keys are: ${recognised}`;
  } else {
    const listed = unknowns.map((k) => `"${k}"`).join(', ');
    message = `unknown option keys: ${listed}; recognised keys are: ${recognised}`;
  }
  const err = new Error(message) as Error & { code: string };
  err.code = 'mds::invalid_options';
  throw err;
}

// ── Per-surface option builders ────────────────────────────────────────────────
//
// D-TS-03 / D-TS-05: four typed builders, one per method-surface combination.
// Each builder returns `undefined` when no options are set (preserves the
// backend's fast path for no-options calls — avoids allocating an empty object
// on every invocation). The per-surface split ensures that adding a new field to
// a string-surface type (e.g. `CompileOptions`) cannot accidentally appear in the
// file-surface options forwarded to the backend.

/**
 * Return a new object containing only the keys from `keys` whose values in
 * `src` are non-null/undefined. Returns `undefined` when `src` is nullish or
 * every selected value is absent — so callers can use `result ?? undefined`
 * without allocating an empty object on every invocation (backend fast path).
 *
 * @internal
 */
function pickDefined<T extends object>(
  src: T | null | undefined,
  keys: readonly (keyof T)[],
): Partial<T> | undefined {
  if (src == null) return undefined;
  const defined = keys.filter(k => src[k] != null);
  if (defined.length === 0) return undefined;
  return Object.fromEntries(defined.map(k => [k, src[k]])) as Partial<T>;
}

/**
 * Build options for the string-source compile surface.
 * Picks `basePath`, `vars`, `sourceMap`, and `sourcesContent` from `CompileOptions`.
 *
 * D-TS-03: used by the native backend's `compile` method and by the WASM
 * backend's `compileOpts()` wrapper (after the WASM basePath guard fires).
 */
export function compileSrcOpt(options?: CompileOptions): Partial<CompileOptions> | undefined {
  return pickDefined(options, ['basePath', 'vars', 'sourceMap', 'sourcesContent']);
}

/**
 * Build options for the string-source check surface.
 * Picks `basePath` and `vars` from `CheckOptions`.
 *
 * D-TS-03: used by the native backend's `check` method. The WASM backend
 * guards against `basePath` before calling `checkOpts()`.
 */
export function checkSrcOpt(options?: CheckOptions): Partial<CheckOptions> | undefined {
  return pickDefined(options, ['basePath', 'vars']);
}

/**
 * Build options for the file-surface compile path.
 * Picks `vars`, `sourceMap`, and `sourcesContent` from `FileOptions`.
 * `basePath` is intentionally absent (D-TS-02).
 *
 * D-TS-03: used by the native backend's `compileFile` method and internally by
 * the WASM backend's `compileOpts()` / `fileOpts()` helpers (which deal with
 * `filename` and `modules` separately).
 */
export function fileCompileOpt(options?: FileOptions): Partial<FileOptions> | undefined {
  return pickDefined(options, ['vars', 'sourceMap', 'sourcesContent']);
}

/**
 * Build options for the file-surface check path.
 * Picks only `vars` from `CheckFileOptions`.
 * `basePath`, `sourceMap`, and `sourcesContent` are all intentionally absent.
 *
 * D-TS-03: used by the native backend's `checkFile` method.
 */
export function fileCheckOpt(options?: CheckFileOptions): { vars: Record<string, unknown> } | undefined {
  return options?.vars != null ? { vars: options.vars } : undefined;
}
