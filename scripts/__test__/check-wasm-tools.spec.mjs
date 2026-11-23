import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { VERSION_TIMEOUT_MS, decideToolCheck, probeToolVersion } from '../check-wasm-tools.mjs';

const SCRIPT = new URL('../check-wasm-tools.mjs', import.meta.url);
const ROOT = new URL('../../', import.meta.url);
const PIN_FILE = '.github/actions/setup-wasm/action.yml';
const read = (path) => readFileSync(new URL(path, ROOT), 'utf8');
const PINS = `runs:
  using: composite
  steps:
    - uses: jetli/wasm-pack-action@sha
      with:
        version: v0.14.0
    - uses: phi-ag/setup-binaryen@sha
      with:
        version: 129
`;
const VERSION = (text) => `printf '%s\\r\\n' '${text}'`;
const SHELL_FIXTURES = { skip: process.platform === 'win32' && 'POSIX tool fixtures' };

function writeTool(bin, name, body) {
  writeFileSync(join(bin, name), `#!/bin/sh\n[ "$#" -eq 1 ] && [ "$1" = --version ] || exit 2\n${body}\n`, { mode: 0o700 });
}

function fixture(t, { pins = PINS, pack = VERSION('wasm-pack 0.14.0'), opt = VERSION('wasm-opt version 129 (version_129)') } = {}) {
  const root = mkdtempSync(join(tmpdir(), 'mds-wasm-tools-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  for (const dir of ['scripts', 'packages/mds-wasm', '.github/actions/setup-wasm', 'bin']) {
    mkdirSync(join(root, dir), { recursive: true });
  }
  const script = join(root, 'scripts/check-wasm-tools.mjs');
  copyFileSync(SCRIPT, script);
  if (pins !== null) writeFileSync(join(root, PIN_FILE), pins);
  for (const [name, body] of [['wasm-pack', pack], ['wasm-opt', opt]]) {
    if (body !== null) writeTool(join(root, 'bin'), name, body);
  }
  return (strict, cwd = '.', extraArgs = []) => spawnSync(process.execPath, [script, ...(strict ? ['--strict'] : []), ...extraArgs], {
    cwd: join(root, cwd),
    env: { ...process.env, PATH: join(root, 'bin') },
    encoding: 'utf8',
    timeout: 10000,
    killSignal: 'SIGKILL',
  });
}

function assertDiagnostic(run, pattern) {
  const warn = run(false);
  assert.equal(warn.status, 0, warn.stderr);
  const lines = warn.stderr.trim().split('\n');
  assert.equal(lines.length, 1, warn.stderr);
  assert.match(lines[0], /^WASM tool drift warning:/);
  assert.match(lines[0], pattern);
  assert.ok(lines[0].includes(PIN_FILE), warn.stderr);

  const strict = run(true);
  assert.equal(strict.status, 1, strict.stderr);
  assert.ok(strict.stdout.includes(`Node ${process.version}`), strict.stdout);
  assert.match(strict.stderr, /^WASM tool check failed:/);
  assert.match(strict.stderr, pattern);
}

test('strict mode accepts the CI version forms and CRLF pin file', SHELL_FIXTURES, (t) => {
  const run = fixture(t, { pins: PINS.replaceAll('\n', '\r\n') });
  const strict = run(true);
  assert.equal(strict.status, 0, strict.stderr);
  assert.ok(strict.stdout.includes(`Node ${process.version}`), strict.stdout);
  assert.match(strict.stdout, /wasm-pack 0\.14\.0.*wasm-opt 129/);
  assert.equal(strict.stderr, '');
});

test('warn mode finds the pins from the package folder', SHELL_FIXTURES, (t) => {
  const run = fixture(t, { opt: VERSION('wasm-opt version 117') });
  const warn = run(false, 'packages/mds-wasm');
  assert.equal(warn.status, 0, warn.stderr);
  assert.match(warn.stderr, /wasm-opt 117 does not match pin 129/);
  assert.ok(warn.stderr.includes(PIN_FILE), warn.stderr);
});

test('both tool problems appear in one diagnostic', SHELL_FIXTURES, (t) => {
  const run = fixture(t, { pack: VERSION('wasm-pack 0.15.0'), opt: null });
  assertDiagnostic(run, /wasm-pack 0\.15\.0 does not match pin 0\.14\.0; wasm-opt is absent from PATH/);
});

test('invalid options follow the exit rule for the requested mode', SHELL_FIXTURES, (t) => {
  const run = fixture(t);
  assertDiagnostic((strict) => run(strict, '.', ['--unknown']), /expected no options or --strict/);
});

const failures = [
  ['version mismatch', { opt: VERSION('wasm-opt version 117') }, /wasm-opt 117 does not match pin 129/],
  ['unparsable version', { opt: VERSION('unexpected output: 129') }, /cannot parse wasm-opt --version output/],
  ['absent wasm-opt', { opt: null }, /wasm-opt is absent from PATH; wasm-pack uses its bundled wasm-opt, not the CI version/],
  ['absent wasm-pack', { pack: null }, /wasm-pack is absent from PATH/],
  ['nonzero exit', { opt: "printf 'tool error\\n' >&2\nexit 7" }, /wasm-opt --version exited with status 7/],
  ['missing pin file', { pins: null }, /cannot read the pin file/],
  ['one pin', { pins: PINS.replace('        version: 129\n', '') }, /expected exactly two tool pins/],
  ['three pins', { pins: `${PINS}    - uses: extra/action@sha\n      with:\n        version: 130\n` }, /expected exactly two tool pins/],
  ['invalid pin', { pins: PINS.replace('v0.14.0', 'latest') }, /invalid wasm-pack pin/],
];
test('the version time limit allows a slow first start under Rosetta (#519)', () => {
  assert.ok(VERSION_TIMEOUT_MS >= 30_000, `limit ${VERSION_TIMEOUT_MS} ms is too short for a Rosetta first start`);
  assert.ok(VERSION_TIMEOUT_MS <= 120_000, `limit ${VERSION_TIMEOUT_MS} ms does not bound a hung tool`);
});

test('strict mode accepts a tool whose first start takes more than 3 s (#519)', SHELL_FIXTURES, (t) => {
  const strict = fixture(t, { pack: `/bin/sleep 4\n${VERSION('wasm-pack 0.14.0')}` })(true);
  assert.equal(strict.status, 0, strict.stderr);
  assert.match(strict.stdout, /WASM tools match pins: wasm-pack 0\.14\.0; wasm-opt 129/);
});

test('a hung tool times out and the strict check fails', SHELL_FIXTURES, (t) => {
  const bin = mkdtempSync(join(tmpdir(), 'mds-wasm-hang-'));
  t.after(() => rmSync(bin, { recursive: true, force: true }));
  writeTool(bin, 'wasm-opt', 'exec /bin/sleep 30');
  const started = Date.now();
  const hung = probeToolVersion('wasm-opt', { timeoutMs: 500, env: { PATH: bin } });
  assert.ok(Date.now() - started < 10_000, 'the time limit must stop the tool');
  assert.deepEqual(hung, { ok: false, error: 'wasm-opt --version timed out' });

  const pins = { 'wasm-pack': '0.14.0', 'wasm-opt': '129' };
  const decision = decideToolCheck(true, pins, { 'wasm-pack': { ok: true, value: '0.14.0' }, 'wasm-opt': hung });
  assert.equal(decision.exitCode, 1);
  assert.match(decision.stderr, /^WASM tool check failed: wasm-opt --version timed out \(pins: /);
});

for (const timeoutMs of [0, -1, 1.5, Number.NaN]) {
  test(`an invalid time limit ${timeoutMs} fails closed`, () => {
    assert.deepEqual(probeToolVersion('wasm-pack', { timeoutMs }), { ok: false, error: 'invalid wasm-pack --version time limit' });
  });
}

test('both local build hooks call the check in warn mode before the builds', () => {
  const shell = read('scripts/build-wasm.sh');
  const hook = 'node "$(dirname -- "${BASH_SOURCE[0]}")/check-wasm-tools.mjs"';
  assert.ok(shell.includes(`${hook}; then`), 'the shell hook must resolve the check from its own path');
  assert.ok(shell.indexOf(hook) < shell.indexOf('wasm-pack build'), 'the shell hook must precede the builds');
  const { scripts } = JSON.parse(read('packages/mds-wasm/package.json'));
  assert.match(scripts.build, /^node \.\.\/\.\.\/scripts\/check-wasm-tools\.mjs && wasm-pack build \S/);
});

test('the composite action calls the strict check after setup', () => {
  assert.match(read(PIN_FILE), /run: wasm-opt --version\r?\n(?:    #[^\r\n]*\r?\n)*    - name: [^\r\n]+\r?\n      shell: bash\r?\n      run: node "\$GITHUB_WORKSPACE\/scripts\/check-wasm-tools\.mjs" --strict(?:\r?\n|$)/);
});

for (const [name, options, diagnostic] of failures) {
  test(`warn mode continues and strict mode fails on ${name}`, SHELL_FIXTURES, (t) => {
    assertDiagnostic(fixture(t, options), diagnostic);
  });
}
