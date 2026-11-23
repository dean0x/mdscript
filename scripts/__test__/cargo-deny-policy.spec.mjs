import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readdirSync, readFileSync } from 'node:fs';
import { test } from 'node:test';
import { ADVISORIES_CONTEXT, DEPENDENCY_SURFACE, EXPECTED_CONTEXTS } from '../verify-pr-checks.mjs';

// Policy reader grammar (#136): blank lines, comments, known table headers,
// bare key assignments, unescaped basic strings, booleans, decimal numbers,
// and arrays of strings or inline tables. Arrays can span lines. Inline tables
// fit on a single line and can hold a string array. Only an unquoted # starts
// a comment. All other syntax, duplicate tables, and duplicate keys fail.
const VERSION = '0.20.2';
const REQUIRED_CONTEXT = 'Rust — cargo-deny licenses, bans, sources';
const CI_WORKFLOW = '.github/workflows/ci.yml';
const ADVISORIES_WORKFLOW = '.github/workflows/deny-advisories.yml';
const CRATES_IO = 'https://github.com/rust-lang/crates.io-index';
const RUSTSEC = 'https://github.com/rustsec/advisory-db';
const LICENSES = ['MIT', 'Apache-2.0', 'ISC'];
const EXCEPTIONS = {
  notify: 'CC0-1.0',
  'unicode-ident': 'Unicode-3.0',
  foldhash: 'Zlib',
  'target-lexicon': 'Apache-2.0 WITH LLVM-exception',
};
const SKIPS = ['syn@2.0.117', 'unicode-width@0.1.14', 'windows-sys@0.60.2', 'wit-bindgen@0.51.0'];
const SHAPE = Symbol('shape');
const POLICY = {
  graph: { 'all-features': true, 'no-default-features': false, features: [], 'exclude-dev': false },
  output: { 'feature-depth': 1 },
  advisories: {
    'db-path': '$CARGO_HOME/advisory-dbs', 'db-urls': [RUSTSEC], 'git-fetch-with-cli': false,
    'disable-yank-checking': false, 'maximum-db-staleness': 'P7D', unsound: 'all', yanked: 'deny',
    unmaintained: 'workspace', 'unused-ignored-advisory': 'warn', ignore: SHAPE,
  },
  licenses: {
    version: 2, 'include-dev': true, 'include-build': true, 'unused-allowed-license': 'warn',
    'unused-license-exception': 'allow', allow: SHAPE, 'confidence-threshold': 0.8,
    clarify: [], exceptions: SHAPE,
  },
  'licenses.private': { ignore: false, registries: [], 'ignore-sources': [] },
  bans: {
    'multiple-versions': 'warn', 'multiple-versions-include-dev': true, wildcards: 'deny',
    'allow-wildcard-paths': false, highlight: 'all', 'workspace-default-features': 'allow',
    'external-default-features': 'allow', allow: [], 'allow-workspace': false, deny: [], features: [],
    skip: SHAPE, 'skip-tree': [],
  },
  'bans.workspace-dependencies': { duplicates: 'deny', 'include-path-dependencies': false, unused: 'deny' },
  sources: {
    'unknown-registry': 'deny', 'unknown-git': 'deny', 'allow-registry': [CRATES_IO], 'allow-git': [],
    private: [], 'required-git-spec': 'rev', 'unused-allowed-source': 'warn', 'unused-allowed-org': 'warn',
  },
  'sources.allow-org': { github: [], gitlab: [], bitbucket: [] },
};

const BARE = /^[A-Za-z0-9_-]+$/;
const BASIC = /^"[^"\\\x00-\x1f]*"$/;
const read = file => readFileSync(new URL(`../../${file}`, import.meta.url), 'utf8');
const exactSet = (actual, expected, label) => assert.deepEqual([...actual].sort(), [...expected].sort(), label);

function readPolicy(text) {
  const lines = text.split(/\r?\n/);
  const tokens = lines.flatMap((line, index) => [...line.matchAll(
    /"[^"\\\x00-\x1f]*"|#.*|[+-]?\d+(?:\.\d+)?(?![A-Za-z0-9_-])|[A-Za-z0-9_-]+|[^ \t]/g,
  )].filter(([part]) => !part.startsWith('#')).map(([part]) => ({ part, line: index })));
  let pos = 0;
  const peek = () => tokens[pos]?.part;
  function take(expected) {
    const token = tokens[pos++];
    assert.ok(token && (expected === undefined || token.part === expected), `expected ${expected ?? 'a token'}`);
    return token;
  }
  function string() {
    const { part } = take();
    assert.match(part, BASIC, 'expected a basic string');
    return part.slice(1, -1);
  }
  function list(close, item, trailingComma) {
    const result = [];
    for (let i = 0; i <= tokens.length; i++) {
      if (peek() === close) {
        take(close);
        return result;
      }
      result.push(item());
      if (peek() === close) continue;
      take(',');
      if (!trailingComma) assert.notEqual(peek(), close, 'unexpected trailing comma in inline table');
    }
    assert.fail(`expected ${close}`);
  }
  function inlineTable() {
    const { line } = take('{');
    const fields = new Map();
    list('}', () => {
      const { part: key } = take();
      assert.match(key, BARE, 'expected a bare inline key');
      assert.ok(!fields.has(key), `duplicate inline key ${key}`);
      take('=');
      if (peek() === '[') {
        take('[');
        fields.set(key, list(']', string, true));
      } else {
        fields.set(key, scalar());
      }
    }, false);
    assert.equal(tokens[pos - 1].line, line, 'inline table must fit on one line');
    const priorLine = lines[line].trimStart().startsWith('{') ? lines[line - 1] : '';
    return { value: Object.fromEntries(fields), comment: /^\s*#\s*(\S.*)$/.exec(priorLine ?? '')?.[1] };
  }
  function scalar() {
    if (BASIC.test(peek())) return string();
    const { part } = take();
    if (part === 'true' || part === 'false') return part === 'true';
    assert.match(part, /^[+-]?(?:0|[1-9]\d*)(?:\.\d+)?$/, 'expected a basic string, boolean, or decimal number');
    return Number(part);
  }

  const tables = new Map();
  let table;
  for (let i = 0; i < tokens.length && pos < tokens.length; i++) {
    const { part: start, line } = take();
    if (start === '[') {
      const name = /^\s*\[([A-Za-z-]+(?:\.[A-Za-z-]+)?)\]\s*(?:#.*)?$/.exec(lines[line])?.[1];
      assert.ok(Object.hasOwn(POLICY, name), `unknown or invalid table: ${lines[line]}`);
      assert.ok(!tables.has(name), `duplicate table ${name}`);
      table = new Map();
      tables.set(name, table);
      for (let next = pos; next < tokens.length && tokens[next].line === line; next++) pos = next + 1;
      continue;
    }
    assert.ok(table, 'assignments require a policy table');
    assert.match(start, BARE, 'expected a bare key');
    assert.ok(!table.has(start), `duplicate key ${start}`);
    assert.equal(take('=').line, line, 'assignment must start on one line');
    assert.equal(tokens[pos]?.line, line, 'value must start on the assignment line');
    if (peek() === '[') {
      take('[');
      table.set(start, list(']', () => peek() === '{' ? inlineTable() : { value: string() }, true));
    } else {
      table.set(start, scalar());
    }
    assert.notEqual(tokens[pos]?.line, tokens[pos - 1].line, 'unexpected text after value');
  }
  return tables;
}

function checkIgnore(entry, today) {
  const { value } = entry;
  assert.deepEqual(Object.keys(value).sort(), ['id', 'reason'], 'ignore must contain only id and reason');
  assert.match(value.id, /^RUSTSEC-\d{4}-\d{4}$/, 'ignore needs one advisory ID');
  const until = /^#[1-9]\d* until (\d{4}-\d{2}-\d{2}): \S.*$/.exec(value.reason)?.[1];
  assert.ok(until, 'ignore reason needs an issue number, an until date and text');
  const date = new Date(`${until}T00:00:00.000Z`);
  assert.ok(Number.isFinite(date.getTime()) && date.toISOString().slice(0, 10) === until, 'invalid until date');
  assert.ok(until >= today, 'ignore expiry date is past');
}

function checkShape(table, key, rows, today) {
  assert.ok(Array.isArray(rows), `${table}.${key} must be an array`);
  if (key === 'allow') {
    exactSet(rows.map(({ value }) => value), LICENSES, 'global license list');
    return;
  }
  for (const row of rows) {
    const { value, comment } = row;
    assert.equal(typeof value, 'object', `${table}.${key} entries must be inline tables`);
    if (key === 'ignore') checkIgnore(row, today);
    if (key === 'exceptions') {
      exactSet(Object.keys(value), ['crate', 'allow'], 'license exception fields');
      assert.ok(Object.hasOwn(EXCEPTIONS, value.crate), 'license exception must name one approved crate');
      assert.deepEqual(value.allow, [EXCEPTIONS[value.crate]], 'crate license exception');
      assert.ok(comment, 'license exception needs a reason comment on the previous line');
    }
    if (key === 'skip') {
      exactSet(Object.keys(value), ['crate', 'reason'], 'duplicate skip fields');
      assert.match(value.reason, /\S/, 'duplicate skip needs a reason');
    }
  }
  if (key === 'exceptions') exactSet(rows.map(row => row.value.crate), Object.keys(EXCEPTIONS), 'exception crates');
  if (key === 'skip') exactSet(rows.map(row => row.value.crate), SKIPS, 'versioned duplicate skips');
}

function checkPolicy(text, today) {
  const tables = readPolicy(text);
  exactSet(tables.keys(), Object.keys(POLICY), 'policy tables');
  for (const [name, expected] of Object.entries(POLICY)) {
    const table = tables.get(name);
    const optional = name === 'advisories' && !table.has('db-urls');
    exactSet(table.keys(), Object.keys(expected).filter(key => !optional || key !== 'db-urls'), `${name} keys`);
    for (const [key, actual] of table) {
      const pinned = expected[key];
      if (pinned === SHAPE) checkShape(name, key, actual, today);
      else assert.deepEqual(Array.isArray(actual) ? actual.map(row => row.value) : actual, pinned, `${name}.${key}`);
    }
  }
}

// Read the block mappings used by these workflows. Match keys at their own
// indentation so a step property or a shell heredoc cannot stand in for a job key.
function blocks(text, depth) {
  const entries = [...text.matchAll(new RegExp(`^ {${depth}}([^\\s#].*)$`, 'gm'))];
  const fields = new Map();
  for (const [index, entry] of entries.entries()) {
    const keyValue = /^([\w-]+):[ \t]*(.*)$/.exec(entry[1]);
    assert.ok(keyValue, 'expected a bare YAML mapping key');
    const [, key, value] = keyValue;
    assert.ok(!fields.has(key), `duplicate YAML key ${key}`);
    fields.set(key, { value: value.trimEnd(), body: text.slice(entry.index + entry[0].length, entries[index + 1]?.index) });
  }
  return fields;
}
const jobs = text => blocks(blocks(text, 0).get('jobs')?.body ?? '', 2);
const steps = job => job.split(/^      - /m).slice(1);
const shellLines = run => run.split('\n').map(line => line.trim()).filter(line => line && !line.startsWith('#'));
const command = checks => `cargo deny --locked --config deny.toml check ${checks}`;
const PREFLIGHT = command('advisories licenses bans sources');
const SANITIZE_LOG = String.raw`sed -e 's/::/%3A%3A/g' -e 's/##\[/%23%23%5B/g' "$log"`;
const logAssignment = checks => `log="$RUNNER_TEMP/cargo-deny-${checks === 'advisories' ? 'advisories' : 'policy'}.log"`;

function checkNames(text, contexts, isCi) {
  const owners = new Set();
  for (const { body } of jobs(text).values()) {
    const fields = blocks(body, 4);
    const name = fields.get('name')?.value.replace(/\s+#.*$/, '').replace(/^(['"])(.*)\1$/, '$2');
    if (contexts.includes(name)) {
      assert.ok(isCi && !owners.has(name), `required context needs one CI job: ${name}`);
      owners.add(name);
    }
    // GitHub adds matrix suffixes to run names, not workflow job names.
    assert.ok(!contexts.some(context => name?.startsWith(`${context} (`)), `job name shadows required context: ${name}`);
  }
}

function checkGate(run, checks) {
  const lines = shellLines(run);
  const gate = lines.filter(line => /^cargo deny\b/.test(line));
  assert.equal(gate.length, 1, 'expected one cargo deny gate command');
  assert.doesNotMatch(gate[0], /(?:^|\s)(?:-A|-t|--(?:exclude(?:-dev|-unpublished)?|metadata-path|disable-fetch|offline|frozen))(?=[\s=]|$)/,
    'gate cannot narrow the graph or disable fetch');
  assert.deepEqual(lines.slice(0, 2), ['set -euo pipefail', 'test -f deny.toml'], 'gate preconditions');
  assert.equal(gate[0], `${command(checks)} > "$log" 2>&1 || rc=$?`, 'locked check command and status capture');
  assert.equal(lines.at(-1), 'exit "$rc"', 'propagate the gate exit code');
  assert.deepEqual(lines.slice(2, -1), [
    'cargo --version > "$RUNNER_TEMP/cargo-version.log"', logAssignment(checks), 'rc=0', gate[0], SANITIZE_LOG,
  ], 'gate runs without extra commands');
}

function checkInstall(install, releasePin) {
  const fields = blocks(install, 8);
  exactSet(fields.keys(), ['uses', 'env', 'with'], 'install step keys');
  assert.equal(/^taiki-e\/install-action@([a-f0-9]{40}) {2}# \S+$/.exec(fields.get('uses').value)?.[1], releasePin, 'install-action SHA');
  assert.equal(fields.get('env').body.trim(), 'GITHUB_TOKEN: ${{ github.token }}');
  const inputs = fields.get('with');
  assert.equal(inputs.value, '', 'install inputs must be a block');
  assert.equal(inputs.body.trim(), `tool: cargo-deny@${VERSION}\n          fallback: none`, 'install version, fallback and default checksum');
}

function checkJobFields(job, name) {
  const fields = blocks(job, 4);
  exactSet(fields.keys(), ['name', 'runs-on', 'timeout-minutes', 'steps'], 'dependency job keys');
  assert.equal(fields.get('name').value, name);
  assert.equal(fields.get('runs-on').value, 'ubuntu-latest');
  assert.equal(fields.get('timeout-minutes').value, '10');
}

function checkJob(job, name, checks, releasePin) {
  checkJobFields(job, name);
  assert.doesNotMatch(job, /rust-cache|\bsecrets\s*(?:\.|\[)|(?:if|continue-on-error):/, 'dependency job cannot skip checks or use cache or secrets');
  const jobSteps = steps(job);
  assert.equal(jobSteps.length, 5, 'expected five dependency job steps');
  const [checkout, install, version, gate, control] = jobSteps;
  for (const runStep of [version, gate, control]) {
    exactSet(blocks(runStep, 8).keys(), ['run'], 'dependency run step keys');
  }
  assert.match(checkout.trimEnd(), /^uses: actions\/checkout@v\d+\n        with:\n          persist-credentials: false$/);
  checkInstall(install, releasePin);
  const versionRun = blocks(version, 8).get('run');
  assert.equal(versionRun.value, '|');
  assert.deepEqual(versionRun.body.trim().split('\n').map(line => line.trim()), [
    'set -euo pipefail',
    'version_log="$RUNNER_TEMP/cargo-deny-version.log"',
    String.raw`printf 'cargo-deny 9.9.9\n' > "$version_log"`,
    'rc=0',
    `grep -Fqx 'cargo-deny ${VERSION}' "$version_log" || rc=$?`,
    'test "$rc" -eq 1',
    'cargo-deny --version > "$version_log"',
    'test "$(wc -l < "$version_log")" -eq 1',
    `grep -Fx 'cargo-deny ${VERSION}' "$version_log"`,
  ], 'reject the control and require the exact tool version');
  assert.match(gate, /^        run: \|$/m, 'expected the gate run block');
  checkGate(gate.split(/^        run: \|$/m)[1], checks);
  return control;
}

function checkCiChecks(ci, control) {
  for (const [id, { body }] of jobs(ci)) {
    for (const step of steps(body)) {
      if (id === 'cargo-deny' && step === control) continue;
      const run = blocks(`        ${step}`, 8).get('run');
      if (!run) continue;
      const inline = run.value.replace(/^(['"])(.*)\1$/, '$2');
      for (const line of shellLines(`${inline}\n${run.body}`.replace(/\\\n/g, ''))) {
        const invocation = /^(?:cargo[ \t]+deny|cargo-deny)[ \t]+([^#;&|<>]*)/.exec(line);
        if (!invocation) continue;
        const args = invocation[1].trim().split(/\s+/);
        const check = args.indexOf('check');
        if (check === -1) continue;
        // An omitted list selects all checks.
        const checks = args.slice(check + 1);
        assert.ok(checks.length > 0 && checks.every(name => /^(?:licenses|bans|sources)$/.test(name)),
          `CI must not check advisories: ${id}`);
      }
    }
  }
}

function checkTriggers(ci, advisories, ciControl) {
  const ciTriggers = blocks(blocks(ci, 0).get('on').body, 2);
  assert.equal(ciTriggers.get('pull_request')?.value, '', 'CI must run on every pull request');
  assert.equal(ciTriggers.get('pull_request')?.body.replace(/^\s*#.*$/gm, '').trim(), '', 'no CI pull request filter');
  assert.equal(ciTriggers.get('push')?.body.trim(), 'branches: [main]', 'CI must run on every main push');
  assert.ok(!ciTriggers.has('pull_request_target'), 'CI cannot use pull_request_target');
  checkCiChecks(ci, ciControl);

  const workflow = blocks(advisories, 0);
  exactSet(workflow.keys(), ['name', 'on', 'permissions', 'jobs'], 'advisories workflow keys');
  assert.equal(workflow.get('permissions').body.trim(), 'contents: read');
  const events = blocks(workflow.get('on').body, 2);
  exactSet(events.keys(), ['schedule', 'workflow_dispatch', 'pull_request'], 'advisories triggers');
  assert.equal(events.get('workflow_dispatch').value + events.get('workflow_dispatch').body.trim(), '');
  assert.match(events.get('schedule').body.trim(), /^- cron: '(?:[0-5]?\d) (?:[01]?\d|2[0-3]) \* \* \*'$/, 'one daily cron');
  const pr = events.get('pull_request');
  assert.equal(pr.value, '');
  const paths = blocks(pr.body, 4);
  exactSet(paths.keys(), ['paths'], 'pull request filters');
  const pathBlock = paths.get('paths');
  assert.equal(pathBlock.value, '');
  const pathLines = pathBlock.body.trim().split('\n').map(line => {
    assert.match(line.trim(), /^- (?:"[^"\n]+"|'[^'\n]+'|[^\s"']+)$/, 'advisories path entry');
    return line.trim().slice(2).replace(/^(['"])(.*)\1$/, '$2');
  });
  exactSet(pathLines, DEPENDENCY_SURFACE, 'advisories paths and verifier surface');
}

function checkControlMiss(lines, branch, label) {
  const start = lines.indexOf(branch[0]);
  assert.ok(start >= 0, label);
  assert.deepEqual(lines.slice(start, start + branch.length), branch, label);
}

function checkControls(policy, advisories) {
  const [policyLines, advisoryLines] = [policy, advisories].map(step => {
    const run = blocks(step, 8).get('run');
    assert.equal(run?.value, '|', 'expected a control run block');
    const lines = shellLines(run.body);
    assert.equal(lines[0], 'set -euo pipefail', 'control shell options');
    return lines;
  });
  assert.deepEqual(policyLines.slice(-3), [
    "expect_failure licenses 4 'error[rejected]'",
    "expect_failure bans 2 'error[banned]'",
    "expect_failure sources 8 'error[source-not-allowed]'",
  ], 'policy control calls');
  checkControlMiss(policyLines, [
    'if [[ "$rc" -ne "$expected_status" ]] || ! grep -Fq -- "$diagnostic" "$log"; then',
    String.raw`printf '%s control: expected exit %s and %s; got exit %s\n' "$check" "$expected_status" "$diagnostic" "$rc" >&2`,
    `${SANITIZE_LOG} >&2`, 'return 1', 'fi',
  ], 'policy control must fail on a miss');
  assert.match(advisories, /^          git -C "\$db" rev-parse --verify 'HEAD\^\{commit\}'$/m, 'verify a populated database');
  assert.match(advisories, /cat > "\$fixture\/Cargo.toml" <<'EOF'\n {10}\[workspace\]/);
  assert.match(advisories, /cat > "\$fixture\/Cargo.lock" <<'EOF'\n {10}version = 4/);
  checkControlMiss(advisoryLines, [
    `if [[ "$rc" -ne 1 ]] || ! grep -Fq 'error[vulnerability]' "$log" || ! grep -Fq "ID: $expected_id" "$log"; then`,
    String.raw`printf 'Advisory control: expected exit 1 and %s; got exit %s\n' "$expected_id" "$rc" >&2`,
    `${SANITIZE_LOG} >&2`, 'exit 1', 'fi',
  ], 'advisory control must fail on a miss');
  for (const [lines, failure, label] of [[policyLines, 'return 1', 'policy'], [advisoryLines, 'exit 1', 'advisory']]) {
    assert.deepEqual(lines.filter(line => /^(?:exit|return)\b/.test(line)), [failure], `${label} control must not exit early`);
  }
  assert.doesNotMatch(advisories, /gh issue|issues:\s*write/);
}

for (const [name, fixture, diagnostic] of [
  ['quoted keys', '"x" = true', /bare key/],
  ['dotted keys', 'x.y = true', /expected =/],
  ['literal strings', "x = 'true'", /basic string/],
  ['multi-line strings', 'x = """\nvalue\n"""', /unexpected text/],
  ['escape sequences', 'x = "bad\\tescape"', /basic string/],
  ['inline tables outside arrays', 'x = { key = "value" }', /basic string/],
  ['array-of-tables headers', '[[graph]]', /invalid table/],
  ['duplicate keys', 'x = true\nx = false', /duplicate key/],
  ['duplicate tables', '[graph]', /duplicate table/],
  ['duplicate inline keys', 'x = [{ id = "a", id = "b" }]', /duplicate inline key/],
  ['unknown table names', '[unlisted]', /unknown or invalid table/],
  ['unclassified lines', 'x', /expected =/],
]) {
  test(`the policy reader rejects ${name}`, () => {
    assert.throws(() => readPolicy(`[graph]\n${fixture}`), diagnostic);
  });
}

test('the policy reader keeps a hash inside a string and a reason on the prior line', () => {
  const parsed = readPolicy('[advisories]\nignore = [\n# Reason for this record\n{ reason = "#1 until 2040-02-29: test" }, # trailing\n]');
  assert.deepEqual(parsed.get('advisories').get('ignore'), [
    { value: { reason: '#1 until 2040-02-29: test' }, comment: 'Reason for this record' },
  ]);
});

const TODAY = '2040-02-29';
const ignore = reason => ({ value: { id: ['RUSTSEC', '2040', '0001'].join('-'), reason } });
for (const [name, reason, diagnostic] of [
  ['no reason', undefined, /ignore reason/],
  ['no issue number', 'until 2040-02-29: test', /ignore reason/],
  ['no until date', '#1: test', /ignore reason/],
  ['an impossible date', '#1 until 2040-02-30: test', /invalid until date/],
  ['a past date', '#1 until 2040-02-28: test', /expiry date is past/],
]) {
  test(`the advisory policy rejects ${name}`, () => assert.throws(() => checkIgnore(ignore(reason), TODAY), diagnostic));
}
test('the advisory policy accepts an ignore through its last UTC day', () => {
  checkIgnore(ignore('#1 until 2040-02-29: test'), TODAY);
});

test('the closed schema requires the full dependency graph and the pinned policy values', () => {
  const baseline = read('deny.toml');
  for (const key of ['targets', 'exclude', 'exclude-unpublished', 'new-key']) {
    assert.throws(() => checkPolicy(baseline.replace('[graph]', `[graph]\n${key} = []`), TODAY), /graph keys/);
  }
  for (const [oldText, replacement, diagnostic] of [
    ['"P7D"', '"P8D"', /advisories.maximum-db-staleness/],
    ['maximum-db-staleness = "P7D"', '', /advisories keys/],
    ['unsound = "all"', 'unsound = "workspace"', /advisories.unsound/],
    ['yanked = "deny"', 'yanked = "warn"', /advisories.yanked/],
    ['unknown-registry = "deny"', 'unknown-registry = "warn"', /sources.unknown-registry/],
    ['unknown-git = "deny"', 'unknown-git = "warn"', /sources.unknown-git/],
    [`db-urls = ["${RUSTSEC}"]`, 'db-urls = []', /advisories.db-urls/],
    [RUSTSEC, 'https://example.invalid/db', /advisories.db-urls/],
    [CRATES_IO, 'https://example.invalid/index', /sources.allow-registry/],
    ['allow-git = []', 'allow-git = ["example"]', /sources.allow-git/],
    ['clarify = []', 'clarify = ["example"]', /licenses.clarify/],
  ]) {
    assert.throws(() => checkPolicy(baseline.replace(oldText, replacement), TODAY), diagnostic);
  }
});

test('the license policy requires a reason immediately above an exception', () => {
  assert.throws(() => checkShape('licenses', 'exceptions', [{ value: { crate: 'notify', allow: ['CC0-1.0'] } }]), /reason comment/);
});

test('the global license list excludes extra licenses', () => {
  const allow = [...LICENSES, 'Zlib'].map(value => ({ value }));
  assert.throws(() => checkShape('licenses', 'allow', allow), /global license list/);
});

const gateFixture = [
  'set -euo pipefail', 'test -f deny.toml', 'cargo --version > "$RUNNER_TEMP/cargo-version.log"',
  logAssignment('advisories'), 'rc=0', `${command('advisories')} > "$log" 2>&1 || rc=$?`, SANITIZE_LOG, 'exit "$rc"',
].join('\n');
for (const flag of ['-A', '--exclude', '--exclude-dev', '--exclude-unpublished', '-t', '--metadata-path', '--disable-fetch', '--offline', '--frozen']) {
  test(`the gate rejects ${flag}`, () => {
    assert.throws(() => checkGate(gateFixture.replace(' --locked', ` --locked ${flag}`), 'advisories'), /cannot narrow/);
  });
}

test('the gate must propagate its status, use the lockfile, and have no pipe', () => {
  checkGate(gateFixture, 'advisories');
  assert.throws(() => checkGate(gateFixture.replace(' --locked', ''), 'advisories'), /locked check command/);
  assert.throws(() => checkGate(gateFixture.replace(' > "$log" 2>&1', ' | tee "$log"'), 'advisories'), /locked check command/);
  assert.throws(() => checkGate(gateFixture.replace('exit "$rc"', 'exit 0'), 'advisories'), /propagate the gate exit code/);
});

test('a static job name cannot stand in for a required context', () => {
  const shadow = `jobs:\n  shadow:\n    name: '${REQUIRED_CONTEXT} (extra)'\n    strategy:\n      matrix: { os: [linux] }`;
  assert.throws(() => checkNames(shadow, [REQUIRED_CONTEXT], true), /shadows required context/);
  const exact = shadow.replace(' (extra)', '');
  checkNames(exact, [REQUIRED_CONTEXT], true);
  assert.throws(() => checkNames(exact, [REQUIRED_CONTEXT], false), /needs one CI job/);
  assert.throws(() => checkNames(`${exact}\n  second:\n    name: ${REQUIRED_CONTEXT}`, [REQUIRED_CONTEXT], true), /needs one CI job/);
});

test('the installer must use the same action SHA and the pinned tool with no fallback', () => {
  const sha = 'a'.repeat(40);
  const install = [
    'name: Install cargo-deny', `        uses: taiki-e/install-action@${sha}  # v2`,
    '        env:', '          GITHUB_TOKEN: ${{ github.token }}', '        with:',
    `          tool: cargo-deny@${VERSION}`, '          fallback: none',
  ].join('\n');
  checkInstall(install, sha);
  assert.throws(() => checkInstall(install, 'b'.repeat(40)), /install-action SHA/);
  assert.throws(() => checkInstall(install.replace(VERSION, '9.9.9'), sha), /install version/);
  assert.throws(() => checkInstall(install.replace('fallback: none', 'fallback: cargo-binstall'), sha), /fallback/);
});

test('a dependency job cannot add job guards or job permissions', () => {
  const job = `    name: ${REQUIRED_CONTEXT}\n    runs-on: ubuntu-latest\n    timeout-minutes: 10\n    steps:`;
  checkJobFields(job, REQUIRED_CONTEXT);
  for (const field of ['if', 'needs', 'paths', 'permissions']) {
    assert.throws(() => checkJobFields(`${job}\n    ${field}: ignored`, REQUIRED_CONTEXT), /dependency job keys/);
    assert.throws(() => checkJobFields(`${job}\n    '${field}': ignored`, REQUIRED_CONTEXT), /bare YAML mapping key/);
  }
});

const controlStep = (workflow, job) => steps(jobs(workflow).get(job).body).at(-1);

for (const [name, from, to, diagnostic] of [
  ['a masked policy call', "expect_failure licenses 4 'error[rejected]'", "expect_failure licenses 4 'error[rejected]' || true", /policy control calls/],
  ['a successful return on a policy miss', 'return 1', 'return 0', /policy control must fail on a miss/],
  ['an early successful exit', 'set -euo pipefail', 'set -euo pipefail\n          exit 0', /policy control must not exit early/],
]) {
  test(`the policy control rejects ${name}`, () => {
    const policy = controlStep(read(CI_WORKFLOW), 'cargo-deny');
    const advisory = controlStep(read(ADVISORIES_WORKFLOW), 'advisories');
    assert.throws(() => checkControls(policy.replace(from, to), advisory), diagnostic);
  });
}

for (const [name, from, to, diagnostic] of [
  ['a successful exit on a miss', 'exit 1', 'exit 0', /advisory control must fail on a miss/],
  ['an early successful exit', 'set -euo pipefail', 'set -euo pipefail\n          exit 0', /advisory control must not exit early/],
]) {
  test(`the advisory control rejects ${name}`, () => {
    const policy = controlStep(read(CI_WORKFLOW), 'cargo-deny');
    const advisory = controlStep(read(ADVISORIES_WORKFLOW), 'advisories');
    assert.throws(() => checkControls(policy, advisory.replace(from, to)), diagnostic);
  });
}

for (const invocation of [
  'cargo deny --locked check',
  'cargo deny --locked --config deny.toml check bans advisories',
  'cargo-deny check advisories',
]) {
  test(`CI rejects ${invocation} in another job`, () => {
    const ci = read(CI_WORKFLOW);
    const addedJob = `\n  extra-check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: ${invocation}\n`;
    assert.throws(() => checkTriggers(ci + addedJob, read(ADVISORIES_WORKFLOW), controlStep(ci, 'cargo-deny')), /CI must not check advisories/);
  });
}

test('CI rejects a pull request path filter', () => {
  const ci = read(CI_WORKFLOW);
  const filtered = ci.replace('  pull_request:\n', '  pull_request:\n    paths: [Cargo.lock]\n');
  assert.throws(() => checkTriggers(filtered, read(ADVISORIES_WORKFLOW), controlStep(ci, 'cargo-deny')), /no CI pull request filter/);
});

function checkTrackedCargoConfig(tracked) {
  const configs = tracked.split('\0').filter(path => path === '.cargo/config.toml' || path === '.cargo/config');
  assert.deepEqual(configs, [], 'workspace Cargo config must not be tracked');
}

for (const file of ['.cargo/config.toml', '.cargo/config']) {
  test(`the workspace must not track ${file}`, () => {
    assert.throws(() => checkTrackedCargoConfig(`.cargo/README.md\0${file}\0`), /workspace Cargo config must not be tracked/);
  });
}

test('the repository satisfies the dependency policy', async t => {
  const ci = read(CI_WORKFLOW);
  const advisories = read(ADVISORIES_WORKFLOW);
  const config = read('deny.toml');
  const release = read('.github/workflows/release.yml');

  await t.test('the policy has only approved keys and values', () => {
    assert.equal(/^# cargo-deny (\S+) checks /m.exec(config)?.[1], VERSION, 'deny.toml tool pin');
    checkPolicy(config, new Date().toISOString().slice(0, 10));
  });

  await t.test('both workflows enforce their checks and match the verifier', () => {
    const shas = [...release.matchAll(/uses: taiki-e\/install-action@([a-f0-9]{40})\b/g)].map(match => match[1]);
    assert.equal(new Set(shas).size, 1, 'release workflow install-action SHA');
    assert.ok(EXPECTED_CONTEXTS.includes(REQUIRED_CONTEXT), 'verifier requires the CI dependency check');
    const ciControl = checkJob(jobs(ci).get('cargo-deny')?.body ?? '', REQUIRED_CONTEXT, 'licenses bans sources', shas[0]);
    checkTriggers(ci, advisories, ciControl);
    const advisoryJobs = jobs(advisories);
    exactSet(advisoryJobs.keys(), ['advisories'], 'advisories jobs');
    const advisoryControl = checkJob(advisoryJobs.get('advisories').body, ADVISORIES_CONTEXT, 'advisories', shas[0]);
    checkControls(ciControl, advisoryControl);
    const workflows = readdirSync(new URL('../../.github/workflows/', import.meta.url)).filter(file => /\.ya?ml$/.test(file));
    assert.ok(workflows.includes('ci.yml') && workflows.includes('deny-advisories.yml'));
    for (const file of workflows) checkNames(read(`.github/workflows/${file}`), EXPECTED_CONTEXTS, file === 'ci.yml');
  });

  await t.test('the workspace does not track Cargo config', () => {
    const tracked = execFileSync('git', ['ls-files', '-z', '--', '.cargo'], {
      cwd: new URL('../../', import.meta.url), encoding: 'utf8',
    });
    checkTrackedCargoConfig(tracked);
  });

  await t.test('the release guide uses the pinned pre-flight command and version', () => {
    const preflight = /^## Pre-flight[^\n]*\n([\s\S]*?)(?=^## |$(?![\s\S]))/m.exec(read('RELEASING.md'))?.[1];
    assert.ok(preflight?.split('\n').includes(PREFLIGHT), 'missing release guide pre-flight command');
    const versions = [...preflight.matchAll(/(?:cargo-deny[-@ ]|\bversion=["']?)(\d+\.\d+\.\d+)\b/gi)];
    exactSet(new Set(versions.map(match => match[1])), [VERSION], 'release guide pre-flight tool version');
  });

  await t.test('the local instructions use the exact pre-flight command and pin', () => {
    assert.ok(read('CLAUDE.md').split('\n').includes(
      `${PREFLIGHT}  # CI pin ${VERSION}; sha256-verified install: RELEASING.md pre-flight`,
    ), 'missing local pre-flight command and tool pin');
  });
});
