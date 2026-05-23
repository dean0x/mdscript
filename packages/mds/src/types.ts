/** Result of a successful compile operation. */
export interface CompileResult {
  /** Rendered Markdown output. */
  output: string;
  /** Non-fatal diagnostic messages produced during compilation. */
  warnings: string[];
  /** Absolute paths of every file transitively imported by the source. */
  dependencies: string[];
}

/** Result of a successful check (validate-only) operation. */
export interface CheckResult {
  /** Non-fatal diagnostic messages produced during validation. */
  warnings: string[];
}

/** Options shared by compile and check operations. */
export interface CompileOptions {
  /** Runtime variables made available for interpolation in the template. */
  vars?: Record<string, unknown>;
}

/** Options shared by file-based compile and check operations. */
export interface FileOptions {
  /** Runtime variables made available for interpolation in the template. */
  vars?: Record<string, unknown>;
}

/** Source location of a compiler error. */
export interface MdsErrorSpan {
  /** Byte offset from the start of the source string. */
  offset: number;
  /** Byte length of the error span. */
  length: number;
  /** 1-based line number of the error, if available. */
  line?: number;
  /** 1-based column number of the error, if available. */
  column?: number;
}

/** Error thrown by the MDS compiler. Use `isMdsError` to identify these. */
export interface MdsError extends Error {
  /** Namespaced error code, e.g. `"mds::undefined_variable"`. */
  code: string;
  /** Optional human-readable guidance on how to fix the error. */
  help?: string;
  /** Source location of the error, if available. */
  span?: MdsErrorSpan;
}

/** Discriminant for the active compiler backend. */
export type BackendType = 'native' | 'wasm';

/** Options for explicit WASM backend initialization. */
export interface InitOptions {
  /**
   * Override the WASM module source:
   * - `string` / `URL` — fetched from the network
   * - `Response` — pre-fetched `fetch()` response
   * - `BufferSource` — pre-loaded bytes (e.g. from a bundler asset)
   */
  wasmUrl?: string | URL | Response | BufferSource;
}

/** Internal interface implemented by each backend adapter. */
export interface MdsBackend {
  compile(source: string, options?: CompileOptions): CompileResult;
  check(source: string, options?: CompileOptions): CheckResult;
  compileFile(path: string, options?: FileOptions): Promise<CompileResult>;
  checkFile(path: string, options?: FileOptions): Promise<CheckResult>;
  getBackend(): BackendType;
}

/**
 * Type guard that identifies errors thrown by the MDS compiler.
 * Returns `true` when `err` is an `Error` with a string `code` property
 * starting with `'mds::'`.
 */
export function isMdsError(err: unknown): err is MdsError {
  return (
    err instanceof Error &&
    typeof (err as MdsError).code === 'string' &&
    (err as MdsError).code.startsWith('mds::')
  );
}
