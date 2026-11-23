import { isUtf8 } from 'node:buffer';
import { readSync } from 'node:fs';
import { isHazardous } from './verify-no-control-bytes.mjs';

const MAX_BYTES = 2 * 1024 * 1024;
const SECTION = '\n=== LICENSE ===\n';
const SECTION_HEADER = /^([^\n]+)\nCrates: ([A-Za-z0-9_-]+(?:, [A-Za-z0-9_-]+)*)\n\n/;
const PATH = /\bfile:\/\/|(?:^|[\s"'(<`=:[{])(?:[A-Za-z]:[\\/]|\\\\[^\s\\]+\\|~[\\/]|\/[^\s/]|(?:\.cargo|registry)[\\/])/i;

function readInput() {
  const buffer = Buffer.alloc(MAX_BYTES + 1);
  for (let offset = 0; offset < buffer.length;) {
    const count = readSync(0, buffer, offset, buffer.length - offset, null);
    if (count === 0) return buffer.subarray(0, offset);
    offset += count;
  }
  return null;
}

function normalize(input) {
  if (!input) return { error: 'input exceeds 2 MiB' };
  if (!isUtf8(input)) return { error: 'input is not UTF-8' };
  const text = input.toString('utf8').replaceAll('\r\n', '\n');
  for (const char of text) {
    if (isHazardous(char.codePointAt(0), null)) return { error: 'hazardous character' };
  }
  if (PATH.test(text)) return { error: 'path in output' };

  const [header, ...sections] = text.split('\n').map(line => line.trimEnd()).join('\n').split(SECTION);
  if (!header.trim() || sections.length === 0) return { error: 'missing license sections' };
  const sorted = [];
  for (const section of sections) {
    const match = SECTION_HEADER.exec(section);
    if (!match) return { error: 'invalid license section' };
    const license = section.slice(match[0].length);
    if (!license.trim()) return { error: 'empty license text' };
    const [, name, crates] = match;
    // Sort the crate list. Keep each copy of the license text (#136).
    sorted.push(`${name}\nCrates: ${crates.split(', ').sort().join(', ')}\n\n${license}`);
  }
  return { output: `${[header, ...sorted.sort()].join(SECTION).trimEnd()}\n` };
}

function generate() {
  let input;
  try {
    input = readInput();
  } catch {
    return { error: 'cannot read input' };
  }
  return normalize(input);
}

const result = generate();
if ('error' in result) {
  console.error(`NOTICE: ${result.error}`);
  process.exitCode = 1;
} else {
  process.stdout.write(result.output);
}
