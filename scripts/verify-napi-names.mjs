#!/usr/bin/env node
// A3 name<->loader verification gate (CRITICAL).
//
// The hand-maintained loader crates/mds-napi/index.js hardcodes, for each of the
// 7 supported platforms, the pair [<binary>.node filename, platform package name].
// @napi-rs/cli generates the per-platform npm/<platform>/package.json packages
// (name + the .node it ships) from the napi config. If those two ever drift, the
// published universal package will fail to load the binary at runtime on the
// affected platform — silently, only for users on that OS/arch.
//
// This gate asserts the generated packages EXACTLY match the loader's strings.
// Run it in CI after `napi create-npm-dirs` (+ `napi artifacts`), before publish.
//
// Usage: node scripts/verify-napi-names.mjs
// Exit 0 = match; exit 1 = mismatch (with a diff) or missing npm/ dir.
'use strict';

import { readFileSync, readdirSync, existsSync, statSync } from 'node:fs';
import { join, dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const NAPI_DIR = join(ROOT, 'crates', 'mds-napi');
const NPM_DIR = join(NAPI_DIR, 'npm');

const errors = [];
const fail = (msg) => errors.push(msg);

// --- 1. Extract the loader's [binary, packageName] pairs from index.js --------
const indexSrc = readFileSync(join(NAPI_DIR, 'index.js'), 'utf8');
const pairRe = /\[\s*'([^']+\.node)'\s*,\s*'(@[^']+)'\s*\]/g;
const loaderPairs = new Map(); // packageName -> binaryFilename
for (const m of indexSrc.matchAll(pairRe)) {
  loaderPairs.set(m[2], m[1]);
}
if (loaderPairs.size !== 7) {
  fail(`index.js: expected 7 platform entries, found ${loaderPairs.size}`);
}

// Expected os/cpu/libc derived from each package-name suffix. Strict on os/cpu;
// libc asserted only for musl (gnu's libc field varies across napi versions).
function expectedTraits(pkgName) {
  const suffix = pkgName.replace(/^@mdscript\/mds-napi-/, '');
  const parts = suffix.split('-'); // e.g. linux-x64-musl, win32-x64-msvc, darwin-arm64
  const os = { darwin: 'darwin', linux: 'linux', win32: 'win32' }[parts[0]];
  const cpu = parts[1]; // x64 | arm64
  const libc = parts[2] === 'musl' ? 'musl' : null;
  return { os, cpu, libc };
}

// --- 2. Read generated npm/*/package.json -------------------------------------
if (!existsSync(NPM_DIR)) {
  fail(
    `crates/mds-napi/npm/ does not exist. Run 'npx napi create-npm-dirs' (and ` +
    `'npx napi artifacts') in crates/mds-napi before this gate.`,
  );
}

const generated = new Map(); // packageName -> { dir, main, files, os, cpu, libc }
if (existsSync(NPM_DIR)) {
  for (const entry of readdirSync(NPM_DIR)) {
    const dir = join(NPM_DIR, entry);
    if (!statSync(dir).isDirectory()) continue;
    const pkgPath = join(dir, 'package.json');
    if (!existsSync(pkgPath)) {
      fail(`npm/${entry}: missing package.json`);
      continue;
    }
    const pkg = JSON.parse(readFileSync(pkgPath, 'utf8'));
    generated.set(pkg.name, {
      dir: entry,
      main: pkg.main,
      files: pkg.files ?? [],
      os: pkg.os ?? [],
      cpu: pkg.cpu ?? [],
      libc: pkg.libc ?? null,
    });
  }
}

// --- 3. Cross-check: every loader entry has a matching generated package ------
for (const [pkgName, binary] of loaderPairs) {
  const gen = generated.get(pkgName);
  if (!gen) {
    fail(`loader references "${pkgName}" but no generated npm/* package has that name`);
    continue;
  }
  // The .node filename the loader expects must be what the package actually ships.
  if (gen.main !== binary) {
    fail(`"${pkgName}": loader expects binary "${binary}" but package main is "${gen.main}"`);
  }
  if (!gen.files.includes(binary)) {
    fail(`"${pkgName}": binary "${binary}" not listed in package files [${gen.files.join(', ')}]`);
  }
  const want = expectedTraits(pkgName);
  if (want.os && !gen.os.includes(want.os)) {
    fail(`"${pkgName}": os should include "${want.os}" but is [${gen.os.join(', ')}]`);
  }
  if (want.cpu && !gen.cpu.includes(want.cpu)) {
    fail(`"${pkgName}": cpu should include "${want.cpu}" but is [${gen.cpu.join(', ')}]`);
  }
  if (want.libc) {
    const libcArr = Array.isArray(gen.libc) ? gen.libc : [];
    if (!libcArr.includes('musl')) {
      fail(`"${pkgName}": libc should include "musl" but is ${JSON.stringify(gen.libc)}`);
    }
  }
}

// --- 4. Cross-check: no generated package the loader doesn't know about -------
for (const pkgName of generated.keys()) {
  if (!loaderPairs.has(pkgName)) {
    fail(`generated package "${pkgName}" has no matching entry in the index.js loader`);
  }
}

// --- Report -------------------------------------------------------------------
if (errors.length > 0) {
  console.error('✖ napi name<->loader verification FAILED:');
  for (const e of errors) console.error(`  - ${e}`);
  process.exit(1);
}

console.log(`✓ napi name<->loader gate: ${loaderPairs.size} platform packages match the loader`);
for (const [pkgName, binary] of loaderPairs) {
  console.log(`  ${pkgName}  ->  ${binary}`);
}
