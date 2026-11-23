import assert from 'node:assert/strict';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const ROOT = fileURLToPath(new URL('../../', import.meta.url));
const PIN = /^([^\s@]+)@([0-9a-f]{40}) {2}# (\S+)$/;
const FIRST_PARTY_PIN = /^[^\s@]+@v\d+(?:\.\d+){0,2}(?:[ \t]+#.*)?$/;
const PYPI = 'pypa/gh-action-pypi-publish';
const RUST = 'dtolnay/rust-toolchain';
const EMPTY_EXPR = /\$\{\{\s*\}\}/;

function yamlFiles(dir) {
  return readdirSync(join(ROOT, dir), { withFileTypes: true })
    .filter((entry) => entry.isFile() && /\.ya?ml$/.test(entry.name))
    .map((entry) => join(dir, entry.name));
}

function actionSources() {
  const actions = '.github/actions';
  const files = [
    ...yamlFiles('.github/workflows'),
    ...readdirSync(join(ROOT, actions), { withFileTypes: true })
      .filter((entry) => entry.isDirectory())
      .flatMap((entry) => yamlFiles(join(actions, entry.name)))
      .filter((file) => /[/\\]action\.ya?ml$/.test(file)),
  ];
  return files.map((file) => [file, readFileSync(join(ROOT, file), 'utf8')]);
}

function indent(line) {
  return line.length - line.trimStart().length;
}

// Inspect the step's own YAML properties. The step can put `with:` before `uses:`.
function stepToolchain(lines, index, propertyIndent) {
  let start = index;
  if (indent(lines[index]) === propertyIndent) {
    for (let i = index - 1; i >= 0; i -= 1) {
      if (lines[i] && indent(lines[i]) < propertyIndent) break;
      start = i;
    }
  }
  for (let i = start; i < lines.length; i += 1) {
    const line = lines[i];
    if (i === index || !line) continue;
    const depth = indent(line);
    if (depth < propertyIndent) break;
    if (depth !== propertyIndent) continue;
    const withValue = /^with:[ \t]*(.*)$/.exec(line.trimStart());
    if (!withValue) continue;
    if (withValue[1].startsWith('{')) {
      return /[{,][ \t]*toolchain:[ \t]*([^\s,}]+)[ \t]*(?=[,}])/.exec(withValue[1])?.[1];
    }
    let inputIndent;
    for (let j = i + 1; j < lines.length; j += 1) {
      const input = lines[j];
      if (!input) continue;
      const depth = indent(input);
      if (depth <= propertyIndent) break;
      inputIndent ??= depth;
      if (depth !== inputIndent) continue;
      const toolchain = /^toolchain:[ \t]*(\S+)(?:[ \t]+#.*)?$/.exec(input.trimStart());
      if (toolchain) return toolchain[1];
    }
    return undefined;
  }
  return undefined;
}

export function checkActionPins(sources) {
  const problems = [];
  const shaByChannel = new Map();
  const channelBySha = new Map();
  let thirdPartyCount = 0;

  for (const [file, text] of sources) {
    // Check raw text. GitHub also scans shell comments in run blocks.
    const empty = EMPTY_EXPR.exec(text);
    if (empty) {
      const line = text.slice(0, empty.index).split('\n').length;
      problems.push(`${file}:${line}: empty Actions expression`);
    }
    // Strip whole-line comments but keep line numbers and pin comments.
    const lines = text.split('\n').map((line) => /^\s*#/.test(line) ? '' : line.trimEnd());
    for (const [i, line] of lines.entries()) {
      const use = /^( *)(- +)?uses:[ \t]*(.*)$/.exec(line);
      if (!use) continue;
      const value = use[3];
      if (value.startsWith('./')) continue;
      const at = `${file}:${i + 1}`;
      const action = value.split('@', 1)[0].toLowerCase();
      if (action.startsWith('actions/')) {
        if (!FIRST_PARTY_PIN.test(value)) {
          problems.push(`${at}: expected a first-party action version tag (vN, vN.N, or vN.N.N)`);
        }
        continue;
      }
      thirdPartyCount += 1;
      if (action === PYPI) {
        // This action needs the release tag to select the image.
        if (!/^v\d+\.\d+\.\d+$/.test(value.slice(PYPI.length + 1))) {
          problems.push(`${at}: expected PyPI vX.Y.Z tag`);
        }
        continue;
      }
      const pin = PIN.exec(value);
      if (!pin) {
        problems.push(`${at}: expected action@<40-digit SHA>  # <ref>`);
        continue;
      }
      if (action !== RUST) continue;

      const [, , sha, channel] = pin;
      if (shaByChannel.has(channel) && shaByChannel.get(channel) !== sha) {
        problems.push(`${at}: toolchain ${channel} has more than one SHA`);
      }
      if (channelBySha.has(sha) && channelBySha.get(sha) !== channel) {
        problems.push(`${at}: toolchain ${channel} shares a SHA with ${channelBySha.get(sha)}`);
      }
      shaByChannel.set(channel, sha);
      channelBySha.set(sha, channel);

      // Old version refs ignore this input. It keeps the version if the pin moves to a stable-branch SHA.
      const raw = stepToolchain(lines, i, use[1].length + (use[2]?.length ?? 0));
      const quoted = /^(['"])(.*)\1$/.exec(raw ?? '');
      if ((quoted?.[2] ?? raw) !== channel) {
        problems.push(`${at}: toolchain input must equal ${channel}`);
      } else if (/^\d+\.\d+$/.test(channel) && !quoted) {
        problems.push(`${at}: quote toolchain ${channel}`);
      }
    }
  }
  return { thirdPartyCount, problems, toolchainChannels: [...shaByChannel.keys()] };
}

const SHA = 'a'.repeat(40);
const OTHER_SHA = 'b'.repeat(40);
const toolchain = (sha, channel, raw = channel) =>
  `      - uses: ${RUST}@${sha}  # ${channel}\n        with: { toolchain: ${raw}, targets: wasm32-unknown-unknown }`;
const inspect = (text) => checkActionPins([['fixture.yml', text]]);

for (const [name, text, expected] of [
  ['rejects a branch ref for a first-party action', '  - uses: actions/checkout@main', 'expected a first-party action version tag (vN, vN.N, or vN.N.N)'],
  ['rejects a mutable third-party ref', '  - uses: Swatinem/rust-cache@v2', 'expected action@<40-digit SHA>  # <ref>'],
  ['requires two spaces before the ref comment', `  - uses: Swatinem/rust-cache@${SHA} # v2`, 'expected action@<40-digit SHA>  # <ref>'],
  ['rejects a SHA for the PyPI action', `  - uses: ${PYPI}@${SHA}  # v1.14.3`, 'expected PyPI vX.Y.Z tag'],
  ['rejects one SHA for two channels', `${toolchain(SHA, 'stable')}\n${toolchain(SHA, '1.96.0')}`, 'toolchain 1.96.0 shares a SHA with stable'],
  ['rejects two SHAs for the same channel', `${toolchain(SHA, 'stable')}\n${toolchain(OTHER_SHA, 'stable')}`, 'toolchain stable has more than one SHA'],
  ['rejects an input that differs from the channel', toolchain(SHA, 'stable', 'nightly'), 'toolchain input must equal stable'],
  ['rejects an unquoted numeric input', toolchain(SHA, '1.88'), 'quote toolchain 1.88'],
  ['does not borrow an input from the next step', `      - uses: ${RUST}@${SHA}  # stable\n      - name: Next\n        with:\n          toolchain: stable`, 'toolchain input must equal stable'],
  ['rejects an empty expression in a shell comment', '    run: |\n      # ${{ }}', 'empty Actions expression'],
]) {
  test(name, () => {
    assert.deepEqual(inspect(text).problems.map((problem) => problem.split(': ').at(-1)), [expected]);
  });
}

for (const tag of ['v4', 'v4.2', 'v4.2.2']) {
  test(`accepts the first-party action tag ${tag}`, () => {
    assert.deepEqual(inspect(`  - uses: actions/checkout@${tag}`).problems, []);
  });
}

test('accepts valid pins and ignores commented-out action uses', () => {
  const text = [
    `    # - uses: Swatinem/rust-cache@v2`,
    `    - uses: actions/checkout@v7`,
    `    - uses: ./.github/actions/setup-wasm`,
    `    - uses: ${PYPI}@v1.14.3`,
    `    - name: Minimum Rust version`,
    `      with:`,
    `        toolchain: "1.88"`,
    `      uses: ${RUST}@${OTHER_SHA}  # 1.88`,
    toolchain(SHA, 'stable'),
  ].join('\n');
  assert.deepEqual(inspect(text), { thirdPartyCount: 3, problems: [], toolchainChannels: ['1.88', 'stable'] });
});

test('workflows and composite actions satisfy the pin policy', () => {
  const { thirdPartyCount, problems, toolchainChannels } = checkActionPins(actionSources());
  assert.ok(thirdPartyCount > 0, 'expected at least one third-party action use');
  assert.deepEqual(problems, [], problems.join('\n'));
  const missingChannels = ['stable', '1.88', '1.96.0'].filter((channel) => !toolchainChannels.includes(channel));
  assert.deepEqual(missingChannels, [], `missing Rust toolchain channels: ${missingChannels.join(', ')}`);
});
