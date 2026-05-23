import type { CompileOptions, FileOptions } from '../types.js';

/**
 * Build the `{ vars }` sub-object only when `options.vars` is defined and non-null.
 *
 * Both native and WASM backends forward vars as a nested object. When the
 * caller passes no vars, omitting the key entirely avoids unnecessary
 * object creation and keeps the options shape minimal.
 */
export function varsOpt(options?: CompileOptions | FileOptions): { vars: Record<string, unknown> } | undefined {
  return options?.vars != null ? { vars: options.vars } : undefined;
}
