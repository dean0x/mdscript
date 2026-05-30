#!/usr/bin/env node
// D1 synchronized-version gate.
//
// One coordinated release means every publishable artifact ships the SAME
// version. This gate asserts:
//   1. Every publishable package.json `version` == the workspace crate version.
//   2. No `file:` specifiers leak into any published dependency set.
//   3. Every internal `@mdscript/*` dependency is a caret range on that version
//      (e.g. ^0.1.0), so installed consumers resolve the matching release.
//
// Run locally and in CI before publishing. Exit 0 = consistent; 1 = drift.
'use strict';

import { readFileSync, existsSync } from 'node:fs';
import { join, dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

// Publishable npm packages (host napi package + universal + wasm + bundler set).
const PKG_PATHS = [
  'crates/mds-napi/package.json',
  'packages/mds/package.json',
  'packages/mds-wasm/package.json',
  'packages/bundler-utils/package.json',
  'packages/vite-plugin/package.json',
  'packages/rollup-plugin/package.json',
  'packages/webpack-loader/package.json',
];

const DEP_SETS = ['dependencies', 'optionalDependencies', 'peerDependencies'];

const errors = [];
const fail = (m) => errors.push(m);

// --- Canonical version: the workspace crate version from root Cargo.toml ------
const cargo = readFileSync(join(ROOT, 'Cargo.toml'), 'utf8');
const wsVerMatch = cargo.match(/\[workspace\.package\][\s\S]*?\bversion\s*=\s*"([^"]+)"/);
if (!wsVerMatch) {
  fail('Cargo.toml: could not find [workspace.package] version');
}
const canonical = wsVerMatch ? wsVerMatch[1] : null;
const semverCaret = canonical ? `^${canonical}` : null;

// --- Check every publishable package ------------------------------------------
for (const rel of PKG_PATHS) {
  const abs = join(ROOT, rel);
  if (!existsSync(abs)) {
    fail(`${rel}: missing (expected publishable package)`);
    continue;
  }
  const pkg = JSON.parse(readFileSync(abs, 'utf8'));

  if (pkg.version !== canonical) {
    fail(`${rel}: version "${pkg.version}" != workspace version "${canonical}"`);
  }

  for (const set of DEP_SETS) {
    const deps = pkg[set];
    if (!deps) continue;
    for (const [name, spec] of Object.entries(deps)) {
      if (typeof spec === 'string' && spec.startsWith('file:')) {
        fail(`${rel}: ${set}["${name}"] uses a file: specifier ("${spec}")`);
      }
      if (name.startsWith('@mdscript/') && spec !== semverCaret) {
        fail(`${rel}: ${set}["${name}"] is "${spec}", expected "${semverCaret}"`);
      }
    }
  }
}

if (errors.length > 0) {
  console.error('✖ version-consistency gate FAILED:');
  for (const e of errors) console.error(`  - ${e}`);
  process.exit(1);
}

console.log(`✓ version gate: ${PKG_PATHS.length} packages + crates all at ${canonical}; no file: refs; internal deps pinned to ${semverCaret}`);
