/**
 * Type-level matrix verification for the browser entry point.
 *
 * AC-P3-16: all lint types a TypeScript consumer must name to use the browser
 * lint API surface are exported from dist/browser.d.ts and compile correctly
 * under the repo's strict settings.
 *
 * AC-P3-20 (browser surface): basePath is NOT accepted on CompileOptions or
 * CheckOptions from the browser entry — it IS in those types (D-TS-01), but
 * since the WASM backend rejects it at runtime, the type is honest. Same
 * `@ts-expect-error` guards as consumer-node.ts for FileOptions etc.
 */
import type {
  CheckOptions,
  CompileOptions,
  LintDiagnostic,
  LintFileOptions,
  LintFileReport,
  LintOptions,
  LintResult,
  LintRuleName,
  LintSpan,
  RuleSeverity,
} from '../../dist/browser.js';

// ── Positive: basePath accepted on string-surface types ───────────────────────
const _compileOpts: CompileOptions = { basePath: '/dir', sourceMap: true };
const _checkOpts: CheckOptions = { basePath: '/dir' };
const _lintOpts: LintOptions = { basePath: '/dir', rules: {} };

// ── Negative: basePath NOT accepted on LintFileOptions ────────────────────────
// @ts-expect-error — basePath is intentionally absent from LintFileOptions
const _lintFileOpts: LintFileOptions = { basePath: '/dir' };

// ── AC-P3-16: all seven lint types are nameable from the browser entry ────────
const _diagArr: LintDiagnostic[] = [];
const _span: LintSpan = { offset: 0, length: 0 };
const _report: LintFileReport = { file: 'a.mds', diagnostics: _diagArr };
const _result: LintResult = { version: 1, files: [_report], truncated: false };
const _severity: RuleSeverity = 'error';
const _ruleName: LintRuleName = 'empty-block';

void _compileOpts; void _checkOpts; void _lintOpts; void _lintFileOpts;
void _diagArr; void _span; void _report; void _result; void _severity; void _ruleName;
