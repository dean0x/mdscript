#!/usr/bin/env node
// Set a new version in the workspace Cargo.toml and its path+version
// dependencies on mds-core. Update publishable npm versions and internal
// dependency ranges. Update only the workspace entries in both lockfiles.
// Stamp CHANGELOG.md. Run locally before you tag a release.
//
// Usage: node scripts/bump-version.mjs <version>
//   Example: node scripts/bump-version.mjs 0.2.0
'use strict';

import { writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { CRATE_CARGO_PATHS, DEP_SETS, LOCK_MEMBERS, PKG_DIRS, PKG_PATHS } from './release-manifest.mjs';
import {
  CARGO_LOCK_VERSION, CORE_DEP_VERSION, WORKSPACE_VERSION, checkVersions, isInternal,
  isMainModule, isRecord, parsePackageLock, parseRecord, readVersionFiles, reportVersionProblems, scanCargoLock,
} from './verify-versions.mjs';

const serialize = (json, eol) => `${JSON.stringify(json, null, 2).replaceAll('\n', eol)}${eol}`;
const lineEnd = (text) => text.includes('\r\n') ? '\r\n' : '\n';
const setVersion = (version) => (_, prefix, _previous, suffix) => `${prefix}${version}${suffix}`;
const RELEASE_VERSION = String.raw`\d+\.\d+\.\d+(?:-[\w.]+)?`;
const VERSION_ARG = new RegExp(`^${RELEASE_VERSION}$`);
const UNRELEASED_LINK = new RegExp(
  String.raw`^\[Unreleased\]: (https://[^\s/]+/[^\s/]+/[^\s/]+)/compare/v(${RELEASE_VERSION})\.\.\.HEAD$`, 'm',
);

export function rewriteCargoLock(text, version, members) {
  const { lines, blocks } = scanCargoLock(text, members);
  for (const entries of blocks.values()) {
    for (const entry of entries) {
      if (entry.source || entry.versions.length !== 1) continue;
      const [{ index }] = entry.versions;
      lines[index] = lines[index].replace(CARGO_LOCK_VERSION, setVersion(version));
    }
  }
  return lines.join('');
}

function versionedPackage(pkg, version) {
  const updated = { ...pkg, version };
  for (const set of DEP_SETS) {
    if (!isRecord(pkg[set])) continue;
    updated[set] = Object.fromEntries(Object.entries(pkg[set]).map(([name, spec]) => [
      name, isInternal(name) && typeof spec === 'string' ? `^${version}` : spec,
    ]));
  }
  return updated;
}

export function rewritePackageLock(text, version, dirs) {
  const parsed = parsePackageLock(text);
  if (!parsed.ok) return parsed;
  const lock = parsed.value;
  const packages = { ...lock.packages };
  for (const dir of dirs) {
    const pkg = packages[dir];
    if (Object.hasOwn(packages, dir) && isRecord(pkg) && typeof pkg.version === 'string') {
      packages[dir] = versionedPackage(pkg, version);
    }
  }
  const eol = lineEnd(text);
  const original = serialize(lock, eol);
  const updated = serialize({ ...lock, packages }, eol);
  if (original === updated) return { ok: true, text };
  // Do not reformat unrelated fields or discard duplicate keys in a hand-edited lock.
  if (original !== text) return { ok: false, problem: 'package-lock.json: unexpected format; a JSON round trip would change other bytes' };
  return { ok: true, text: updated };
}

function stampChangelog(text, version, date) {
  const withHeading = text.replace(/^(## \[Unreleased\]\s*)$/m, `$1\n## [${version}] — ${date}\n`);
  if (withHeading === text) return { ok: false, problem: 'CHANGELOG.md: missing ## [Unreleased] heading' };

  // Use the existing repository URL and keep earlier release links intact.
  const stamped = withHeading.replace(UNRELEASED_LINK, (_, baseUrl, prevVersion) =>
    `[Unreleased]: ${baseUrl}/compare/v${version}...HEAD\n[${version}]: ${baseUrl}/compare/v${prevVersion}...v${version}`,
  );
  if (stamped === withHeading) return { ok: false, problem: 'CHANGELOG.md: missing or invalid [Unreleased]: compare link' };
  return { ok: true, text: stamped };
}

function prepareBump(files, version, date) {
  const updated = new Map(files);
  const replacement = setVersion(version);
  updated.set('Cargo.toml', files.get('Cargo.toml').replace(WORKSPACE_VERSION, replacement));
  for (const path of CRATE_CARGO_PATHS) {
    updated.set(path, files.get(path).replace(CORE_DEP_VERSION, replacement));
  }

  const problems = [];
  for (const path of PKG_PATHS) {
    const text = files.get(path);
    const parsed = parseRecord(text, path);
    if (parsed.ok) updated.set(path, serialize(versionedPackage(parsed.value, version), lineEnd(text)));
    else problems.push(parsed.problem);
  }

  updated.set('Cargo.lock', rewriteCargoLock(files.get('Cargo.lock'), version, LOCK_MEMBERS));
  const npmLock = rewritePackageLock(files.get('package-lock.json'), version, PKG_DIRS);
  if (npmLock.ok) updated.set('package-lock.json', npmLock.text);
  else problems.push(npmLock.problem);
  const changelog = stampChangelog(files.get('CHANGELOG.md'), version, date);
  if (changelog.ok) updated.set('CHANGELOG.md', changelog.text);
  else problems.push(changelog.problem);

  if (problems.length > 0) return { updated, problems };
  return { updated, problems: checkVersions(updated).problems };
}

function main() {
  const version = process.argv[2];
  if (!VERSION_ARG.test(version ?? '')) {
    console.error('Usage: bump-version.mjs <semver>  (e.g. 0.2.0)');
    process.exitCode = 1;
    return;
  }

  const root = fileURLToPath(new URL('../', import.meta.url));
  const { files, problems: readProblems } = readVersionFiles(root, ['CHANGELOG.md']);
  if (readProblems.length > 0) return reportVersionProblems(readProblems);
  const { updated, problems } = prepareBump(files, version, new Date().toISOString().slice(0, 10));
  if (problems.length > 0) return reportVersionProblems(problems);

  let changed = 0;
  for (const [path, text] of updated) {
    if (text === files.get(path)) continue;
    try {
      writeFileSync(join(root, path), text);
    } catch (error) {
      return reportVersionProblems([`${path}: cannot write file (${error.code ?? 'unknown error'})`]);
    }
    changed++;
    console.log(`  ${path} → ${version}`);
  }
  console.log(`\n✓ bumped ${changed} files to ${version}`);
}

if (isMainModule(import.meta.url)) main();
