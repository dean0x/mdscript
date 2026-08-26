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
  CompileFileOptions,
  CompileOptions,
  FileOptions,
  LintDiagnostic,
  LintFileOptions,
  LintFileReport,
  LintOptions,
  LintResult,
  LintRuleName,
  LintSpan,
  MarkdownResult,
  RuleSeverity,
  SourceMapV3,
} from '../../dist/node.js';

// ── Positive cases: basePath accepted on string-surface types ─────────────────

// D-TS-01: CompileOptions must accept basePath (inherited via CheckOptions).
const _compileOpts: CompileOptions = { basePath: '/some/dir', vars: {}, sourceMap: true };

// D-TS-01: CheckOptions must accept basePath.
const _checkOpts: CheckOptions = { basePath: '/some/dir', vars: {} };

// LintOptions already had basePath; confirm it still does.
const _lintOpts: LintOptions = { basePath: '/some/dir', vars: {}, rules: { 'unused-variable': 'warn' } };

// ── Positive cases: CompileFileOptions accepts compile-surface keys ────────────

// D-TS-03: CompileFileOptions must accept vars, sourceMap, sourcesContent.
const _compileFileOpts: CompileFileOptions = { vars: {}, sourceMap: true, sourcesContent: false };

// ── Negative cases: basePath NOT accepted on file-surface types ───────────────
// Each @ts-expect-error is self-verifying: if basePath were ever added to these
// types, tsc emits "Unused @ts-expect-error directive" and the build fails.

// D-TS-03: CompileFileOptions must NOT have basePath.
// @ts-expect-error — basePath is intentionally absent from CompileFileOptions (D-TS-03)
const _compileFileBasePath: CompileFileOptions = { basePath: '/some/dir' };

// D-TS-02: FileOptions (deprecated alias for CompileFileOptions) must NOT have basePath.
// @ts-expect-error — basePath is intentionally absent from FileOptions (D-TS-02)
const _fileOpts: FileOptions = { basePath: '/some/dir' };

// CheckFileOptions must NOT have basePath.
// @ts-expect-error — basePath is intentionally absent from CheckFileOptions
const _checkFileOpts: CheckFileOptions = { basePath: '/some/dir' };

// LintFileOptions must NOT have basePath.
// @ts-expect-error — basePath is intentionally absent from LintFileOptions
const _lintFileOpts: LintFileOptions = { basePath: '/some/dir' };

// ── Variable-passing negative cases (AC-P3-20 stronger claim) ────────────────
// Object-literal excess-property checks only fire on fresh literals. The cases
// below use typed variables — the realistic consumer shape — to prove that the
// structural type matrix also rejects passing a string-surface options object
// directly to a file-surface API. `basePath?: never` on the file-surface types
// causes TypeScript to report: "Type 'string | undefined' is not assignable to
// type 'undefined'" when a variable carrying basePath is used.
declare const _compileSrcVar: CompileOptions;
// @ts-expect-error — CompileOptions (basePath?: string) is not assignable to FileOptions (basePath?: never)
const _fileFromCompileVar: FileOptions = _compileSrcVar;
declare const _checkSrcVar: CheckOptions;
// @ts-expect-error — CheckOptions (basePath?: string) is not assignable to CheckFileOptions (basePath?: never)
const _checkFileFromVar: CheckFileOptions = _checkSrcVar;
declare const _lintSrcVar: LintOptions;
// @ts-expect-error — LintOptions (basePath?: string) is not assignable to LintFileOptions (basePath?: never)
const _lintFileFromVar: LintFileOptions = _lintSrcVar;

// ── Inferred-object case (AC-P3-20 / testing-05) ─────────────────────────────
// The PR description claimed that inferred-object variables (type inferred from
// the literal, e.g. `{ basePath: string }`) are NOT rejected by file-surface
// types. The compiler disagrees: TS2322 fires — `string` is not assignable to
// `undefined` (the effective type of `basePath?: never`). The fixture encodes
// the ACTUAL compiler behaviour and prevents the PR-description claim from
// becoming silently true in a future TypeScript release.
const _inferredWithBasePath = { basePath: '/some/dir' };
// @ts-expect-error — { basePath: string } is rejected by CompileFileOptions (basePath?: never)
const _compileFileFromInferred: CompileFileOptions = _inferredWithBasePath;

// ── PR2 guard: rule-name and severity typing on LintOptions ──────────────────
// D-224-1 ruling: an unrecognised RULE NAME is deliberately NOT a type error —
// `rules` is `Record<string, RuleSeverity>` so configs naming a rule added in a
// newer binary still compile; the engine warns at runtime via
// LintResult.lint_warnings. Both cases below must therefore be ACCEPTED.
const _validRule: LintOptions = { rules: { 'unused-variable': 'warn' } };
const _fwdCompat: LintOptions = { rules: { 'a-future-rule': 'off' } };

// The SEVERITY value, by contrast, IS a closed set — an invalid severity must be a
// type error. This is the negative control proving `rules` is not typed as
// `Record<string, string>`: if RuleSeverity were widened, tsc reports
// "Unused @ts-expect-error directive" and the build fails.
// @ts-expect-error — 'sometimes' is not a RuleSeverity ('error' | 'warn' | 'info' | 'off')
const _badSeverity: LintOptions = { rules: { 'unused-variable': 'sometimes' } };

// ── SourceMapV3 must be nameable from the entry the exports map resolves ──────
// MarkdownResult.sourceMap is typed as SourceMapV3; a consumer that cannot name
// the type cannot annotate the value.
const _sourceMap: SourceMapV3 = { version: 3, sources: ['input.mds'], names: [], mappings: '' };
const _markdown: MarkdownResult = {
  kind: 'markdown', output: '', warnings: [], dependencies: [], sourceMap: _sourceMap,
};

// ── AC-P3-16: all lint types are nameable from the browser surface ─────────────
// (browser types are verified in consumer-browser.ts; here we just confirm they
// compile correctly when imported from the node entry.)
const _diagArr: LintDiagnostic[] = [];
// testing-03: pins LintDiagnostic.help: string | null and .span: LintSpan | null.
// Reverting either `| null` widening in types.ts causes TS2322 on these
// assignments, making the breaking type change detectable at compile time.
const _diagWithNulls: LintDiagnostic = {
  rule: 'unused-variable',
  severity: 'warn',
  message: 'variable is unused',
  help: null,
  fixable: false,
  span: null,
};
const _span: LintSpan = { offset: 0, length: 0 };
const _report: LintFileReport = { file: 'a.mds', diagnostics: _diagArr };
const _result: LintResult = { version: 1, files: [_report], truncated: false };
const _severity: RuleSeverity = 'warn';
const _ruleName: LintRuleName = 'unused-variable';

// Prevent unused-variable TS errors for the above declarations.
void _compileOpts; void _checkOpts; void _lintOpts;
void _compileFileOpts; void _compileFileBasePath;
void _fileOpts; void _checkFileOpts; void _lintFileOpts;
void _fileFromCompileVar; void _checkFileFromVar; void _lintFileFromVar;
void _inferredWithBasePath; void _compileFileFromInferred;
void _validRule; void _fwdCompat; void _badSeverity;
void _sourceMap; void _markdown;
void _diagArr; void _diagWithNulls; void _span; void _report; void _result; void _severity; void _ruleName;
