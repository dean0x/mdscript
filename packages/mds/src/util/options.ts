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
 * String keys of `T` whose non-nullable type is not `never`.
 *
 * Used to exclude `basePath?: never` marker fields from the {@link keysOf}
 * witness requirement. Those fields exist only to prevent structural
 * assignability of string-surface options to file-surface option types at
 * compile time; they carry no runtime meaning and must not appear in the
 * recognised-key list that {@link assertKnownKeys} enforces.
 */
type RuntimeKeys<T extends object> = {
  [K in keyof T & string]: [NonNullable<T[K]>] extends [never] ? never : K;
}[keyof T & string];

/**
 * Returns the keys of `T` as a readonly string array.
 *
 * The `witness` parameter must supply `true` for every runtime key of `T`
 * (i.e. every key whose non-nullable type is not `never`) — this binds the
 * returned list to the interface at compile time. If `T` gains a new key the
 * witness literal must be updated; failing to do so is a compile error, not a
 * silent omission.
 *
 * Keys typed as `?: never` (structural-subtyping blockers) are excluded from
 * the witness requirement via {@link RuntimeKeys}.
 *
 * @example
 * ```typescript
 * keysOf<LintOptions>({ basePath: true, vars: true, rules: true })
 * // → readonly ['basePath', 'vars', 'rules']
 * ```
 */
function keysOf<T extends object>(witness: Record<RuntimeKeys<T>, true>): readonly string[] {
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
 * - `compileFile` / `checkFile`: `basePath` is NOT in the key list. When a caller
 *   passes `basePath` on these methods the wrapper emits a purpose-built rejection via
 *   {@link BASEPATH_PASSTHROUGH}'s error factory — the backend never receives it.
 *   See {@link getBasePathError} and issue #74.
 * - `lint`, `lintFile`, `lintVirtual`: key lists match napi exactly.
 */
export const METHOD_KEYS: Readonly<Record<MethodName, readonly string[]>> = {
  compile:     keysOf<CompileOptions>({ basePath: true, vars: true, sourceMap: true, sourcesContent: true }),
  check:       keysOf<CheckOptions>({ basePath: true, vars: true }),
  compileFile: keysOf<FileOptions>({ vars: true, sourceMap: true, sourcesContent: true }),
  checkFile:   keysOf<CheckFileOptions>({ vars: true }),
  lint:        keysOf<LintOptions>({ basePath: true, vars: true, rules: true }),
  lintFile:    keysOf<LintFileOptions>({ vars: true, rules: true }),
  lintVirtual: keysOf<LintFileOptions>({ vars: true, rules: true }),
};

// ── basePath error factory for file-surface methods ────────────────────────────

/**
 * Build the `mds::invalid_options` error for `basePath` on file-surface methods.
 *
 * Message is byte-identical to napi `parse_file_opts` / `parse_check_file_opts`
 * (crates/mds-napi/src/lib.rs). The wrapper emits this error BEFORE backend
 * dispatch, so napi never produces this message for a public `compileFile` /
 * `checkFile` call. Nothing enforces the two strings staying in sync except
 * U-OV-27, which compares this message against the raw addon's at runtime.
 * Keep the two in lockstep; editing either alone fails that test.
 */
function makeFileBasePathError(): Error & { code: string } {
  const err = new Error(
    'option "basePath" is not valid for compileFile/checkFile; ' +
    'the base directory is derived from the file path',
  ) as Error & { code: string };
  err.code = 'mds::invalid_options';
  return err;
}

/**
 * Methods for which `basePath` must be rejected with a purpose-built error rather
 * than the generic "unknown option key" form.
 *
 * Each entry maps a method name to the factory that creates its rejection.
 * Using a Map (not a Set) means a new method can only be added when an error
 * factory is simultaneously provided — widening without a handler is a TypeScript
 * error on the Map literal, not a silent omission.
 *
 * The wrapper emits this error BEFORE dispatch (via {@link getBasePathError});
 * no backend ever receives a `basePath` on these methods. `assertKnownKeys` skips
 * the generic unknown-key path for `basePath` on these methods because they are
 * absent from METHOD_KEYS for those surfaces.
 *
 * KNOWN RESIDUAL (OD-5): napi also has purpose-built `basePath` messages for
 * `lintFile` ("not valid for lintFile; …") and `lintVirtual`, but those two methods
 * are deliberately NOT in this map. The wrapper intercepts them first and emits the
 * generic `unknown option key "basePath"; recognised keys are: vars, rules` form, so
 * the wrapper and napi messages diverge for that one input. This is intentional and
 * locked in by U-OV-7 and U-OV-13; the generic message still names the offending key
 * and is a hard error either way. Widening this map would change those messages and
 * is deferred rather than bundled into #180/#215/#213.
 */
const BASEPATH_PASSTHROUGH: ReadonlyMap<MethodName, () => Error & { code: string }> = new Map([
  ['compileFile', makeFileBasePathError],
  ['checkFile',   makeFileBasePathError],
] as const);

/**
 * Return the purpose-built `basePath` error for `method` if `options.basePath`
 * is non-null and `method` is in {@link BASEPATH_PASSTHROUGH}, or `undefined`
 * otherwise.
 *
 * Callers throw the returned error synchronously — the same channel as
 * {@link assertKnownKeys} — so that `try { compileFile(f, opts) } catch` captures
 * both unknown-key and basePath errors (U-OV-32 / U-OV-33). The structural benefit:
 * adding a new method to BASEPATH_PASSTHROUGH requires providing a factory via the
 * Map literal, ensuring the handler is co-located with the set membership.
 *
 * @param options - The caller-supplied options object (non-null).
 * @param method  - Public method name.
 */
export function getBasePathError(
  options: object,
  method: MethodName,
): (Error & { code: string }) | undefined {
  const factory = BASEPATH_PASSTHROUGH.get(method);
  if (!factory) return undefined;
  const basePath = (options as Record<string, unknown>)['basePath'];
  return basePath != null ? factory() : undefined;
}

// ── Main validator ─────────────────────────────────────────────────────────────

/**
 * Assert that every key in `options` is in the allowed list for `method`.
 *
 * Uses the same message format as `format_unknown_keys_error` in
 * `crates/mds-core/src/options.rs`. For all methods except `compileFile` and
 * `checkFile`, the wrapper and napi produce byte-identical messages for the same
 * unknown key. For `compileFile` and `checkFile`, `basePath` is absent from
 * METHOD_KEYS for those surfaces and is skipped here — the caller uses
 * {@link getBasePathError} to emit the purpose-built rejection (issue #74).
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
    // basePath is absent from METHOD_KEYS for file-surface methods; skip it here
    // so the generic "unknown option key" path is not taken. The caller uses
    // getBasePathError() to emit the purpose-built rejection (issue #74).
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

// ── Option forwarding ──────────────────────────────────────────────────────────

/**
 * Forward `options` to the backend, keeping only the keys listed in
 * {@link METHOD_KEYS} for `method`.
 *
 * This is the **single authoritative forwarding path** for all seven public
 * methods. When METHOD_KEYS is updated (e.g. a new key is added to a method's
 * option interface and its {@link keysOf} witness is updated), forwarding picks
 * it up automatically with no additional edit. The previous design used
 * independent per-surface builders (`compileSrcOpt`, `checkSrcOpt`, etc.) that
 * each hardcoded their own key array — those could drift from METHOD_KEYS without
 * a compile error, which is the root shape of PF-004 / #180.
 *
 * Returns `undefined` when `options` is nullish or every accepted key is absent,
 * preserving the backend no-options fast path (avoids allocating empty objects
 * on every call). D-TS-03 / D-TS-05.
 *
 * @param options - Caller-supplied options (null/undefined treated as "no options").
 * @param method  - Public method name; selects the key list from METHOD_KEYS.
 */
export function forwardOpts(
  options: object | null | undefined,
  method: MethodName,
): Record<string, unknown> | undefined {
  if (options == null) return undefined;
  const keys = METHOD_KEYS[method];
  const src = options as Record<string, unknown>;
  const out: Record<string, unknown> = {};
  let any = false;
  for (const k of keys) {
    const v = src[k];
    if (v != null) {
      // unavoidable: accumulating into Record<string,unknown> loses per-key value
      // types, but callers cast to their concrete options type at the call site.
      out[k] = v;
      any = true;
    }
  }
  return any ? out : undefined;
}
