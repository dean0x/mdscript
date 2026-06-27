#!/usr/bin/env node
// Writes dist-cjs/package.json to mark the CJS output directory as CommonJS.
// Called as the final step of the dual-build (ESM + CJS) for packages that
// ship both formats. Usage: node scripts/write-cjs-package.cjs <output-dir>
'use strict';

const fs = require('fs');
const path = require('path');

const rawArg = process.argv[2] || 'dist-cjs';
// Resolve against cwd and validate the result stays within cwd to prevent
// path-traversal (CWE-23) when the argument is supplied by a caller.
const cwd = process.cwd();
const outDir = path.resolve(cwd, rawArg);
if (!outDir.startsWith(cwd + path.sep) && outDir !== cwd) {
  console.error(`write-cjs-package: output path escapes cwd: ${outDir}`);
  process.exit(1);
}
fs.mkdirSync(outDir, { recursive: true });
const dest = path.join(outDir, 'package.json');
fs.writeFileSync(dest, '{"type":"commonjs"}\n');
