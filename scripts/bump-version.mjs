#!/usr/bin/env node
// Bump all publishable package versions + workspace Cargo.toml to a new
// version and stamp the CHANGELOG. Designed to run in CI (prepare-release
// job) or locally before tagging.
//
// Usage: node scripts/bump-version.mjs <version>
//   e.g. node scripts/bump-version.mjs 0.2.0
'use strict';

import { readFileSync, writeFileSync } from 'node:fs';
import { join, dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

const version = process.argv[2];
if (!version || !/^\d+\.\d+\.\d+(-[\w.]+)?$/.test(version)) {
  console.error('Usage: bump-version.mjs <semver>  (e.g. 0.2.0)');
  process.exit(1);
}

const caret = `^${version}`;
let changed = 0;

// --- 1. Cargo.toml workspace version ----------------------------------------
const cargoPath = join(ROOT, 'Cargo.toml');
const cargo = readFileSync(cargoPath, 'utf8');
const newCargo = cargo.replace(
  /(\[workspace\.package\][\s\S]*?\bversion\s*=\s*")([^"]+)(")/,
  `$1${version}$3`,
);
if (newCargo !== cargo) {
  writeFileSync(cargoPath, newCargo);
  changed++;
  console.log(`  Cargo.toml → ${version}`);
}

// --- 1b. Inter-crate dependency versions (path + version deps on mds-core) ---
const CRATE_CARGO_PATHS = [
  'crates/mds-cli/Cargo.toml',
  'crates/mds-napi/Cargo.toml',
  'crates/mds-wasm/Cargo.toml',
];

for (const rel of CRATE_CARGO_PATHS) {
  const abs = join(ROOT, rel);
  const content = readFileSync(abs, 'utf8');
  const updated = content.replace(
    /(package\s*=\s*"mds-core".*?version\s*=\s*")([^"]+)(")/g,
    `$1${version}$3`,
  );
  if (updated !== content) {
    writeFileSync(abs, updated);
    changed++;
    console.log(`  ${rel} → mds-core ${version}`);
  }
}

// --- 2. Publishable package.json files --------------------------------------
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

const DEP_SETS = ['dependencies', 'optionalDependencies', 'peerDependencies'];

for (const rel of PKG_PATHS) {
  const abs = join(ROOT, rel);
  const pkg = JSON.parse(readFileSync(abs, 'utf8'));
  pkg.version = version;

  for (const set of DEP_SETS) {
    const deps = pkg[set];
    if (!deps) continue;
    for (const name of Object.keys(deps)) {
      if (name.startsWith('@mdscript/')) {
        deps[name] = caret;
      }
    }
  }

  writeFileSync(abs, JSON.stringify(pkg, null, 2) + '\n');
  changed++;
  console.log(`  ${rel} → ${version}`);
}

// --- 3. Stamp CHANGELOG -----------------------------------------------------
const clPath = join(ROOT, 'CHANGELOG.md');
const cl = readFileSync(clPath, 'utf8');
const today = new Date().toISOString().slice(0, 10);

const stamped = cl
  .replace(
    /^(## \[Unreleased\]\s*)$/m,
    `$1\n## [${version}] — ${today}\n`,
  )
  .replace(
    /^(\[Unreleased\]:.*\/compare\/)v[\d.]+(...HEAD)$/m,
    `$1v${version}$2`,
  )
  .replace(
    /^(\[[\d.]+\]:.*\/releases\/tag\/)v[\d.]+$/m,
    `$1v${version}\n[${version}]: https://github.com/dean0x/mdscript/releases/tag/v${version}`,
  );

if (stamped !== cl) {
  writeFileSync(clPath, stamped);
  changed++;
  console.log(`  CHANGELOG.md → ${version} (${today})`);
}

console.log(`\n✓ bumped ${changed} files to ${version}`);
