export interface CompileResult {
  output: string;
  warnings: string[];
  dependencies: string[];
}

export interface CheckResult {
  warnings: string[];
}

export interface CompileOptions {
  vars?: Record<string, unknown>;
}

export interface FileOptions {
  vars?: Record<string, unknown>;
}

export interface MdsErrorSpan {
  offset: number;
  length: number;
  line?: number;
  column?: number;
}

export interface MdsError extends Error {
  code: string;
  help?: string;
  span?: MdsErrorSpan;
}

export type BackendType = 'native' | 'wasm';

export interface InitOptions {
  wasmUrl?: string | URL | Response | BufferSource;
}

export interface MdsBackend {
  compile(source: string, options?: CompileOptions): CompileResult;
  check(source: string, options?: CompileOptions): CheckResult;
  compileFile(path: string, options?: FileOptions): Promise<CompileResult>;
  checkFile(path: string, options?: FileOptions): Promise<CheckResult>;
  getBackend(): BackendType;
}

export function isMdsError(err: unknown): err is MdsError {
  return err instanceof Error && typeof (err as MdsError).code === 'string';
}
