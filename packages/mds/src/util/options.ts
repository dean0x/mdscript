import type { CompileOptions, CheckOptions, FileOptions, LintOptions, LintFileOptions } from '../types.js';

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

// ── Internal backend option shapes ────────────────────────────────────────────

// These represent what napi's option parsers accept — wider than the public
// TypeScript interfaces for compile and check, which intentionally omit basePath
// (open issue #180). The backend (parse_compile_opts / parse_check_opts) accepts
// basePath; the public TS types do not expose it yet.

/** Backend-accepted options for `compile` — superset of public {@link CompileOptions}. */
interface _CompileBackendOpts extends CompileOptions {
  basePath?: string;
}

/** Backend-accepted options for `check` — superset of public {@link CheckOptions}. */
interface _CheckBackendOpts extends CheckOptions {
  basePath?: string;
}

// ── Per-method key table ───────────────────────────────────────────────────────

/**
 * Allowed option keys per public wrapper method.
 *
 * Each list is derived via {@link keysOf} from the corresponding backend option
 * interface, binding the table to the interface at compile time. When a method's
 * accepted options change, the witness object in the {@link keysOf} call must be
 * updated, or the call becomes a type error.
 *
 * Reconciliation against napi option parsers (`crates/mds-napi/src/lib.rs`):
 * - `compile`/`check`: `basePath` added — napi's `parse_compile_opts` /
 *   `parse_check_opts` accept it; the public TS types do not yet expose it
 *   (open issue #180).
 * - `compileFile`/`checkFile`: `basePath` is NOT in the key list; instead it is
 *   passed through to the backend without wrapper interception so napi's purpose-built
 *   error fires ("not valid for compileFile/checkFile; the base directory is derived
 *   from the file path"). See {@link BASEPATH_PASSTHROUGH} and issue #74.
 * - `lint`, `lintFile`, `lintVirtual`: key lists match napi exactly.
 */
const METHOD_KEYS: Readonly<Record<MethodName, readonly string[]>> = {
  compile:     keysOf<_CompileBackendOpts>({ basePath: true, vars: true, sourceMap: true, sourcesContent: true }),
  check:       keysOf<_CheckBackendOpts>({ basePath: true, vars: true }),
  compileFile: keysOf<FileOptions>({ vars: true, sourceMap: true, sourcesContent: true }),
  checkFile:   keysOf<CheckOptions>({ vars: true }),
  lint:        keysOf<LintOptions>({ basePath: true, vars: true, rules: true }),
  lintFile:    keysOf<LintFileOptions>({ vars: true, rules: true }),
  lintVirtual: keysOf<LintFileOptions>({ vars: true, rules: true }),
};

/**
 * Methods for which `basePath` is passed through to the backend without wrapper
 * interception (issue #74). The backend emits a purpose-built actionable error
 * for these methods ("not valid for compileFile/checkFile; the base directory is
 * derived from the file path") rather than the generic "unknown option key" format.
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
 * (issue #74; open issue #180).
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

/**
 * Build the `{ vars }` sub-object only when `options.vars` is defined and non-null.
 *
 * Used for check and checkFile where source-map options are not applicable.
 * When the caller passes no vars, omitting the key entirely avoids unnecessary
 * object creation and keeps the options shape minimal.
 */
export function varsOpt(
  options?: { vars?: Record<string, unknown> },
): { vars: Record<string, unknown> } | undefined {
  return options?.vars != null ? { vars: options.vars } : undefined;
}

/**
 * Build the options object for compile/compileFile, forwarding vars,
 * sourceMap, and sourcesContent when present and non-null.
 *
 * Returns `undefined` when no options are set so the backend receives no
 * options argument (avoids allocating a needless empty object on the hot path).
 */
export function compileOpt(
  options?: CompileOptions | FileOptions,
): { vars?: Record<string, unknown>; sourceMap?: boolean; sourcesContent?: boolean } | undefined {
  if (options == null) return undefined;
  const out: { vars?: Record<string, unknown>; sourceMap?: boolean; sourcesContent?: boolean } = {};
  if (options.vars != null) out.vars = options.vars;
  if ((options as CompileOptions).sourceMap != null) out.sourceMap = (options as CompileOptions).sourceMap;
  if ((options as CompileOptions).sourcesContent != null) out.sourcesContent = (options as CompileOptions).sourcesContent;
  return Object.keys(out).length > 0 ? out : undefined;
}
