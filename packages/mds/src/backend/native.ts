import type {
  BackendType,
  CheckResult,
  CompileMessagesResult,
  CompileOptions,
  CompileResult,
  FileOptions,
  MdsNodeBackend,
} from '../types.js';
import { varsOpt } from '../util/options.js';
import { assertResultShape, validateBackendMethods, BASE_METHODS, NODE_METHODS } from './contract.js';

/**
 * Shape of the napi addon exports.
 * compile/check/compileMessages accept { basePath?, vars? } for string sources.
 * compileFile/checkFile/compileMessagesFile accept { vars? } for file paths.
 */
interface NapiAddon {
  compile(source: string, opts?: { basePath?: string; vars?: Record<string, unknown> }): CompileResult;
  check(source: string, opts?: { basePath?: string; vars?: Record<string, unknown> }): CheckResult;
  compileMessages(source: string, opts?: { basePath?: string; vars?: Record<string, unknown> }): CompileMessagesResult;
  compileFile(path: string, opts?: { vars?: Record<string, unknown> }): CompileResult;
  checkFile(path: string, opts?: { vars?: Record<string, unknown> }): CheckResult;
  compileMessagesFile(path: string, opts?: { vars?: Record<string, unknown> }): CompileMessagesResult;
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

    compileMessages(source: string, options?: CompileOptions): CompileMessagesResult {
      const result: unknown = addon.compileMessages(source, varsOpt(options));
      assertResultShape(result, 'compileMessages');
      return result as CompileMessagesResult;
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

    async compileMessagesFile(path: string, options?: FileOptions): Promise<CompileMessagesResult> {
      const result: unknown = await addon.compileMessagesFile(path, varsOpt(options));
      assertResultShape(result, 'compileMessages');
      return result as CompileMessagesResult;
    },

    getBackend(): BackendType {
      return 'native';
    },
  };
}
