import type { CheckFileOptions, CompileFileOptions, LintFileOptions } from '../../dist/node.js';

// Without exact mode, the types accept an explicit undefined basePath.
// At runtime, the wrappers treat it as absent (U-OV-29).
const _compileFile: CompileFileOptions = { basePath: undefined };
const _checkFile: CheckFileOptions = { basePath: undefined };
const _lintFile: LintFileOptions = { basePath: undefined };

void _compileFile; void _checkFile; void _lintFile;
