#!/usr/bin/env node
// D1 synchronized-version gate.
//
// One coordinated release means every publishable artifact ships the SAME
// version. This gate asserts:
//   1. Every publishable package.json `version` == the workspace crate version.
//   2. No `file:` specifiers leak into any published dependency set.
//   3. Every internal `@mdscript/*` dependency is a caret range on that version
//      (e.g. ^0.1.0), so installed consumers resolve the matching release.
//   4. Every workspace crate's path+version dep on mds-core matches the
//      workspace version (kept in lockstep by bump-version.mjs).
//   5. crates/mds-python/pyproject.toml names the package "markdown-script"
//      (ADR-012) and declares version as dynamic (maturin reads from Cargo
//      workspace), so the published wheel version always equals canonical.
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
  'packages/rspack-loader/package.json',
];

// Workspace crates with a path+version dep on mds-core (must stay in lockstep
// with the workspace version — see scripts/bump-version.mjs CRATE_CARGO_PATHS).
const CRATE_CARGO_PATHS = [
  'crates/mds-cli/Cargo.toml',
  'crates/mds-napi/Cargo.toml',
  'crates/mds-wasm/Cargo.toml',
  'crates/mds-python/Cargo.toml',
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

// --- Check every workspace crate's path+version dep on mds-core ---------------
for (const rel of CRATE_CARGO_PATHS) {
  const abs = join(ROOT, rel);
  if (!existsSync(abs)) {
    fail(`${rel}: missing (expected workspace crate)`);
    continue;
  }
  const content = readFileSync(abs, 'utf8');
  const matches = [...content.matchAll(/package\s*=\s*"mds-core".*?version\s*=\s*"([^"]+)"/g)];
  if (matches.length === 0) {
    fail(`${rel}: no path+version dep on mds-core found`);
    continue;
  }
  for (const [, depVersion] of matches) {
    if (depVersion !== canonical) {
      fail(`${rel}: mds-core dep version "${depVersion}" != workspace version "${canonical}"`);
    }
  }
}

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

// --- Check Python pyproject.toml (ADR-012: name must be "markdown-script";
//     version must be dynamic so maturin stamps it from the Cargo workspace) ----
const PYPROJECT_PATH = 'crates/mds-python/pyproject.toml';
const pyprojectAbs = join(ROOT, PYPROJECT_PATH);
if (!existsSync(pyprojectAbs)) {
  fail(`${PYPROJECT_PATH}: missing (expected mds-python package manifest)`);
} else {
  const pyproject = readFileSync(pyprojectAbs, 'utf8');
  // Assert distribution name is "markdown-script" (ADR-012; "mdscript" on PyPI
  // is held by an unrelated project — sharing the name is unsafe).
  if (!/\bname\s*=\s*"markdown-script"/.test(pyproject)) {
    fail(`${PYPROJECT_PATH}: project name is not "markdown-script" (ADR-012)`);
  }
  // Assert version is dynamic. maturin stamps the version from the Cargo
  // workspace [workspace.package] version at build time. If "version" is
  // removed from dynamic[], maturin will fail to build the wheel; but catching
  // it here avoids a confusing maturin error deep in the release pipeline.
  if (!/dynamic\s*=\s*\[(?:[^[\]]*,\s*)?["']version["']/.test(pyproject)) {
    fail(`${PYPROJECT_PATH}: "version" must be in dynamic[] so maturin reads it from the Cargo workspace`);
  }
}

if (errors.length > 0) {
  console.error('✖ version-consistency gate FAILED:');
  for (const e of errors) console.error(`  - ${e}`);
  process.exit(1);
}

console.log(`✓ version gate: ${PKG_PATHS.length} packages + ${CRATE_CARGO_PATHS.length} crates all at ${canonical}; no file: refs; internal deps pinned to ${semverCaret}; Python pyproject.toml name=markdown-script version=dynamic`);
