import type {
  BackendType,
  CheckFileOptions,
  CheckOptions,
  CheckResult,
  CompileOptions,
  CompileResult,
  FileOptions,
  LintFileOptions,
  LintOptions,
  LintResult,
  MdsNodeBackend,
} from '../types.js';
import { compileSrcOpt, checkSrcOpt, fileCompileOpt, fileCheckOpt } from '../util/options.js';
import { assertResultShape, validateBackendMethods, BASE_METHODS, NODE_METHODS } from './contract.js';

/** Options forwarded to the napi addon for source-string compile (accepts basePath). */
type NapiCompileOpts = {
  basePath?: string;
  vars?: Record<string, unknown>;
  sourceMap?: boolean;
  sourcesContent?: boolean;
};

/** Options forwarded to the napi addon for source-string check (accepts basePath). */
type NapiCheckOpts = {
  basePath?: string;
  vars?: Record<string, unknown>;
};

/** Options forwarded to the napi addon for compileFile (no basePath). */
type NapiFileCompileOpts = {
  vars?: Record<string, unknown>;
  sourceMap?: boolean;
  sourcesContent?: boolean;
};

/** Options forwarded to the napi addon for source-string lint (accepts basePath). */
type NapiLintOpts = { basePath?: string; vars?: Record<string, unknown>; rules?: Record<string, string> };
/** Options forwarded to the napi addon for file-based and virtual lint. */
type NapiLintFileOpts = { vars?: Record<string, unknown>; rules?: Record<string, string> };

/**
 * Shape of the napi addon exports.
 * compile/check accept { basePath?, vars?, sourceMap?, sourcesContent? } for string sources.
 * compileFile/checkFile accept { vars?, sourceMap?, sourcesContent? } for file paths.
 * lint accepts { basePath?, vars?, rules? }; lintFile/lintVirtual accept only
 * { vars?, rules? } — napi rejects `basePath` on both (parse_lint_file_opts /
 * parse_lint_virtual_opts in crates/mds-napi/src/lib.rs).
 */
interface NapiAddon {
  compile(source: string, opts?: NapiCompileOpts): unknown;
  check(source: string, opts?: NapiCheckOpts): unknown;
  compileFile(path: string, opts?: NapiFileCompileOpts): unknown;
  checkFile(path: string, opts?: { vars?: Record<string, unknown> }): unknown;
  lint(source: string, opts?: NapiLintOpts): unknown;
  lintFile(path: string, opts?: NapiLintFileOpts): unknown;
  lintVirtual(modules: Record<string, string>, entry: string, opts?: NapiLintFileOpts): unknown;
}

/** Build lint options from LintOptions, omitting null/undefined entries. */
function lintOpt(options?: LintOptions): NapiLintOpts | undefined {
  if (options == null) return undefined;
  const out: NapiLintOpts = {};
  if (options.basePath != null) out.basePath = options.basePath;
  if (options.vars != null) out.vars = options.vars;
  if (options.rules != null) out.rules = options.rules;
  return Object.keys(out).length > 0 ? out : undefined;
}

/** Build lint file options from LintFileOptions, omitting null/undefined entries. */
function lintFileOpt(options?: LintFileOptions): NapiLintFileOpts | undefined {
  if (options == null) return undefined;
  const out: NapiLintFileOpts = {};
  if (options.vars != null) out.vars = options.vars;
  if (options.rules != null) out.rules = options.rules;
  return Object.keys(out).length > 0 ? out : undefined;
}

/**
 * Create a native (napi) backend adapter from an injected addon.
 *
 * The addon is injected rather than imported directly so callers can test
 * with a mock and the module remains environment-agnostic.
 *
 * On creation, validates that the addon exposes the full set of base + node
 * methods from the canonical manifest. Per-call return-shape validation guards
 * against native-layer ABI drift.
 */
export function createNativeBackend(addon: NapiAddon): MdsNodeBackend {
  // Validate addon method presence at construction time using the canonical
  // manifest — catches native-layer ABI drift before any method is called.
  validateBackendMethods(addon, [...BASE_METHODS, ...NODE_METHODS], 'native addon');

  return {
    compile(source: string, options?: CompileOptions): CompileResult {
      const result: unknown = addon.compile(source, compileSrcOpt(options) as NapiCompileOpts | undefined);
      assertResultShape(result, 'compile');
      return result as CompileResult;
    },

    check(source: string, options?: CheckOptions): CheckResult {
      const result: unknown = addon.check(source, checkSrcOpt(options) as NapiCheckOpts | undefined);
      assertResultShape(result, 'check');
      return result as CheckResult;
    },

    async compileFile(path: string, options?: FileOptions): Promise<CompileResult> {
      const result: unknown = await addon.compileFile(path, fileCompileOpt(options) as NapiFileCompileOpts | undefined);
      assertResultShape(result, 'compile');
      return result as CompileResult;
    },

    async checkFile(path: string, options?: CheckFileOptions): Promise<CheckResult> {
      const result: unknown = await addon.checkFile(path, fileCheckOpt(options));
      assertResultShape(result, 'check');
      return result as CheckResult;
    },

    lint(source: string, options?: LintOptions): LintResult {
      const result: unknown = addon.lint(source, lintOpt(options));
      assertResultShape(result, 'lint');
      return result as LintResult;
    },

    async lintFile(path: string, options?: LintFileOptions): Promise<LintResult> {
      const result: unknown = await addon.lintFile(path, lintFileOpt(options));
      assertResultShape(result, 'lint');
      return result as LintResult;
    },

    lintVirtual(modules: Record<string, string>, entry: string, options?: LintFileOptions): LintResult {
      const result: unknown = addon.lintVirtual(modules, entry, lintFileOpt(options));
      assertResultShape(result, 'lint');
      return result as LintResult;
    },

    getBackend(): BackendType {
      return 'native';
    },
  };
}
