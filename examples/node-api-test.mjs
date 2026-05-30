import * as mds from '../packages/mds/dist/node.js';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));

await mds.init();
console.log(`Backend: ${mds.getBackend()}`);

const tests = [];
let passed = 0;
let failed = 0;

function test(name, fn) {
  tests.push({ name, fn });
}

function assert(condition, msg) {
  if (!condition) throw new Error(`Assertion failed: ${msg}`);
}

// ─── Test: compile simple string ─────────────────────────────────
test('compile simple string', () => {
  const result = mds.compile('---\nname: World\n---\nHello {name}!\n');
  assert(result.output.includes('Hello World!'), 'should interpolate variable');
  assert(result.warnings.length === 0, 'should have no warnings');
  assert(result.dependencies.length === 0, 'string compile has no deps');
});

// ─── Test: compile with runtime vars ─────────────────────────────
test('compile with vars override', () => {
  const result = mds.compile('---\nenv: dev\n---\nEnv: {env}\n', {
    vars: { env: 'production' },
  });
  assert(result.output.includes('Env: production'), 'vars should override frontmatter');
});

// ─── Test: compile file with imports ─────────────────────────────
test('compileFile with imports', async () => {
  const result = await mds.compileFile(
    resolve(__dirname, 'ai-agent/system-prompt.mds'),
  );
  assert(result.output.includes('DataBot'), 'should contain agent name');
  assert(result.output.includes('Safety Guidelines'), 'should include imported guardrails');
  assert(result.dependencies.length === 3, `expected 3 deps, got ${result.dependencies.length}`);
});

// ─── Test: compileFile with vars ─────────────────────────────────
test('compileFile with vars', async () => {
  const result = await mds.compileFile(
    resolve(__dirname, 'edge-cases/08_runtime_vars.mds'),
    { vars: { is_production: true, is_development: false, debug: true } },
  );
  assert(result.output.includes('Debug Mode Enabled'), 'debug should be enabled via vars');
  assert(result.output.includes('Production Checklist'), 'should show production section');
  assert(!result.output.includes('Development Notes'), 'should NOT show dev section');
});

// ─── Test: check valid file ──────────────────────────────────────
test('check valid file', async () => {
  const result = await mds.checkFile(
    resolve(__dirname, 'ai-agent/multi-turn-prompt.mds'),
  );
  assert(result.warnings.length === 0, 'should have no warnings');
});

// ─── Test: error handling with isMdsError ────────────────────────
test('error handling', () => {
  try {
    mds.compile('Hello {undefined_var}!');
    assert(false, 'should have thrown');
  } catch (err) {
    assert(mds.isMdsError(err), 'should be MDS error');
    assert(err.code === 'mds::undefined_var', `expected mds::undefined_var, got ${err.code}`);
  }
});

// ─── Test: complex template with nested data ─────────────────────
test('complex nested data', () => {
  const source = `---
users:
  - name: Alice
    active: true
  - name: Bob
    active: false
---
@for user in users:
@if user.active:
- {user.name} (active)
@end
@end
`;
  const result = mds.compile(source);
  assert(result.output.includes('Alice (active)'), 'should include active user');
  const body = result.output.split('---\n').slice(2).join('---\n');
  assert(!body.includes('Bob'), 'body should exclude inactive user');
});

// ─── Test: function definition and call ──────────────────────────
test('function definition and call', () => {
  const source = `---
---
@define greet(name, role):
Hello {name}, you are a {role}!
@end

{greet("Alice", "developer")}
`;
  const result = mds.compile(source);
  assert(result.output.includes('Hello Alice, you are a developer!'), 'should expand function');
});

// ─── Test: code block passthrough ────────────────────────────────
test('code block passthrough', () => {
  const source = '---\nlang: Python\n---\n\n```python\nx = {\"key\": \"value\"}\n```\n\nLanguage: {lang}\n';
  const result = mds.compile(source);
  assert(result.output.includes('{"key": "value"}'), 'braces in code block should be literal');
  assert(result.output.includes('Language: Python'), 'var outside code block should interpolate');
});

// ─── Test: escaped braces ────────────────────────────────────────
test('escaped braces', () => {
  const source = '---\nname: test\n---\nLiteral: \\{name\\} Interpolated: {name}\n';
  const result = mds.compile(source);
  assert(result.output.includes('Literal: {name}'), 'escaped braces should be literal');
  assert(result.output.includes('Interpolated: test'), 'non-escaped should interpolate');
});

// ─── Test: empty array loop ──────────────────────────────────────
test('empty array loop', () => {
  const source = '---\nitems: []\n---\nBefore\n@for item in items:\n{item}\n@end\nAfter\n';
  const result = mds.compile(source);
  assert(result.output.includes('Before'), 'should have content before loop');
  assert(result.output.includes('After'), 'should have content after loop');
  assert(!result.output.includes('undefined'), 'should not have undefined');
});

// ─── Run all tests ───────────────────────────────────────────────
console.log(`\nRunning ${tests.length} tests...\n`);

for (const { name, fn } of tests) {
  try {
    await fn();
    passed++;
    console.log(`  PASS  ${name}`);
  } catch (err) {
    failed++;
    console.log(`  FAIL  ${name}: ${err.message}`);
  }
}

console.log(`\n${passed} passed, ${failed} failed out of ${tests.length} tests`);
if (failed > 0) process.exit(1);
