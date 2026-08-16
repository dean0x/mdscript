// NOTE (AC-P3-21): index.ts is NOT in the package `exports` map and has no
// `main`/`types` fallback. dist/index.js is unreachable to consumers. This
// barrel is an internal convenience for repo-level tooling only. Public types
// are exported from dist/node.d.ts (Node) and dist/browser.d.ts (browser)
// via the `"."` exports-map entry in package.json.
export type {
  BackendType,
  CheckFileOptions,
  CheckOptions,
  CheckResult,
  CompileOptions,
  CompileResult,
  FileOptions,
  InitOptions,
  LintDiagnostic,
  LintFileOptions,
  LintFileReport,
  LintOptions,
  LintResult,
  LintRuleName,
  LintSpan,
  MarkdownResult,
  MdsBackend,
  MdsBaseBackend,
  MdsError,
  MdsErrorSpan,
  MdsNodeBackend,
  Message,
  MessagesResult,
  RuleSeverity,
  SourceMapV3,
} from './types.js';
export { isMdsError, LINT_RULE_NAMES } from './types.js';
export type { WasmModule } from './backend/wasm.js';
export { initWasmNode, initWasmBrowser, createWasmBackend } from './backend/wasm.js';
