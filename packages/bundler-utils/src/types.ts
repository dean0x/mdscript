/**
 * Minimal interface for the MDS compiler API required by bundler plugins.
 * Both the native Node.js backend and the WASM backend satisfy this interface.
 *
 * Structural typing relationship: the real `@mdscript/mds` module namespace satisfies
 * this interface by duck typing — no explicit `implements` declaration is needed
 * because TypeScript's structural type system enforces compatibility at the
 * dynamic `import('@mdscript/mds')` call sites in transformer.ts.
 *
 * This interface intentionally omits `InitOptions` (the optional argument that
 * the real `init()` accepts) because bundler plugins always call `init()` with
 * no arguments. Widening the interface to include options the plugins never use
 * would couple bundler-utils to the full @mdscript/mds API surface unnecessarily.
 */
export interface MdsApi {
  /** Compile a file at the given absolute path and return the compiled output. */
  compileFile(path: string, options?: { vars?: Record<string, unknown> }): Promise<CompileResult>;
  /** Initialize the compiler backend. Must be awaited before calling compileFile. */
  init(): Promise<void>;
}

/** Result of a successful file compilation. */
export interface CompileResult {
  /** Rendered Markdown output string. */
  output: string;
  /** Non-fatal diagnostic messages produced during compilation. */
  warnings: string[];
  /** Absolute paths of every file transitively imported by the source. */
  dependencies: string[];
}

/** The JavaScript module code emitted by the transformer and associated metadata. */
export interface TransformResult {
  /** The generated JavaScript module source code. */
  code: string;
  /** Absolute paths of every file this transform depends on. */
  dependencies: string[];
  /** Non-fatal compiler warnings from this transform. */
  warnings: string[];
}

/** Options accepted by bundler plugin factories (Vite, Rollup, Webpack, etc.). */
export interface MdsPluginOptions {
  /** Runtime variables made available for interpolation in .mds templates. */
  vars?: Record<string, unknown>;
}

/**
 * A compiler error formatted for consumption by a bundler's error reporting API.
 * Matches the error shape expected by Vite, Rollup, and Webpack loaders.
 */
export interface FormattedError {
  /** Human-readable error message, possibly multi-line (includes help text). */
  message: string;
  /** The module id (file path) that produced this error. */
  id?: string;
  /** 1-based source line number of the error, if available. */
  line?: number;
  /** 1-based source column number of the error, if available. */
  column?: number;
  /** Source code frame for inline display, if available. */
  frame?: string;
}
