import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readdirSync, readFileSync } from 'node:fs';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const file = path => new URL(`../../${path}`, import.meta.url);
const read = path => readFileSync(file(path), 'utf8');
const notice = readFileSync(file('NOTICE'));
const pythonNotice = readFileSync(file('crates/mds-python/NOTICE'));
const template = read('about.hbs');
const ci = read('.github/workflows/ci.yml');
const release = read('.github/workflows/release.yml');
const header = 'Rust crate license notices (#136)\n\n'
  + 'This notice covers Rust crates resolved from Cargo.lock for the shipped targets.\n'
  + 'It does not cover the Rust standard library, toolchain parts, crates.io source packages, or npm dependencies.\n';
const marker = '\n=== LICENSE ===\n';
const section = 'MIT\nCrates: alpha, zeta\n\nExample license text\n';
const fixture = `${header}${marker}${section}`;

function normalize(input) {
  return spawnSync(process.execPath, [fileURLToPath(file('scripts/generate-notice.mjs'))], { input });
}

function noticeProblem(input) {
  const result = normalize(input);
  if (result.status === 1) return result.stderr.toString().trim();
  assert.equal(result.status, 0, result.error?.message);
  if (!input.equals(result.stdout)) return 'NOTICE is not in canonical form';
  if (!input.toString('utf8').startsWith(header + marker)) return 'Wrong NOTICE scope';
  return null;
}

test('NOTICE has the stated scope and canonical bytes (#136)', () => {
  assert.equal(noticeProblem(notice), null);
  assert.equal(noticeProblem(Buffer.from(fixture)), null);
});

for (const [name, text, expected] of [
  ['home path', `${fixture}~/crates/example\n`, 'NOTICE: path in output'],
  ['absolute path', `${fixture}/Users/example\n`, 'NOTICE: path in output'],
  ['bracketed path', `${fixture}[/opt/example]\n`, 'NOTICE: path in output'],
  ['file URL', `${fixture}file:///opt/example\n`, 'NOTICE: path in output'],
  ['registry path', `${fixture}registry/src/example\n`, 'NOTICE: path in output'],
  ['drive path', `${fixture}C:\\example\n`, 'NOTICE: path in output'],
  ['CR', fixture.replace('Example', '\rExample'), 'NOTICE: hazardous character'],
  ['CRLF', fixture.replaceAll('\n', '\r\n'), 'NOTICE is not in canonical form'],
  ['form feed', `${fixture}${String.fromCodePoint(0x0c)}`, 'NOTICE: hazardous character'],
  ['BOM', String.fromCodePoint(0xfeff) + fixture, 'NOTICE: hazardous character'],
  ['missing final LF', fixture.trimEnd(), 'NOTICE is not in canonical form'],
  ['extra final LF', `${fixture}\n`, 'NOTICE is not in canonical form'],
  ['unsorted crates', fixture.replace('alpha, zeta', 'zeta, alpha'), 'NOTICE is not in canonical form'],
  ['unsorted licenses', `${fixture}${marker}${section.replace(/^MIT/, 'Apache')}`, 'NOTICE is not in canonical form'],
  ['missing license text', fixture.replace('Example license text\n', ''), 'NOTICE: empty license text'],
  ['incorrect scope', fixture.replace('It does not cover', 'It covers'), 'Wrong NOTICE scope'],
]) {
  test(`NOTICE rejects ${name} (#136)`, () => {
    assert.equal(noticeProblem(Buffer.from(text)), expected);
  });
}

for (const [name, input, error] of [
  ['invalid UTF-8', Buffer.from([0xff]), 'input is not UTF-8'],
  ['oversize input', Buffer.alloc(2 * 1024 * 1024 + 1), 'input exceeds 2 MiB'],
  ['empty input', Buffer.alloc(0), 'missing license sections'],
  ['malformed section', Buffer.from(`${header}${marker}MIT\n`), 'invalid license section'],
]) {
  test(`The generator rejects ${name} without output (#136)`, () => {
    const result = normalize(input);
    assert.deepEqual([result.status, result.stderr.toString(), result.stdout.length], [1, `NOTICE: ${error}\n`, 0]);
  });
}

function templateProblem(text) {
  if (!text.startsWith(header)) return 'Wrong NOTICE scope';
  const fields = Array.from(text.matchAll(/\{\{\{?\s*([^{}]+?)\s*\}\}\}?/g), match => match[1]);
  const licenseFields = ['#each licenses', 'name', '#each used_by', 'crate.name', '#unless @last', '/unless', '/each', 'text', '/each'];
  return JSON.stringify(fields) === JSON.stringify(licenseFields) && text.includes('{{{text}}}')
    ? null : 'Unexpected NOTICE template fields';
}

test('The template emits names and verbatim license text only (#136)', () => {
  assert.equal(templateProblem(template), null);
  for (const field of ['version', 'source_path', 'path']) {
    assert.equal(templateProblem(template.replace('{{name}}', `{{name}} {{${field}}}`)), 'Unexpected NOTICE template fields');
  }
});

test('Python NOTICE matches root NOTICE byte for byte (#136)', () => {
  const altered = Buffer.from(pythonNotice);
  altered[0] ^= 1;
  assert.equal(notice.equals(altered), false);
  assert.equal(notice.equals(pythonNotice), true);
});

function configProblems(wasm, python) {
  const { files, scripts } = JSON.parse(wasm);
  const problems = [];
  if (!files.includes('NOTICE')) problems.push('WASM files list omits NOTICE');
  if (!scripts.build.includes("copyFileSync('../../NOTICE', 'NOTICE')")) problems.push('WASM build omits NOTICE');
  const licenseFiles = /^license-files\s*=\s*([^\n]+)$/m.exec(python)?.[1];
  if (!licenseFiles || !JSON.parse(licenseFiles).includes('NOTICE')) problems.push('Python license files omit NOTICE');
  return problems;
}

test('The WASM and Python package settings include NOTICE (#136)', () => {
  const wasm = read('packages/mds-wasm/package.json');
  const python = read('crates/mds-python/pyproject.toml');
  assert.deepEqual(configProblems(wasm, python), []);
  assert.deepEqual(configProblems(wasm.replace('"NOTICE",', '"NOTICE.bak",'), python), ['WASM files list omits NOTICE']);
  assert.deepEqual(configProblems(wasm.replace("copyFileSync('../../NOTICE', 'NOTICE')", 'noop()'), python), ['WASM build omits NOTICE']);
  assert.deepEqual(configProblems(wasm, python.replace('"NOTICE"]', '"NOTICE.bak"]')), ['Python license files omit NOTICE']);
});

const stripComments = text => text.replace(/^ *#.*$/gm, '');
const jobBlocks = text => new Map(Array.from(stripComments(text).matchAll(/^  ([\w-]+):\n([\s\S]*?)(?=^  [\w-]+:\n|(?![\s\S]))/gm),
  match => [match[1], match[2]]));

function namedStep(job, name) {
  const starts = Array.from(job.matchAll(/^      - (?:name|uses|run): .*/gm));
  const i = starts.findIndex(match => match[0] === `      - name: ${name}`);
  if (i < 0) return '';
  return job.slice(starts[i].index, starts[i + 1]?.index);
}

const generator = /\bgenerate-notice\.mjs\b|(?:\bcargo(?:-about)?|\$cargo_about")\s+about\s+generate\b/;

test('Only the release workflow can generate NOTICE (#136)', () => {
  const workflows = readdirSync(file('.github/workflows')).filter(name => /\.ya?ml$/.test(name));
  assert.ok(workflows.includes('release.yml'));
  assert.ok(workflows.includes('ci.yml'));
  for (const name of workflows.filter(name => name !== 'release.yml')) {
    assert.equal(generator.test(stripComments(read(`.github/workflows/${name}`))), false, name);
  }
  assert.equal(generator.test('run: cargo about generate --frozen'), true);
});

function freshnessProblems(workflow, ciWorkflow) {
  const problems = [];
  const [defaults, jobText = ''] = workflow.split(/^jobs:\s*$/m);
  const jobs = jobBlocks(jobText);
  const gate = jobs.get('version-gate') ?? '';
  const install = namedStep(gate, 'Install cargo-about (pinned)');
  const version = namedStep(gate, 'Assert cargo-about is the pinned version');
  const freshness = namedStep(gate, 'Assert Rust crate NOTICE files are current');
  if (!generator.test(freshness) || [...jobs].some(([id, job]) => id !== 'version-gate' && generator.test(job))
      || generator.test(gate.replace(freshness, ''))) problems.push('NOTICE generator must run only in the freshness step');
  if (generator.test(stripComments(ciWorkflow))) problems.push('CI must not regenerate NOTICE');
  if (/^    if:/m.test(gate) || [install, version, freshness].some(step => /^        if:/m.test(step))) {
    problems.push('The NOTICE steps must run on every release event');
  }
  const grants = /^    permissions:\n((?:      .*\n|\n)+)/m.exec(gate)?.[1].trim().split(/\n\s*/);
  if (!grants || !grants.every(line => /^[\w-]+: (?:read|none)(?:\s+#.*)?$/.test(line))
      || /\bsecrets(?:\.|\[)/.test(stripComments(defaults) + gate)) {
    problems.push('The NOTICE job must not hold publish grants');
  }
  const inputs = install.split('        with:\n')[1]?.trimEnd();
  if (!/^        uses: taiki-e\/install-action@[a-f0-9]{40} +#/m.test(install)
      || inputs !== '          tool: cargo-about@0.9.1\n          fallback: none'
      || (gate.includes('Swatinem/rust-cache@') && gate.indexOf(install) > gate.indexOf('Swatinem/rust-cache@'))) {
    problems.push('Install the pinned cargo-about before any Rust cache');
  }
  if (!version.includes('cargo-about 0.9.1') || !version.includes('cargo-about 9.9.9')) {
    problems.push('Check the binary version with a wrong-version control');
  }
  if (!freshness.includes('cargo fetch --locked') || !freshness.includes('about generate --frozen --workspace --all-features --fail')
      || !freshness.includes('for checked in NOTICE crates/mds-python/NOTICE')
      || !freshness.includes('cmp -s "$checked" "$notice_out" || rc=$?')) {
    problems.push('Regenerate from the locked graph and compare both copies');
  }
  const error = freshness.indexOf('Refresh NOTICE:');
  const diff = freshness.indexOf('diff -u');
  if (error < 0 || diff < error || !freshness.includes('diff_rc=$?') || !freshness.includes("sed -n '1,80p'")) {
    problems.push('Print the refresh command before the bounded diff and retain its status');
  }
  return problems;
}

test('Local and release generation use all configured targets (#136)', () => {
  const line = text => text.split('\n').find(row => row.trimStart().startsWith('"$cargo_about" about generate '))?.trim();
  const recipe = line(release);
  assert.ok(recipe, 'release generation command');
  assert.equal(line(read('RELEASING.md')), recipe);
  const config = /^targets = \[([^\]]+)\]/m.exec(read('about.toml'))[1];
  const targets = [...config.matchAll(/"([\w-]+)"/g)].map(match => match[1]).sort();
  const cliTargets = command => [...command.matchAll(/ --target ([\w-]+)/g)].map(match => match[1]).sort();
  assert.equal(targets.length, 8);
  assert.deepEqual(cliTargets(recipe), targets);
  assert.notDeepEqual(cliTargets(recipe.replace(` --target ${targets[0]}`, '')), targets);
});

test('Release freshness checks the locked NOTICE in a read-only job (#136)', () => {
  assert.deepEqual(freshnessProblems(release, ci), []);
  for (const [before, after, reason] of [
    ['  publish-npm:\n', '  publish-npm:\n    # control\n    steps:\n      - run: node scripts/generate-notice.mjs\n', 'NOTICE generator must run only in the freshness step'],
    ['    name: Version gate', '    if: false\n    name: Version gate', 'The NOTICE steps must run on every release event'],
    ['- name: Assert Rust crate NOTICE files are current\n', '- name: Assert Rust crate NOTICE files are current\n        if: false\n', 'The NOTICE steps must run on every release event'],
    ['      checks: read', '      id-token: write\n      checks: read', 'The NOTICE job must not hold publish grants'],
    ['      checks: read', '      checks: write', 'The NOTICE job must not hold publish grants'],
    ['env:\n  CARGO_TERM_COLOR:', 'env:\n  NPM_TOKEN: $' + '{{ secrets.NPM_TOKEN }}\n  CARGO_TERM_COLOR:', 'The NOTICE job must not hold publish grants'],
    ['      checks: read', '      checks: read\n    env:\n      NPM_TOKEN: $' + '{{ secrets.NPM_TOKEN }}', 'The NOTICE job must not hold publish grants'],
    ['tool: cargo-about@0.9.1', 'tool: cargo-about@0.9.2', 'Install the pinned cargo-about before any Rust cache'],
    ['cargo-about 9.9.9', 'cargo-about 0.9.1', 'Check the binary version with a wrong-version control'],
    ['about generate --frozen', 'about generate --locked', 'Regenerate from the locked graph and compare both copies'],
    ['for checked in NOTICE crates/mds-python/NOTICE', 'for checked in NOTICE', 'Regenerate from the locked graph and compare both copies'],
    ['cmp -s "$checked" "$notice_out" || rc=$?', 'rc=0', 'Regenerate from the locked graph and compare both copies'],
    ['diff_rc=$?', 'diff_rc=0', 'Print the refresh command before the bounded diff and retain its status'],
  ]) {
    const control = release.replace(before, after);
    assert.notEqual(control, release, `Missing fixture target: ${before}`);
    assert.deepEqual(freshnessProblems(control, ci), [reason]);
  }
  assert.deepEqual(freshnessProblems(release, `${ci}\n          node scripts/generate-notice.mjs\n`), ['CI must not regenerate NOTICE']);
});

function listingProblems(step) {
  const lines = stripComments(step);
  const problems = [];
  if (!/\b(?:unzip -Z1|tar -tzf|npm pack --dry-run --json)[^\n]+> /m.test(lines)
      || !/\bgrep -F[xq]* -- [^\n]+/m.test(lines)) problems.push('Check a saved archive list with grep -F');
  if (/\|\s*(?:\n\s*)?grep\b/.test(lines)) problems.push('Do not pipe a list into grep');
  return problems;
}

function platformOrderIsValid(workflow) {
  const jobs = jobBlocks(workflow.split(/^jobs:\s*$/m)[1]);
  const stage = jobs.get('stage-and-verify-napi');
  const publish = jobs.get('publish-npm');
  const inOrder = (job, ...names) => {
    const steps = names.map(name => namedStep(job, name));
    return steps.every((step, i) => step && (i === 0 || job.indexOf(step) > job.indexOf(steps[i - 1])));
  };
  return stage.includes('needs: [build-napi]')
    && stage.includes("needs.build-napi.result == 'success'")
    && inOrder(stage, 'Add LICENSE and NOTICE to each platform package', 'Assert platform packages contain NOTICE',
      'A3 — name <-> loader verification gate', 'Upload staged napi tree')
    && inOrder(publish, 'Assert restored platform packages contain NOTICE', 'Re-run A3 gate against the restored tree',
      'Publish platform packages (napi prepublish)', 'Assert platform NOTICE after napi prepublish', 'Publish npm packages')
    && namedStep(stage, 'Assert platform packages contain NOTICE').includes('run: &assert-platform-notice |')
    && ['Assert restored platform packages contain NOTICE', 'Assert platform NOTICE after napi prepublish']
      .every(name => namedStep(publish, name).includes('run: *assert-platform-notice'));
}

test('The platform list checks run before A3 and after napi prepublish (#136)', () => {
  assert.equal(platformOrderIsValid(release), true);
  assert.equal(platformOrderIsValid(release.replace('needs: [build-napi]', 'needs: [build-napi, build-python]')), false);
  assert.equal(platformOrderIsValid(release.replace('name: Assert restored platform packages contain NOTICE', 'name: Removed')), false);
  assert.equal(platformOrderIsValid(release.replace('name: Assert platform NOTICE after napi prepublish', 'name: Removed')), false);
});

test('The wheel, sdist and platform checks use saved archive lists (#136)', () => {
  const jobs = jobBlocks(release.split(/^jobs:\s*$/m)[1]);
  const stage = jobs.get('stage-and-verify-napi');
  const python = jobs.get('build-python');
  const ciJobs = jobBlocks(ci.split(/^jobs:\s*$/m)[1]);
  const wheel = [...ciJobs.values()].map(job => namedStep(job, 'Assert wheel is cp311-abi3 and contains NOTICE')).find(Boolean);
  const sdist = namedStep(python, 'Assert sdist version and NOTICE');
  const pack = namedStep(stage, 'Assert platform packages contain NOTICE');
  for (const step of [wheel, sdist, pack]) {
    assert.ok(step, 'The archive check must exist');
    assert.deepEqual(listingProblems(step), []);
    assert.deepEqual(listingProblems(`${step}\n          list | grep -F -- NOTICE\n`), ['Do not pipe a list into grep']);
    assert.deepEqual(listingProblems(step.replaceAll('grep -F', 'grep -E')), ['Check a saved archive list with grep -F']);
  }
});
