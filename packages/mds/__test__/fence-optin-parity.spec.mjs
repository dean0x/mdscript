import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { test } from 'node:test';

const MDS_URL = new URL('../dist/node.js', import.meta.url).href;
const ADDON = new URL('../../../crates/mds-napi/mds-napi.node', import.meta.url);

// Backend choice happens at module load, so each leg needs its own process.
function runLeg(backend, source) {
  if (backend === 'native' && !existsSync(ADDON)) {
    throw new Error('Native addon missing. Run npm run build:native -w @mdscript/mds-napi.');
  }
  const script = `
    const mds = await import(${JSON.stringify(MDS_URL)});
    const source = ${JSON.stringify(source)};
    await mds.init();
    console.log(JSON.stringify({
      backend: mds.getBackend(),
      output: mds.compile(source).output,
      unmarked: mds.compile(source.replace(' +mds', '')).output,
    }));
  `;
  const proc = spawnSync(process.execPath, ['--input-type=module', '-e', script], {
    env: { ...process.env, MDS_BACKEND: backend },
    encoding: 'utf8',
  });
  if (proc.error) throw proc.error;
  assert.equal(proc.status, 0, `${backend} leg failed:\n${proc.stderr}`);
  const result = JSON.parse(proc.stdout);
  assert.equal(result.backend, backend);
  return result;
}

test('native and WASM emit the same fence and indented conditional output', () => {
  const source = readFileSync(new URL('./fixtures/fence_optin.mds', import.meta.url), 'utf8');
  const expected = readFileSync(new URL('./fixtures/fence_optin.expected.md', import.meta.url), 'utf8')
    .replaceAll('\r\n', '\n');
  const native = runLeg('native', source);
  const wasm = runLeg('wasm', source);

  assert.equal(wasm.output, native.output, 'backend output differs');
  assert.equal(native.output, expected, 'output differs from the shared expected file');
  assert.equal(wasm.unmarked, native.unmarked);
  assert.notEqual(native.output, native.unmarked);
  assert.ok(native.unmarked.includes('  @if 1 == 2:\n'));
  assert.ok(native.output.includes('```text\nfence selected arm\n```\n'));
});
