import type {
  BackendType,
  CheckResult,
  CompileOptions,
  CompileResult,
  FileOptions,
  LintFileOptions,
  LintOptions,
  LintResult,
  MdsNodeBackend,
} from '../types.js';
import { varsOpt } from '../util/options.js';
import { assertResultShape, validateBackendMethods, BASE_METHODS, NODE_METHODS } from './contract.js';

/**
 * Shape of the napi addon exports.
 * compile/check accept { basePath?, vars? } for string sources.
 * compileFile/checkFile accept { vars? } for file paths.
 * lint/lintFile/lintVirtual accept { basePath?, vars?, rules? }.
 */
interface NapiAddon {
  compile(source: string, opts?: { basePath?: string; vars?: Record<string, unknown> }): unknown;
  check(source: string, opts?: { basePath?: string; vars?: Record<string, unknown> }): unknown;
  compileFile(path: string, opts?: { vars?: Record<string, unknown> }): unknown;
  checkFile(path: string, opts?: { vars?: Record<string, unknown> }): unknown;
  lint(source: string, opts?: { basePath?: string; vars?: Record<string, unknown>; rules?: Record<string, string> }): unknown;
  lintFile(path: string, opts?: { vars?: Record<string, unknown>; rules?: Record<string, string> }): unknown;
  lintVirtual(modules: Record<string, unknown>, entry: string, opts?: { vars?: Record<string, unknown>; rules?: Record<string, string> }): unknown;
}

/** Build lint options from LintOptions, omitting null/undefined entries. */
function lintOpt(
  options?: LintOptions,
): { basePath?: string; vars?: Record<string, unknown>; rules?: Record<string, string> } | undefined {
  if (options == null) return undefined;
  const out: { basePath?: string; vars?: Record<string, unknown>; rules?: Record<string, string> } = {};
  if (options.basePath != null) out.basePath = options.basePath;
  if (options.vars != null) out.vars = options.vars;
  if (options.rules != null) out.rules = options.rules;
  return Object.keys(out).length > 0 ? out : undefined;
}

/** Build lint file options from LintFileOptions, omitting null/undefined entries. */
function lintFileOpt(
  options?: LintFileOptions,
): { vars?: Record<string, unknown>; rules?: Record<string, string> } | undefined {
  if (options == null) return undefined;
  const out: { vars?: Record<string, unknown>; rules?: Record<string, string> } = {};
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
      const result: unknown = addon.compile(source, varsOpt(options));
      assertResultShape(result, 'compile');
      return result as CompileResult;
    },

    check(source: string, options?: CompileOptions): CheckResult {
      const result: unknown = addon.check(source, varsOpt(options));
      assertResultShape(result, 'check');
      return result as CheckResult;
    },

    async compileFile(path: string, options?: FileOptions): Promise<CompileResult> {
      const result: unknown = await addon.compileFile(path, varsOpt(options));
      assertResultShape(result, 'compile');
      return result as CompileResult;
    },

    async checkFile(path: string, options?: FileOptions): Promise<CheckResult> {
      const result: unknown = await addon.checkFile(path, varsOpt(options));
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
