// The Node.js and browser entry points re-export `LINT_RULE_NAMES` from this module.
// Consumers can import this value from `@mdscript/mds` in either environment.
export { isMdsError, LINT_RULE_NAMES } from './types.js';
export type {
  BackendType,
  CheckOptions,
  CheckResult,
  CompileOptions,
  CompileResult,
  InitOptions,
  LintDiagnostic,
  LintFileOptions,
  LintFileReport,
  LintOptions,
  LintResult,
  LintRuleName,
  LintSpan,
  MarkdownResult,
  MdsError,
  MdsErrorSpan,
  Message,
  MessagesResult,
  RuleSeverity,
  SourceMapV3,
} from './types.js';
