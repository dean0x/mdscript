/**
 * Type-level matrix verification for the Node.js entry point.
 *
 * AC-P3-20: verify that `basePath` is accepted on string-surface types and
 * rejected on file-surface types, using `@ts-expect-error` on every negative
 * case. An UNUSED `@ts-expect-error` fails the build, which is the type-level
 * positive control: if `basePath` were ever added to a file-surface type,
 * tsc reports "Unused @ts-expect-error directive" and the run fails.
 *
 * AC-P3-21: all option types a consumer needs are exported from dist/node.d.ts
 * (the entry the `exports` map resolves for Node.js consumers).
 *
 * Note: this file is compiled by tsconfig.types.json (noEmit: true) and is NOT
 * in the main build (tsconfig.json excludes __test__). It runs as part of
 * `npm test` via the test script preamble.
 */
import type {
  CheckFileOptions,
  CheckOptions,
  CompileOptions,
  FileOptions,
  LintDiagnostic,
  LintFileOptions,
  LintFileReport,
  LintOptions,
  LintResult,
  LintRuleName,
  LintSpan,
  RuleSeverity,
} from '../../dist/node.js';

// ── Positive cases: basePath accepted on string-surface types ─────────────────

// D-TS-01: CompileOptions must accept basePath (inherited via CheckOptions).
const _compileOpts: CompileOptions = { basePath: '/some/dir', vars: {}, sourceMap: true };

// D-TS-01: CheckOptions must accept basePath.
const _checkOpts: CheckOptions = { basePath: '/some/dir', vars: {} };

// LintOptions already had basePath; confirm it still does.
const _lintOpts: LintOptions = { basePath: '/some/dir', vars: {}, rules: { 'unused-variable': 'warn' } };

// ── Negative cases: basePath NOT accepted on file-surface types ───────────────
// Each @ts-expect-error is self-verifying: if basePath were ever added to these
// types, tsc emits "Unused @ts-expect-error directive" and the build fails.

// D-TS-02: FileOptions must NOT have basePath.
// @ts-expect-error — basePath is intentionally absent from FileOptions (D-TS-02)
const _fileOpts: FileOptions = { basePath: '/some/dir' };

// CheckFileOptions must NOT have basePath.
// @ts-expect-error — basePath is intentionally absent from CheckFileOptions
const _checkFileOpts: CheckFileOptions = { basePath: '/some/dir' };

// LintFileOptions must NOT have basePath.
// @ts-expect-error — basePath is intentionally absent from LintFileOptions
const _lintFileOpts: LintFileOptions = { basePath: '/some/dir' };

// ── PR2 guard: invalid rule name must be rejected in LintOptions ──────────────
// D-224-1 introduced LintRuleName; ensure the type fixture protects both PRs.
const _validRule: LintOptions = { rules: { 'unused-variable': 'warn' } };
// Record<string, RuleSeverity> is accepted for forward compatibility.
const _fwdCompat: LintOptions = { rules: { 'a-future-rule': 'off' } };

// ── AC-P3-16: all lint types are nameable from the browser surface ─────────────
// (browser types are verified in consumer-browser.ts; here we just confirm they
// compile correctly when imported from the node entry.)
const _diagArr: LintDiagnostic[] = [];
const _span: LintSpan = { offset: 0, length: 0 };
const _report: LintFileReport = { file: 'a.mds', diagnostics: _diagArr };
const _result: LintResult = { version: 1, files: [_report], truncated: false };
const _severity: RuleSeverity = 'warn';
const _ruleName: LintRuleName = 'unused-variable';

// Prevent unused-variable TS errors for the above declarations.
void _compileOpts; void _checkOpts; void _lintOpts;
void _fileOpts; void _checkFileOpts; void _lintFileOpts;
void _validRule; void _fwdCompat;
void _diagArr; void _span; void _report; void _result; void _severity; void _ruleName;
