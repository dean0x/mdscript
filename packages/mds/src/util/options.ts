import type { CompileOptions, FileOptions } from '../types.js';

/**
 * Allowed option keys per public wrapper method.
 *
 * The lists mirror what each function actually forwards to the backend — keys
 * absent from the TS interface (e.g. `basePath` on `compile`) are excluded so
 * callers receive an actionable error instead of silent drops.
 *
 * Ordering matches the Rust `format_unknown_keys_error` known-key lists in
 * `crates/mds-napi/src/lib.rs` so wrapper- and backend-thrown error messages
 * share the same phrasing and key-listing format (PF-004 parallel-path enforcement).
 */
const METHOD_KEYS: Readonly<Record<string, readonly string[]>> = {
  compile: ['vars', 'sourceMap', 'sourcesContent'],
  check: ['vars'],
  compileFile: ['vars', 'sourceMap', 'sourcesContent'],
  checkFile: ['vars'],
  lint: ['basePath', 'vars', 'rules'],
  lintFile: ['vars', 'rules'],
  lintVirtual: ['vars', 'rules'],
};

/**
 * Assert that every key in `options` is in the allowed list for `method`.
 *
 * Mirrors `format_unknown_keys_error` from `crates/mds-core/src/options.rs` so
 * the error message phrasing and key-listing format are byte-identical to what
 * the native / WASM backends would throw for the same unknown key:
 *   - Single key:   `unknown option key "foo"; recognised keys are: vars, rules`
 *   - Multiple keys:`unknown option keys: "foo", "bar"; recognised keys are: vars, rules`
 *
 * Throws `Error & { code: 'mds::invalid_options' }` — satisfies `isMdsError`.
 *
 * @param options - The caller-supplied options object (never null/undefined; guard before calling).
 * @param method  - Public method name, e.g. `'compile'`.
 */
export function assertKnownKeys(options: object, method: string): void {
  const known = METHOD_KEYS[method];
  if (known == null) return;
  const unknowns = Object.keys(options).filter((k) => !known.includes(k));
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
