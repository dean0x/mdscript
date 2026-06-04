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

// ─── Test: built-in string functions ─────────────────────────
test('built-in string functions', () => {
  const result = mds.compile(`---
name: "  hello world  "
greeting: "Hello, World!"
---
UPPER: {upper(name)}
LOWER: {lower(greeting)}
TRIM: [{trim(name)}]
REPLACE: {replace(greeting, "World", "MDS")}
STARTS: {starts_with(greeting, "Hello")}
ENDS: {ends_with(greeting, "World!")}
CONTAINS: {contains(greeting, "World")}
SLICE: [{slice(greeting, 0, 5)}]
`);
  assert(result.output.includes('UPPER:   HELLO WORLD'), 'upper should work');
  assert(result.output.includes('LOWER: hello, world!'), 'lower should work');
  assert(result.output.includes('TRIM: [hello world]'), 'trim should work');
  assert(result.output.includes('REPLACE: Hello, MDS!'), 'replace should work');
  assert(result.output.includes('STARTS: true'), 'starts_with should return true');
  assert(result.output.includes('ENDS: true'), 'ends_with should return true');
  assert(result.output.includes('CONTAINS: true'), 'contains should return true');
  assert(result.output.includes('SLICE: [Hello]'), 'slice should work');
});

// ─── Test: built-in array functions ─────────────────────────
test('built-in array functions', () => {
  const result = mds.compile(`---
fruits:
  - banana
  - apple
  - cherry
  - apple
csv: "red,green,blue"
---
SPLIT: {split(csv, ",")}
JOIN: {join(fruits, " | ")}
LENGTH: {length(fruits)}
FIRST: {first(fruits)}
LAST: {last(fruits)}
SORT: {sort(fruits)}
UNIQUE: {unique(fruits)}
REVERSE: {reverse(fruits)}
`);
  assert(result.output.includes('SPLIT: red, green, blue'), 'split should work');
  assert(result.output.includes('JOIN: banana | apple | cherry | apple'), 'join should work');
  assert(result.output.includes('LENGTH: 4'), 'length should work');
  assert(result.output.includes('FIRST: banana'), 'first should work');
  assert(result.output.includes('LAST: apple'), 'last should work');
  assert(result.output.includes('SORT: apple, apple, banana, cherry'), 'sort should work');
  assert(result.output.includes('UNIQUE: banana, apple, cherry'), 'unique should work');
  assert(result.output.includes('REVERSE: apple, cherry, apple, banana'), 'reverse should work');
});

// ─── Test: type conversion builtins ─────────────────────────
test('type conversion builtins', () => {
  const result = mds.compile(`---
num: 42
flag: true
nothing: null
numeric_str: "123"
---
S_NUM: [{string(num)}]
S_BOOL: [{string(flag)}]
S_NULL: [{string(nothing)}]
N_STR: {number(numeric_str)}
N_BOOL: {number(flag)}
N_NULL: {number(nothing)}
`);
  assert(result.output.includes('S_NUM: [42]'), 'string(num) should work');
  assert(result.output.includes('S_BOOL: [true]'), 'string(bool) should work');
  assert(result.output.includes('S_NULL: []'), 'string(null) should be empty');
  assert(result.output.includes('N_STR: 123'), 'number(str) should work');
  assert(result.output.includes('N_BOOL: 1'), 'number(true) should be 1');
  assert(result.output.includes('N_NULL: 0'), 'number(null) should be 0');
});

// ─── Test: default function arguments ───────────────────────
test('default function arguments', () => {
  const result = mds.compile(`---
---
@define greet(name, greeting = "Hello"):
{greeting}, {name}!
@end

@define badge(label, color = "blue", size = 3):
[{color}:{label}:{size}]
@end

DEFAULTS: {greet("Alice")}
OVERRIDE: {greet("Bob", "Hey")}
BADGE_DEF: {badge("v2")}
BADGE_PART: {badge("v2", "green")}
BADGE_FULL: {badge("v2", "red", 5)}
`);
  assert(result.output.includes('DEFAULTS: Hello, Alice!'), 'default arg should apply');
  assert(result.output.includes('OVERRIDE: Hey, Bob!'), 'explicit arg should override');
  assert(result.output.includes('BADGE_DEF: [blue:v2:3]'), 'all defaults should apply');
  assert(result.output.includes('BADGE_PART: [green:v2:3]'), 'partial override should work');
  assert(result.output.includes('BADGE_FULL: [red:v2:5]'), 'full override should work');
});

// ─── Test: default arg types (number, bool, null) ───────────
test('default arg types', () => {
  const result = mds.compile(`---
---
@define show_num(val = 42):
num:{val}
@end

@define show_bool(val = true):
bool:{val}
@end

@define show_null(val = null):
@if val:
has_val
@else:
null_val
@end
@end

{show_num()}
{show_bool()}
{show_null()}
{show_null("override")}
`);
  assert(result.output.includes('num:42'), 'default number arg should work');
  assert(result.output.includes('bool:true'), 'default bool arg should work');
  assert(result.output.includes('null_val'), 'default null arg should be falsy');
  assert(result.output.includes('has_val'), 'overridden null default should be truthy');
});

// ─── Test: logical AND operator ─────────────────────────────
test('logical AND operator', () => {
  const result = mds.compile(`---
a: true
b: true
c: false
---
@if a && b:
BOTH_TRUE
@end
@if a && c:
SHOULD_NOT_APPEAR
@else:
AND_FALSE
@end
`);
  assert(result.output.includes('BOTH_TRUE'), 'AND with both true should render');
  assert(!result.output.includes('SHOULD_NOT_APPEAR'), 'AND with one false should not render');
  assert(result.output.includes('AND_FALSE'), 'AND false branch should render');
});

// ─── Test: logical OR operator ──────────────────────────────
test('logical OR operator', () => {
  const result = mds.compile(`---
a: true
b: false
c: false
---
@if a || b:
OR_TRUE
@end
@if b || c:
SHOULD_NOT_APPEAR
@else:
OR_FALSE
@end
`);
  assert(result.output.includes('OR_TRUE'), 'OR with one true should render');
  assert(!result.output.includes('SHOULD_NOT_APPEAR'), 'OR with both false should not render');
  assert(result.output.includes('OR_FALSE'), 'OR false branch should render');
});

// ─── Test: operator precedence (AND binds tighter) ──────────
test('operator precedence', () => {
  const result = mds.compile(`---
a: true
b: false
c: false
---
@if b && c || a:
PREC_PASS
@end
@if a || b && c:
PREC_PASS2
@end
`);
  assert(result.output.includes('PREC_PASS'), '(false && false) || true should be true');
  assert(result.output.includes('PREC_PASS2'), 'true || (false && false) should be true');
});

// ─── Test: chaining built-in functions ──────────────────────
test('chaining builtins', () => {
  const result = mds.compile(`---
---
CHAIN1: {upper(trim("  hello  "))}
CHAIN2: {join(sort(split("cherry,apple,banana", ",")), " < ")}
CHAIN3: {upper(replace("hello world", "world", "mds"))}
CHAIN4: {reverse(slice("abcdefgh", 2, 6))}
CHAIN5: {length(split("a,b,c,d,e", ","))}
CHAIN6: {first(reverse(split("a,b,c", ",")))}
`);
  assert(result.output.includes('CHAIN1: HELLO'), 'upper(trim()) should chain');
  assert(result.output.includes('CHAIN2: apple < banana < cherry'), 'sort(split()) should chain');
  assert(result.output.includes('CHAIN3: HELLO MDS'), 'upper(replace()) should chain');
  assert(result.output.includes('CHAIN4: fedc'), 'reverse(slice()) should chain');
  assert(result.output.includes('CHAIN5: 5'), 'length(split()) should chain');
  assert(result.output.includes('CHAIN6: c'), 'first(reverse(split())) should chain');
});

// ─── Test: builtins with logical operators ──────────────────
test('builtins with logical operators in same template', () => {
  const result = mds.compile(`---
admin: true
active: true
name: "Alice"
---
@define user_line(user_name, role = "member"):
@if admin && active:
{upper(user_name)}: {role}
@end
@end

{user_line(name)}
{user_line("Bob", "admin")}
`);
  assert(result.output.includes('ALICE: member'), 'default arg + logical + builtin should work');
  assert(result.output.includes('BOB: admin'), 'explicit arg should override default');
});

// ─── Test: compileFile with v0.2.0 edge-case templates ──────
test('compileFile: builtin string functions', async () => {
  const result = await mds.compileFile(
    resolve(__dirname, 'edge-cases/16_builtin_string_functions.mds'),
  );
  assert(result.output.includes('UPPER:   HELLO WORLD'), 'upper via file');
  assert(result.output.includes('TRIM: [hello world]'), 'trim via file');
  assert(result.output.includes('REPLACE: Hello, MDS!'), 'replace via file');
});

test('compileFile: default arguments', async () => {
  const result = await mds.compileFile(
    resolve(__dirname, 'edge-cases/19_default_arguments.mds'),
  );
  assert(result.output.includes('Hello, Alice!'), 'default arg via file');
  assert(result.output.includes('Hey, Bob!'), 'override arg via file');
  assert(result.output.includes('[blue:v0.2.0:3]'), 'multi-default via file');
});

test('compileFile: logical operators', async () => {
  const result = await mds.compileFile(
    resolve(__dirname, 'edge-cases/20_logical_operators.mds'),
  );
  assert(!result.output.includes('FAIL'), 'no FAIL lines in logical operators template');
  const passCount = (result.output.match(/PASS/g) || []).length;
  assert(passCount >= 10, `expected >=10 PASS lines, got ${passCount}`);
});

test('compileFile: chaining builtins', async () => {
  const result = await mds.compileFile(
    resolve(__dirname, 'edge-cases/21_chaining_builtins.mds'),
  );
  assert(result.output.includes('TYPESCRIPT'), 'chain upper(trim(first(split()))) via file');
  assert(result.output.includes('apple < banana < cherry'), 'chain sort+join via file');
  assert(result.output.includes('HELLO MDS'), 'chain replace+upper via file');
});

test('compileFile: combined v2 features', async () => {
  const result = await mds.compileFile(
    resolve(__dirname, 'edge-cases/22_combined_v2_features.mds'),
  );
  assert(result.output.includes('[ADMIN] ALICE'), 'combined: user badge with logical+builtin');
  assert(result.output.includes('go, python, rust, typescript'), 'combined: unique+sort+join');
  assert(result.output.includes('Tag count: 4'), 'combined: length(unique())');
  assert(result.output.includes('Account suspended: charlie'), 'combined: inactive branch');
});

// ─── Tests: expression directives (issue #74) ───────────────────
test('expression @if: function call truthy', () => {
  const result = mds.compile(`---
tags:
  - rust
  - go
---
@if contains(tags, "rust"):
yes
@else:
no
@end
`);
  assert(result.output.includes('yes'), '@if contains() should be truthy');
});

test('expression @if: negated function call', () => {
  const result = mds.compile(`---
name: alice
---
@if !starts_with(name, "z"):
yes
@else:
no
@end
`);
  assert(result.output.includes('yes'), '@if !starts_with() should be truthy for non-z name');
});

test('expression @if: comparison with expression on both sides', () => {
  const result = mds.compile(`---
a: Alice
b: ALICE
---
@if lower(a) == lower(b):
match
@else:
no-match
@end
`);
  assert(result.output.includes('match'), '@if lower(a)==lower(b) should match');
});

test('expression @for: function call iterable', () => {
  const result = mds.compile(`---
csv: "x,y,z"
---
@for item in split(csv, ","):
- {item}
@end
`);
  assert(result.output.includes('- x'), '@for split iterable should produce x');
  assert(result.output.includes('- y'), '@for split iterable should produce y');
  assert(result.output.includes('- z'), '@for split iterable should produce z');
});

test('expression @for: nested calls (sort+unique)', () => {
  const result = mds.compile(`---
tags:
  - b
  - a
  - b
---
@for t in sort(unique(tags)):
- {t}
@end
`);
  const lines = result.output.split('\n').filter(l => l.startsWith('- '));
  assert(lines.length === 2, `should have 2 unique items, got ${lines.length}`);
  assert(result.output.includes('- a'), 'sorted unique should include a');
  assert(result.output.includes('- b'), 'sorted unique should include b');
});

test('expression @if: logical AND with function calls', () => {
  const result = mds.compile(`---
text: grunge
---
@if contains(text, "g") && contains(text, "r"):
yes
@else:
no
@end
`);
  assert(result.output.includes('yes'), '@if && with calls should work');
});

test('expression @if: error cases (undefined function)', () => {
  try {
    mds.compile('@if notabuiltin(x):\nyes\n@end\n');
    assert(false, 'should have thrown');
  } catch {
    // expected
  }
});

test('compileFile: expression directives', async () => {
  const result = await mds.compileFile(
    resolve(__dirname, 'edge-cases/23_expression_directives.mds'),
  );
  assert(result.output.includes('Has rust tag'), 'expression @if contains should work');
  assert(result.output.includes('Admin access granted'), 'expression @if lower==admin should work');
  assert(result.output.includes('- a'), '@for split iterable should work');
  assert(result.output.includes('- go'), '@for sort(unique) should produce sorted results');
});

test('compileFile: colon in string args', async () => {
  const result = await mds.compileFile(
    resolve(__dirname, 'edge-cases/24_colon_in_string_args.mds'),
  );
  assert(result.output.includes('Path contains usr:local'), 'colon in string arg for @if');
  assert(result.output.includes('- usr'), '@for with colon separator should work');
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
