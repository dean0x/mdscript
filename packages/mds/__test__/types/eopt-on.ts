import type {
  CheckFileOptions,
  CheckOptions,
  CompileFileOptions,
  CompileOptions,
  InitOptions,
  LintFileOptions,
  LintOptions,
} from '../../dist/node.js';

// Optional input fields accept explicit undefined even in exact mode.
const _check: CheckOptions = { vars: undefined, basePath: undefined };
const _compile: CompileOptions = { sourceMap: undefined, sourcesContent: undefined };
const _compileFile: CompileFileOptions = { vars: undefined, sourceMap: undefined, sourcesContent: undefined };
const _checkFile: CheckFileOptions = { vars: undefined };
const _lint: LintOptions = { vars: undefined, rules: undefined, basePath: undefined };
const _lintFile: LintFileOptions = { vars: undefined, rules: undefined };
const _init: InitOptions = { wasmUrl: undefined };

// @ts-expect-error -- CompileFileOptions requires an absent basePath in exact mode.
const _compileFileBasePath: CompileFileOptions = { basePath: undefined };
// @ts-expect-error -- CheckFileOptions requires an absent basePath in exact mode.
const _checkFileBasePath: CheckFileOptions = { basePath: undefined };
// @ts-expect-error -- LintFileOptions requires an absent basePath in exact mode.
const _lintFileBasePath: LintFileOptions = { basePath: undefined };

void _check; void _compile; void _compileFile; void _checkFile;
void _lint; void _lintFile; void _init;
void _compileFileBasePath; void _checkFileBasePath; void _lintFileBasePath;
