import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { LOCK_MEMBERS } from '../release-manifest.mjs';

const ROOT = fileURLToPath(new URL('../../', import.meta.url));
const read = (path) => readFileSync(join(ROOT, path), 'utf8');
const serialize = (data) => `${JSON.stringify(data, null, 2)}\n`;
const canonical = () => /^\[workspace\.package\]\r?\nversion = "([^"]+)"/m.exec(read('Cargo.toml'))[1];
const NEXT = '101.102.103';
const PRERELEASE = `${NEXT}-rc.1`;
const CORE = '[[package]]\nname = "mds-core"';
const REGISTRY_CORE = `\n${CORE}\nversion = "9.9.9"\nsource = "registry+https://example.invalid/index"\n`;
const CORE_BLOCK = /\[\[package\]\]\r?\nname = "mds-core"\r?\n[\s\S]*?(?=^\[\[package\]\]|(?![\s\S]))/m;

// Copy only release inputs, plus the scripts. Never invoke the bump CLI on ROOT.
function fixture(t) {
  const dir = mkdtempSync(join(tmpdir(), 'mds-versions-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const files = ['Cargo.toml', 'Cargo.lock', 'package-lock.json', 'CHANGELOG.md'];
  const copy = (path) => {
    const dest = join(dir, path);
    mkdirSync(dirname(dest), { recursive: true });
    copyFileSync(join(ROOT, path), dest);
  };
  for (const base of ['crates', 'packages', 'scripts']) {
    const dirs = readdirSync(join(ROOT, base), { withFileTypes: true });
    if (base === 'scripts') {
      for (const { name } of dirs) {
        if (/^(?:verify-versions|bump-version|release-manifest)\.mjs$/.test(name)) copy(`${base}/${name}`);
      }
      continue;
    }
    for (const entry of dirs.filter((entry) => entry.isDirectory())) {
      const subdir = `${base}/${entry.name}`;
      const names = readdirSync(join(ROOT, subdir));
      for (const name of ['Cargo.toml', 'package.json', 'pyproject.toml']) {
        if (names.includes(name)) files.push(`${subdir}/${name}`);
      }
    }
  }
  for (const path of files) copy(path);
  return {
    dir,
    files,
    read: (path) => readFileSync(join(dir, path), 'utf8'),
    write: (path, text) => writeFileSync(join(dir, path), text),
    run: (script, ...args) => spawnSync(process.execPath, [join(dir, script), ...args], { cwd: dir, encoding: 'utf8' }),
  };
}

function assertRejected(result, problem) {
  assert.equal(result.status, 1, result.stderr || result.stdout);
  assert.match(result.stderr, /version-consistency gate FAILED/);
  assert.match(result.stderr, problem);
}

for (const [name, edit, problem] of [
  ['stale member', (text) => text.replace(`${CORE}\nversion = "${canonical()}"`, `${CORE}\nversion = "9.9.9"`), /Cargo\.lock:.*mds-core.*9\.9\.9/],
  ['absent member', (text) => text.replace(CORE_BLOCK, ''), /Cargo\.lock:.*mds-core.*missing/],
  ['second source-less member block', (text) => `${text}\n${text.match(CORE_BLOCK)[0]}`, /Cargo\.lock:.*mds-core.*duplicate/],
  ['registry duplicate', (text) => text + REGISTRY_CORE, /Cargo\.lock:.*mds-core.*duplicate/],
  ['registry-only member', (text) => text.replace(CORE_BLOCK, REGISTRY_CORE), /Cargo\.lock:.*mds-core.*duplicate/],
]) {
  test(`Cargo.lock rejects ${name}`, (t) => {
    const repo = fixture(t);
    const original = repo.read('Cargo.lock');
    const planted = edit(original);
    assert.notEqual(planted, original);
    repo.write('Cargo.lock', planted);
    assertRejected(repo.run('scripts/verify-versions.mjs'), problem);
  });
}

const NAPI_DIR = 'crates/mds-napi';
const MDS_DIR = 'packages/mds';
const MDS_DEP = '@mdscript/mds-napi';
const PYPROJECT = 'crates/mds-python/pyproject.toml';
for (const [name, path, edit, problem] of [
  ['package version drift', `${MDS_DIR}/package.json`, (text) => {
    const pkg = JSON.parse(text);
    return serialize({ ...pkg, version: NEXT });
  }, /packages\/mds\/package\.json: version "101\.102\.103" != workspace version/],
  ['an internal file specifier', `${MDS_DIR}/package.json`, (text) => {
    const pkg = JSON.parse(text);
    return serialize({
      ...pkg, optionalDependencies: { ...pkg.optionalDependencies, [MDS_DEP]: `file:../../${NAPI_DIR}` },
    });
  }, /packages\/mds\/package\.json: optionalDependencies\["@mdscript\/mds-napi"\] uses a file: specifier/],
  ['mds-core dependency version drift', 'crates/mds-cli/Cargo.toml', (text) =>
    text.replace(/^(.*package = "mds-core".*version = ")[^"]+/m, (_, prefix) => `${prefix}${NEXT}`),
  /crates\/mds-cli\/Cargo\.toml: mds-core dep version "101\.102\.103" != workspace version/],
  ['a different Python project name', PYPROJECT, (text) => text.replace('name = "markdown-script"', 'name = "mdscript"'),
    /crates\/mds-python\/pyproject\.toml: project name is not "markdown-script"/],
  ['a Python dynamic list without version', PYPROJECT, (text) => text.replace('dynamic = ["version"]', 'dynamic = []'),
    /crates\/mds-python\/pyproject\.toml: "version" must be in dynamic\[\]/],
]) {
  test(`version gate rejects ${name}`, (t) => {
    const repo = fixture(t);
    const original = repo.read(path);
    const planted = edit(original);
    assert.notEqual(planted, original);
    repo.write(path, planted);
    assertRejected(repo.run('scripts/verify-versions.mjs'), problem);
  });
}

for (const [name, edit, problem] of [
  ['stale workspace version', (lock) => { lock.packages[NAPI_DIR].version = '9.9.9'; }, /package-lock\.json:.*crates\/mds-napi.*9\.9\.9/],
  ['absent workspace', (lock) => { delete lock.packages[NAPI_DIR]; }, /package-lock\.json:.*crates\/mds-napi.*missing/],
  ['absent workspace version', (lock) => { delete lock.packages[NAPI_DIR].version; }, /package-lock\.json:.*crates\/mds-napi.*version/],
  ['nonstring workspace version', (lock) => { lock.packages[NAPI_DIR].version = { toString: null }; }, /package-lock\.json:.*crates\/mds-napi.*version/],
  ['stale internal range', (lock) => { lock.packages[MDS_DIR].optionalDependencies[MDS_DEP] = '^9.9.9'; }, /package-lock\.json:.*@mdscript\/mds-napi.*\^9\.9\.9/],
  ['invalid JSON', () => '{', /package-lock\.json:.*JSON/],
]) {
  test(`package-lock.json rejects ${name}`, (t) => {
    const repo = fixture(t);
    const original = repo.read('package-lock.json');
    const lock = JSON.parse(original);
    const planted = edit(lock) ?? serialize(lock);
    assert.notEqual(planted, original);
    repo.write('package-lock.json', planted);
    assertRejected(repo.run('scripts/verify-versions.mjs'), problem);
  });
}

test('verification accepts locks with CRLF line ends', (t) => {
  const repo = fixture(t);
  for (const path of ['Cargo.lock', 'package-lock.json']) {
    repo.write(path, repo.read(path).replace(/\r?\n/g, '\r\n'));
  }
  const result = repo.run('scripts/verify-versions.mjs');
  assert.equal(result.status, 0, result.stderr);
  const version = canonical();
  assert.equal(result.stdout, `✓ version gate: 8 packages + 4 crates all at ${version}; no file: refs; internal deps pinned to ^${version}; Python pyproject.toml name=markdown-script version=dynamic\n`);
});

test('bump updates only workspace lock fields and keeps CRLF', (t) => {
  const repo = fixture(t);
  const version = canonical();
  const pattern = new RegExp(`^version = "${version.replaceAll('.', '\\.')}"$`, 'gm');
  const cargo = repo.read('Cargo.lock');
  assert.equal([...cargo.matchAll(pattern)].length, 5);
  const crlf = (text) => text.replace(/\r?\n/g, '\r\n');
  repo.write('Cargo.lock', crlf(cargo));
  const lock = JSON.parse(repo.read('package-lock.json'));
  // These fields represent a registry package, not a workspace package.
  lock.packages['node_modules/planted'] = { version, dependencies: { '@mdscript/mds': `^${version}` } };
  repo.write('package-lock.json', crlf(serialize(lock)));

  const result = repo.run('scripts/bump-version.mjs', NEXT);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(repo.read('Cargo.lock'), crlf(cargo.replace(pattern, `version = "${NEXT}"`)));
  for (const [dir, pkg] of Object.entries(lock.packages)) {
    if (!/^(?:packages|crates)\/[^/]+$/.test(dir)) continue;
    pkg.version = NEXT;
    for (const set of ['dependencies', 'optionalDependencies', 'peerDependencies']) {
      for (const name of Object.keys(pkg[set] ?? {})) {
        if (name.startsWith('@mdscript/')) pkg[set][name] = `^${NEXT}`;
      }
    }
  }
  assert.equal(repo.read('package-lock.json'), crlf(serialize(lock)));
  const verified = repo.run('scripts/verify-versions.mjs');
  assert.equal(verified.status, 0, verified.stderr);
});

test('bump stamps the changelog before and after a prerelease', (t) => {
  const repo = fixture(t);
  const previous = canonical();
  for (const [version, from] of [[PRERELEASE, previous], [NEXT, PRERELEASE]]) {
    const result = repo.run('scripts/bump-version.mjs', version);
    assert.equal(result.status, 0, result.stderr);
    const changelog = repo.read('CHANGELOG.md');
    const [unreleased, release] = changelog.split(/\r?\n/).filter((line) => /^\[.*\]:/.test(line));
    assert.ok(unreleased.startsWith('[Unreleased]: '));
    assert.ok(unreleased.endsWith(`/compare/v${version}...HEAD`));
    assert.ok(release.startsWith(`[${version}]: `));
    assert.ok(release.endsWith(`/compare/v${from}...v${version}`));
    assert.ok(changelog.includes(`\n## [${version}] — `));
  }
});

for (const [name, path, edit, problem, version = NEXT] of [
  ['missing Unreleased heading', 'CHANGELOG.md', (text) => text.replace(/^## \[Unreleased\]\r?\n/m, ''),
    /CHANGELOG\.md: missing ## \[Unreleased\] heading/],
  ['missing Unreleased compare link', 'CHANGELOG.md', (text) => text.replace(/^\[Unreleased\]:[^\r\n]*\r?\n/m, ''),
    /CHANGELOG\.md: missing or invalid \[Unreleased\]: compare link/, PRERELEASE],
  ['missing Cargo member', 'Cargo.lock', (text) => text.replace(CORE_BLOCK, ''), /Cargo\.lock:.*mds-core.*missing/],
  ['duplicate Cargo member', 'Cargo.lock', (text) => text + REGISTRY_CORE, /Cargo\.lock:.*mds-core.*duplicate/],
  ['missing npm workspace', 'package-lock.json', (text) => {
    const lock = JSON.parse(text);
    delete lock.packages[NAPI_DIR];
    return serialize(lock);
  }, /package-lock\.json:.*crates\/mds-napi.*missing/],
  ['noncanonical npm lock format', 'package-lock.json', (text) => JSON.stringify(JSON.parse(text)), /package-lock\.json:.*format/],
  ['invalid new manifest', 'Cargo.toml', (text) => text.replace('[workspace.package]', '[workspace.metadata]'), /Cargo\.toml:.*workspace\.package/],
]) {
  test(`bump leaves all files intact on ${name}`, (t) => {
    const repo = fixture(t);
    const original = repo.read(path);
    const planted = edit(original);
    assert.notEqual(planted, original);
    repo.write(path, planted);
    const before = repo.files.map(repo.read);
    const result = repo.run('scripts/bump-version.mjs', version);
    assertRejected(result, problem);
    assert.deepEqual(repo.files.map(repo.read), before);
  });
}

test('lock member names match the Cargo workspace member names', () => {
  const workspace = read('Cargo.toml').split(/^\[/m).find((section) => section.startsWith('workspace]'));
  const members = /^[ \t]*members\s*=\s*\[([^\]]*)\]/m.exec(workspace ?? '');
  assert.ok(members, 'Cargo.toml: expected [workspace] members');
  const names = [...members[1].matchAll(/"([^"]+)"/g)].map(([, dir]) => {
    const pkg = read(`${dir}/Cargo.toml`).split(/^\[/m).find((section) => section.startsWith('package]'));
    const name = /^[ \t]*name\s*=\s*"([^"]+)"/m.exec(pkg ?? '');
    assert.ok(name, `${dir}/Cargo.toml: expected [package] name`);
    return name[1];
  });
  assert.deepEqual(LOCK_MEMBERS.toSorted(), names.toSorted());
});

test('import exposes pure lock checks and rewrites without CLI effects', (t) => {
  const repo = fixture(t);
  const imports = `
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import * as verify from './scripts/verify-versions.mjs';
import * as bump from './scripts/bump-version.mjs';
import * as manifest from './scripts/release-manifest.mjs';
const { LOCK_MEMBERS, PKG_DIRS, PKG_PATHS, CRATE_CARGO_PATHS } = manifest;
assert.equal(PKG_PATHS.length, 8);
assert.equal(CRATE_CARGO_PATHS.length, 4);
assert.equal(PKG_DIRS.length, 8);
const cargo = readFileSync('Cargo.lock', 'utf8');
const npm = readFileSync('package-lock.json', 'utf8');
const version = ${JSON.stringify(canonical())};
assert.deepEqual(verify.checkCargoLock(cargo, version, LOCK_MEMBERS), []);
assert.deepEqual(verify.checkPackageLock(npm, version, PKG_DIRS), []);
assert.equal(bump.rewriteCargoLock(cargo, version, LOCK_MEMBERS), cargo);
assert.deepEqual(bump.rewritePackageLock(npm, version, PKG_DIRS), { ok: true, text: npm });
const registry = ${JSON.stringify(REGISTRY_CORE)};
const revised = bump.rewriteCargoLock(cargo + registry, ${JSON.stringify(NEXT)}, LOCK_MEMBERS);
assert.equal(revised.slice(-registry.length), registry);
assert.equal(revised.match(/^name = "mds-core"\\nversion = "([^"]+)"/m)[1], ${JSON.stringify(NEXT)});
`;
  repo.write('import.mjs', imports);
  const before = repo.files.map(repo.read);
  const result = repo.run('import.mjs', NEXT);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout + result.stderr, '');
  assert.deepEqual(repo.files.map(repo.read), before);
});

test('both scripts use only builtin or relative imports and the shared manifest', () => {
  for (const name of ['verify-versions', 'bump-version']) {
    const text = read(`scripts/${name}.mjs`);
    const imports = [...text.matchAll(/\bfrom ['"]([^'"]+)['"]/g)].map(([, spec]) => spec);
    assert.ok(imports.includes('./release-manifest.mjs'), name);
    assert.ok(imports.every((spec) => /^(?:node:|\.\/)/.test(spec)), name);
    assert.ok(!imports.includes('node:child_process'), name);
  }
});

test('both workflows run verification as a required step in their existing jobs', () => {
  for (const [workflow, job] of [['ci', 'source-hygiene'], ['release', 'version-gate']]) {
    const text = read(`.github/workflows/${workflow}.yml`);
    const body = text.split(`\n  ${job}:\n`)[1]?.split(/\n  \S/)[0] ?? '';
    const steps = body.split(/\n      - /);
    const matched = steps.filter((step) => /^        run: node scripts\/verify-versions\.mjs$/m.test(step));
    assert.equal(matched.length, 1, `${workflow}: expected one plain version check in ${job}`);
    assert.doesNotMatch(matched[0], /^        (?:if|continue-on-error):/m);
  }
});
