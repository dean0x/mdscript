// `LINT_RULE_NAMES` is a value export; exported directly from both entry points
// (dist/node.js and dist/browser.js) so that consumers can import it via
// `@mdscript/mds` regardless of environment.
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
