/**
 * Type-level matrix verification for the browser entry point.
 *
 * AC-P3-16: all lint types a TypeScript consumer must name to use the browser
 * lint API surface are exported from dist/browser.d.ts and compile correctly
 * under the repo's strict settings.
 *
 * AC-P3-20 (browser surface): `basePath` IS accepted at the type level on
 * `CompileOptions`, `CheckOptions`, and `LintOptions` from the browser entry.
 * These string-surface types are shared between the Node.js and browser entries
 * (D-TS-01; recorded as ADR-011). The WASM backend rejects a non-null
 * `basePath` at runtime with `mds::invalid_options`; the enforcement is
 * runtime-only, not type-level. Callers who need import resolution must use
 * the Node.js entry with MDS_BACKEND=native.
 *
 * `FileOptions` and `CheckFileOptions` carry no `@ts-expect-error` guard here
 * because neither type is exported from the browser entry — there is nothing
 * to verify absence on. The only file-surface negative case in this fixture
 * is `LintFileOptions`, the one file-surface type the browser entry does export.
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
  SourceMapV3,
} from '../../dist/browser.js';

// ── Positive: basePath accepted on string-surface types (shared with Node) ────
// D-TS-01 / ADR-011: CompileOptions, CheckOptions, and LintOptions carry
// `basePath` on both browser and Node entries. WASM enforces the constraint at
// runtime. These three assignments MUST compile without @ts-expect-error.
const _compileOpts: CompileOptions = { basePath: '/dir', sourceMap: true };
const _checkOpts: CheckOptions = { basePath: '/dir' };
const _lintOpts: LintOptions = { basePath: '/dir', rules: {} };

// ── Negative: basePath NOT accepted on LintFileOptions ────────────────────────
// @ts-expect-error — basePath is intentionally absent from LintFileOptions
const _lintFileOpts: LintFileOptions = { basePath: '/dir' };

// Variable-passing case: LintOptions (basePath?: string) must be rejected by
// LintFileOptions (basePath?: never) — not only fresh object literals.
declare const _lintSrcVar: LintOptions;
// @ts-expect-error — LintOptions (basePath?: string) not assignable to LintFileOptions (basePath?: never)
const _lintFileFromVar: LintFileOptions = _lintSrcVar;

// ── AC-P3-16: all seven lint types are nameable from the browser entry ────────
const _diagArr: LintDiagnostic[] = [];
// testing-03: pins LintDiagnostic.help: string | null and .span: LintSpan | null.
// Reverting either `| null` widening in types.ts causes TS2322 on these
// assignments, making the breaking type change detectable at compile time.
const _diagWithNulls: LintDiagnostic = {
  rule: 'empty-block',
  severity: 'error',
  message: 'empty block',
  help: null,
  fixable: false,
  span: null,
};
const _span: LintSpan = { offset: 0, length: 0 };
const _report: LintFileReport = { file: 'a.mds', diagnostics: _diagArr };
const _result: LintResult = { version: 1, files: [_report], truncated: false };
const _severity: RuleSeverity = 'error';
const _ruleName: LintRuleName = 'empty-block';

// SourceMapV3 must also be nameable from the browser entry — compile({sourceMap:true})
// is supported there, so consumers need the result type.
const _sourceMap: SourceMapV3 = { version: 3, sources: ['input.mds'], names: [], mappings: '' };

void _compileOpts; void _checkOpts; void _lintOpts; void _lintFileOpts; void _lintFileFromVar;
void _diagArr; void _diagWithNulls; void _span; void _report; void _result; void _severity; void _ruleName;
void _sourceMap;
