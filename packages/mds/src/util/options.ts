import type { CompileOptions, FileOptions } from '../types.js';

/**
 * Build the `{ vars }` sub-object only when `options.vars` is defined and non-null.
 *
 * Used for check and checkFile where source-map options are not applicable.
 * When the caller passes no vars, omitting the key entirely avoids unnecessary
 * object creation and keeps the options shape minimal.
 */
export function varsOpt(options?: CompileOptions | FileOptions): { vars: Record<string, unknown> } | undefined {
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
