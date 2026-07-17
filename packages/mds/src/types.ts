/**
 * Source Map v3 document appended to a {@link MarkdownResult} when
 * `sourceMap: true` is passed to a compile function.
 *
 * All bindings and the CLI produce compliant SMv3. The `file` key is absent
 * for binding results (caller-supplied entry identity is used as `sources[0]`);
 * the CLI writes the sidecar path. `sourcesContent` is present only when
 * `sourcesContent: true` was requested.
 *
 * @see https://sourcemaps.info/spec.html
 */
export interface SourceMapV3 {
  /** Always `3`. */
  version: 3;
  /** Source file paths. Binding results use the caller-supplied entry identity. */
  sources: string[];
  /**
   * Original source text for each entry in `sources`, index-aligned.
   * Present only when `sourcesContent: true` was requested.
   *
   * **Privacy warning**: embedding `sourcesContent` includes the full original
   * template text — including any hardcoded secrets or PII — in the map.
   * Only use `--embed-sources` / `sourcesContent: true` in trusted environments.
   */
  sourcesContent?: (string | null)[];
  /** Symbol names referenced by segments (typically empty for MDS). */
  names: string[];
  /** Base64-VLQ encoded segment mapping string. */
  mappings: string;
}

/** A single structured message produced by a `@message` block. */
export interface Message {
  /** The role string (e.g. `"system"`, `"user"`, `"assistant"`). */
  role: string;
  /** The rendered body text of the message (trimmed). */
  content: string;
}

/**
 * Compile result when the source compiled to Markdown output.
 * The `output` field carries the rendered string. The `messages` field is absent.
 */
export interface MarkdownResult {
  kind: 'markdown';
  /** Rendered Markdown output. */
  output: string;
  /** Non-fatal diagnostic messages produced during compilation. */
  warnings: string[];
  /** Absolute paths of every file transitively imported by the source. */
  dependencies: string[];
  /**
   * Source Map v3 document. Present only when `sourceMap: true` was passed
   * to the compile function. Absent (not `null`) when not requested.
   */
  sourceMap?: SourceMapV3;
}

/**
 * Compile result when the source compiled to structured chat messages.
 * The `messages` field carries the array. The `output` field is absent.
 */
export interface MessagesResult {
  kind: 'messages';
  /** Structured chat messages produced from `@message` blocks. */
  messages: Message[];
  /** Non-fatal diagnostic messages produced during compilation. */
  warnings: string[];
  /** Absolute paths of every file transitively imported by the source. */
  dependencies: string[];
}

/**
 * Discriminated union of all possible compile outputs.
 * Branch on `result.kind` to narrow to `MarkdownResult` or `MessagesResult`.
 */
export type CompileResult = MarkdownResult | MessagesResult;

/** Result of a successful check (validate-only) operation. */
export interface CheckResult {
  /** Non-fatal diagnostic messages produced during validation. */
  warnings: string[];
}

/** Options shared by compile and check operations. */
export interface CompileOptions {
  /** Runtime variables made available for interpolation in the template. */
  vars?: Record<string, unknown>;
  /**
   * When `true`, appends a {@link SourceMapV3} document to the result as
   * `result.sourceMap`. Ignored for `@message`-mode templates (no renderable
   * output means no mappable positions). Defaults to `false`.
   */
  sourceMap?: boolean;
  /**
   * When `true`, embeds the original source text in `sourceMap.sourcesContent`.
   * Requires `sourceMap: true`; passing `sourcesContent: true` without
   * `sourceMap: true` is an error (`mds::invalid_options`).
   *
   * **Privacy warning**: the embedded text contains the full template source,
   * including any hardcoded values. Only use in trusted environments.
   * Defaults to `false`.
   */
  sourcesContent?: boolean;
}

/** Options shared by file-based compile and check operations. */
export interface FileOptions {
  /** Runtime variables made available for interpolation in the template. */
  vars?: Record<string, unknown>;
  /**
   * When `true`, appends a {@link SourceMapV3} document to the result as
   * `result.sourceMap`. Ignored for `@message`-mode templates. Defaults to `false`.
   */
  sourceMap?: boolean;
  /**
   * When `true`, embeds the original source text in `sourceMap.sourcesContent`.
   * Requires `sourceMap: true`. Defaults to `false`.
   */
  sourcesContent?: boolean;
}

// ---------------------------------------------------------------------------
// Lint types
// ---------------------------------------------------------------------------

/** Byte-range span locating a lint diagnostic within its source file. */
export interface LintSpan {
  /** Byte offset from the start of the source string. */
  offset: number;
  /** Byte length of the diagnostic span. */
  length: number;
}

/** A single lint finding produced by a lint rule. */
export interface LintDiagnostic {
  /** Rule identifier (e.g. `"unused-variable"`, `"duplicate-import"`). */
  rule: string;
  /** Severity level: `"error"`, `"warn"`, or `"info"`. */
  severity: 'error' | 'warn' | 'info';
  /** Human-readable description of the finding. */
  message: string;
  /** Optional guidance on how to resolve the finding. */
  help?: string;
  /** Whether the lint engine can auto-fix this diagnostic (`--fix`). */
  fixable: boolean;
  /** Source location of the finding, if available. */
  span?: LintSpan;
}

/**
 * Severity value accepted for a per-rule override in `LintOptions.rules` /
 * `LintFileOptions.rules`. Mirrors the Rust `Severity` enum serialization:
 * `"off"` silences the rule; `"info"` / `"warn"` / `"error"` set its level.
 */
export type RuleSeverity = 'error' | 'warn' | 'info' | 'off';

/** All diagnostics for a single file in a lint result. */
export interface LintFileReport {
  /** Path or name of the linted file. */
  file: string;
  /** Diagnostics produced for this file. */
  diagnostics: LintDiagnostic[];
}

/**
 * Canonical lint result returned by all lint surfaces (CLI `--format json`,
 * napi, WASM, Python). All surfaces produce byte-identical JSON for the same
 * input and rules configuration.
 */
export interface LintResult {
  /** Schema version; always 1 in this release. */
  version: number;
  /** Per-file findings. Empty when the source is clean. */
  files: LintFileReport[];
  /**
   * `true` when the diagnostic count exceeded `MAX_DIAGNOSTICS` (1000) and
   * earlier diagnostics were dropped. Re-run after fixing to surface the rest.
   */
  truncated: boolean;
}

/** Options for source-string lint operations. */
export interface LintOptions {
  /** Runtime variables injected into the check gate (not the lint rules). */
  vars?: Record<string, unknown>;
  /** Per-rule severity overrides, e.g. `{ 'shadow-variable': 'warn' }`. */
  rules?: Record<string, RuleSeverity>;
  /**
   * Base directory for resolving `@import` directives in the source string.
   * Required when the source contains `@import` or `@extends`.
   * Ignored by the WASM backend (which cannot access the filesystem).
   */
  basePath?: string;
}

/** Options for file-based and virtual lint operations. */
export interface LintFileOptions {
  /** Runtime variables injected into the check gate (not the lint rules). */
  vars?: Record<string, unknown>;
  /** Per-rule severity overrides, e.g. `{ 'shadow-variable': 'warn' }`. */
  rules?: Record<string, RuleSeverity>;
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

/**
 * Browser-safe backend interface — compile/check/lint/lintVirtual/getBackend.
 * Does not include file operations (which require node:fs).
 */
export interface MdsBaseBackend {
  compile(source: string, options?: CompileOptions): CompileResult;
  check(source: string, options?: CompileOptions): CheckResult;
  /**
   * Lint an MDS source string.
   * Runs the check gate first; returns a LintResult with per-rule findings.
   */
  lint(source: string, options?: LintOptions): LintResult;
  /**
   * Lint a multi-module virtual filesystem.
   * Caller provides the full module map and entry key.
   */
  lintVirtual(modules: Record<string, string>, entry: string, options?: LintFileOptions): LintResult;
  getBackend(): BackendType;
}

/**
 * Full backend interface for Node.js environments.
 * Extends MdsBaseBackend with file-based compile/check/lint operations.
 */
export interface MdsNodeBackend extends MdsBaseBackend {
  compileFile(path: string, options?: FileOptions): Promise<CompileResult>;
  checkFile(path: string, options?: FileOptions): Promise<CheckResult>;
  /** Lint an MDS file, resolving @import directives relative to the file. */
  lintFile(path: string, options?: LintFileOptions): Promise<LintResult>;
}

/**
 * Backward-compatible alias. New code should prefer MdsNodeBackend.
 * @deprecated Use MdsNodeBackend directly.
 */
export type MdsBackend = MdsNodeBackend;

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
