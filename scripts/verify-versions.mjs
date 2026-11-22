#!/usr/bin/env node
// Check release manifest and lockfile versions against the Cargo workspace.
// One release uses one version. This gate checks these rules:
//   1. Each publishable package.json uses the workspace version.
//   2. Published dependency sets use no `file:` specifiers.
//   3. Each internal `@mdscript/*` dependency uses a caret range on that
//      version (for example, ^0.1.0).
//   4. Each crate's path+version dependency on mds-core uses the workspace
//      version.
//   5. The Python distribution name is `markdown-script`, not `mdscript`.
//      pyproject.toml declares the version as dynamic so maturin reads it from
//      the Cargo workspace.
//   6. Cargo.lock has one source-less entry at that version for each workspace
//      crate. The publishable workspace entries in package-lock.json use that
//      version and the same caret ranges for internal dependencies.
//
// Run locally and in CI. Exit 0 means consistent; exit 1 means drift or bad input.
'use strict';

import { readFileSync, realpathSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { CRATE_CARGO_PATHS, DEP_SETS, LOCK_MEMBERS, PKG_DIRS, PKG_PATHS } from './release-manifest.mjs';

const PYPROJECT_PATH = 'crates/mds-python/pyproject.toml';
export const WORKSPACE_VERSION = /(^\[workspace\.package\][ \t]*\r?\n(?:(?!\[)[^\r\n]*\r?\n)*?[ \t]*version[ \t]*=[ \t]*")([^"\r\n]+)(")/m;
export const CORE_DEP_VERSION = /(package\s*=\s*"mds-core".*?version\s*=\s*")([^"\r\n]+)(")/g;
export const CARGO_LOCK_VERSION = /^([ \t]*version[ \t]*=[ \t]*")([^"\r\n]+)(")/;

export const isRecord = (value) => value !== null && typeof value === 'object' && !Array.isArray(value);
export const isInternal = (name) => name.startsWith('@mdscript/');

export function parseRecord(text, path) {
  let value;
  try {
    value = JSON.parse(text);
  } catch {
    return { ok: false, problem: `${path}: invalid JSON` };
  }
  if (!isRecord(value)) return { ok: false, problem: `${path}: expected a JSON object` };
  return { ok: true, value };
}

// Keep line separators so a rewrite leaves all other bytes intact.
export function scanCargoLock(text, members) {
  const lines = text.split(/(\r?\n)/);
  const blocks = new Map(members.map((name) => [name, []]));
  let block;
  for (let i = 0; i < lines.length; i += 2) {
    const line = lines[i].trim();
    if (line.startsWith('[')) {
      block = /^\[\[package\]\](?:\s*#.*)?$/.test(line) ? { source: false, versions: [] } : undefined;
    } else if (block) {
      const name = /^name\s*=\s*"([^"]+)"/.exec(line);
      if (name) blocks.get(name[1])?.push(block);
      if (/^source\s*=/.test(line)) block.source = true;
      const version = CARGO_LOCK_VERSION.exec(lines[i]);
      if (version) block.versions.push({ index: i, value: version[2] });
    }
  }
  return { lines, blocks };
}

export function checkCargoLock(text, version, members) {
  const { blocks } = scanCargoLock(text, members);
  const problems = [];
  for (const [name, entries] of blocks) {
    const at = `Cargo.lock: ${name}`;
    if (entries.length === 0) {
      problems.push(`${at}: missing workspace member`);
    } else if (entries.length !== 1 || entries[0].source) {
      problems.push(`${at}: duplicate or registry member; expected one source-less block`);
    } else if (entries[0].versions.length !== 1) {
      problems.push(`${at}: expected one version line`);
    } else if (entries[0].versions[0].value !== version) {
      problems.push(`${at}: version "${entries[0].versions[0].value}" != workspace version "${version}"`);
    }
  }
  return problems;
}

function checkNpmEntry(pkg, at, version, rejectFile = false) {
  const problems = [];
  if (typeof pkg.version !== 'string') {
    problems.push(`${at}: expected a version string`);
  } else if (pkg.version !== version) {
    problems.push(`${at}: version ${JSON.stringify(pkg.version)} != workspace version ${JSON.stringify(version)}`);
  }
  const caret = `^${version}`;
  for (const set of DEP_SETS) {
    const deps = pkg[set];
    if (deps === undefined) continue;
    if (!isRecord(deps)) {
      problems.push(`${at}: ${set} must be an object`);
      continue;
    }
    for (const [name, spec] of Object.entries(deps)) {
      const field = `${at}: ${set}[${JSON.stringify(name)}]`;
      if (rejectFile && typeof spec === 'string' && spec.startsWith('file:')) {
        problems.push(`${field} uses a file: specifier (${JSON.stringify(spec)})`);
      }
      if (isInternal(name) && spec !== caret) {
        const actual = typeof spec === 'string' ? JSON.stringify(spec) : 'not a string';
        problems.push(`${field} is ${actual}, expected ${JSON.stringify(caret)}`);
      }
    }
  }
  return problems;
}

export function parsePackageLock(text) {
  const parsed = parseRecord(text, 'package-lock.json');
  if (!parsed.ok) return parsed;
  if (!isRecord(parsed.value.packages)) {
    return { ok: false, problem: 'package-lock.json: expected a packages object' };
  }
  return parsed;
}

export function checkPackageLock(text, version, dirs) {
  const parsed = parsePackageLock(text);
  if (!parsed.ok) return [parsed.problem];
  const { packages } = parsed.value;
  return dirs.flatMap((dir) => {
    const at = `package-lock.json: ${dir}`;
    if (!Object.hasOwn(packages, dir)) return [`${at}: missing workspace entry`];
    if (!isRecord(packages[dir])) return [`${at}: expected a workspace object`];
    return checkNpmEntry(packages[dir], at, version);
  });
}

export function checkVersions(files) {
  const version = WORKSPACE_VERSION.exec(files.get('Cargo.toml'))?.[2];
  if (!version) return { version, problems: ['Cargo.toml: could not find [workspace.package] version'] };
  const problems = [];
  for (const path of CRATE_CARGO_PATHS) {
    const matches = [...files.get(path).matchAll(CORE_DEP_VERSION)];
    if (matches.length === 0) problems.push(`${path}: no path+version dep on mds-core found`);
    for (const [, , depVersion] of matches) {
      if (depVersion !== version) {
        problems.push(`${path}: mds-core dep version "${depVersion}" != workspace version "${version}"`);
      }
    }
  }
  for (const path of PKG_PATHS) {
    const parsed = parseRecord(files.get(path), path);
    if (parsed.ok) problems.push(...checkNpmEntry(parsed.value, path, version, true));
    else problems.push(parsed.problem);
  }
  const pyproject = files.get(PYPROJECT_PATH);
  if (!/\bname\s*=\s*"markdown-script"/.test(pyproject)) {
    problems.push(`${PYPROJECT_PATH}: project name is not "markdown-script"`);
  }
  if (!/dynamic\s*=\s*\[(?:[^\[\]]*,\s*)?["']version["']/.test(pyproject)) {
    problems.push(`${PYPROJECT_PATH}: "version" must be in dynamic[] so maturin reads it from the Cargo workspace`);
  }
  problems.push(...checkCargoLock(files.get('Cargo.lock'), version, LOCK_MEMBERS));
  problems.push(...checkPackageLock(files.get('package-lock.json'), version, PKG_DIRS));
  return { version, problems };
}

export function readVersionFiles(root, extraPaths = []) {
  const files = new Map();
  const problems = [];
  const paths = ['Cargo.toml', 'Cargo.lock', 'package-lock.json', ...CRATE_CARGO_PATHS, ...PKG_PATHS, PYPROJECT_PATH, ...extraPaths];
  for (const path of paths) {
    try {
      files.set(path, readFileSync(join(root, path), 'utf8'));
    } catch (error) {
      problems.push(`${path}: cannot read file (${error.code ?? 'unknown error'})`);
    }
  }
  return { files, problems };
}

export function isMainModule(metaUrl) {
  const entry = process.argv[1];
  if (!entry) return false;
  try {
    return realpathSync(entry) === realpathSync(fileURLToPath(metaUrl));
  } catch {
    return pathToFileURL(resolve(entry)).href === metaUrl;
  }
}

export function reportVersionProblems(problems) {
  console.error('✖ version-consistency gate FAILED:');
  for (const problem of problems) console.error(`  - ${problem}`);
  process.exitCode = 1;
}

function main() {
  const { files, problems: readProblems } = readVersionFiles(fileURLToPath(new URL('../', import.meta.url)));
  if (readProblems.length > 0) return reportVersionProblems(readProblems);
  const { version, problems } = checkVersions(files);
  if (problems.length > 0) return reportVersionProblems(problems);
  console.log(`✓ version gate: ${PKG_PATHS.length} packages + ${CRATE_CARGO_PATHS.length} crates all at ${version}; no file: refs; internal deps pinned to ^${version}; Python pyproject.toml name=markdown-script version=dynamic`);
}

if (isMainModule(import.meta.url)) main();
