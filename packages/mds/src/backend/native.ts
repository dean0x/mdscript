import type {
  BackendType,
  CheckResult,
  CompileOptions,
  CompileResult,
  FileOptions,
  MdsNodeBackend,
} from '../types.js';
import { varsOpt } from '../util/options.js';

/**
 * Shape of the napi addon exports.
 * compile/check accept { basePath?, vars? } for string sources.
 * compileFile/checkFile accept { vars? } for file paths.
 */
interface NapiAddon {
  compile(source: string, opts?: { basePath?: string; vars?: Record<string, unknown> }): CompileResult;
  check(source: string, opts?: { basePath?: string; vars?: Record<string, unknown> }): CheckResult;
  compileFile(path: string, opts?: { vars?: Record<string, unknown> }): CompileResult;
  checkFile(path: string, opts?: { vars?: Record<string, unknown> }): CheckResult;
}

/**
 * Create a native (napi) backend adapter from an injected addon.
 *
 * The addon is injected rather than imported directly so callers can test
 * with a mock and the module remains environment-agnostic.
 */
export function createNativeBackend(addon: NapiAddon): MdsNodeBackend {
  return {
    compile(source: string, options?: CompileOptions): CompileResult {
      return addon.compile(source, varsOpt(options));
    },

    check(source: string, options?: CompileOptions): CheckResult {
      return addon.check(source, varsOpt(options));
    },

    async compileFile(path: string, options?: FileOptions): Promise<CompileResult> {
      return addon.compileFile(path, varsOpt(options));
    },

    async checkFile(path: string, options?: FileOptions): Promise<CheckResult> {
      return addon.checkFile(path, varsOpt(options));
    },

    getBackend(): BackendType {
      return 'native';
    },
  };
}
