// Consume an MDS Source Map v3 document via the @mdscript/mds API.
//
// Compiles annotated-prompt.mds with `sourceMap: true`, then decodes the
// Base64-VLQ `mappings` string to trace a generated output line back to the
// original source file and line it came from — the entry template or the
// imported `_style.mds` partial.
//
// Run from the repository root:
//   node examples/source-maps/consume-map.mjs
//
// End users who installed the package would instead write:
//   import * as mds from '@mdscript/mds';

import * as mds from '../../packages/mds/dist/node.js';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const entry = resolve(__dirname, 'annotated-prompt.mds');

// ── Minimal Base64-VLQ decoder (Source Map v3) ──────────────────────────────
// A production consumer would use the `source-map` npm package; this compact
// decoder keeps the example dependency-free. Returns, per generated line, an
// array of segments with ABSOLUTE { genCol, srcIndex, srcLine, srcCol }.
const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
const CHAR2INT = new Map([...B64].map((c, i) => [c, i]));

function decodeMappings(mappings) {
  const lines = [];
  let srcIndex = 0, srcLine = 0, srcCol = 0;
  for (const group of mappings.split(';')) {
    const segs = [];
    let genCol = 0;
    for (const segStr of group.split(',')) {
      if (!segStr.length) continue;
      const fields = [];
      let i = 0, guard = 0;
      // Each segment has 1, 4, or 5 VLQ fields. Bound the loop (reliability).
      while (i < segStr.length && fields.length < 5 && guard++ < 8) {
        let result = 0, shift = 0, cont = 1, g2 = 0;
        while (cont && g2++ < 8) {
          const digit = CHAR2INT.get(segStr[i++]);
          cont = digit & 32;
          result += (digit & 31) << shift;
          shift += 5;
        }
        fields.push(result & 1 ? -(result >>> 1) : result >>> 1);
      }
      genCol += fields[0];
      const seg = { genCol };
      if (fields.length >= 4) {
        srcIndex += fields[1];
        srcLine += fields[2];
        srcCol += fields[3];
        Object.assign(seg, { srcIndex, srcLine, srcCol });
      }
      segs.push(seg);
    }
    lines.push(segs);
  }
  return lines;
}

// ── Security assertion: sources[] must never contain absolute paths ────────────
// Enforces the invariant documented in crates/mds-core/src/source_path.rs
// (PF-005 / ADR-005): the relativization guard ensures no raw filesystem path
// leaks into sources[]. An absolute or drive-qualified entry means the guard
// was bypassed — report that as a bug in source_path.rs.
//
// The check itself is verifiable: run with MDS_BACKEND=wasm to confirm both
// backends honour the invariant on the same compilation.
function assertNoAbsoluteSources(sources) {
  for (const src of sources) {
    if (src.startsWith('/')) {
      throw new Error(
        `SECURITY: sources[] contains an absolute path: ${JSON.stringify(src)}\n` +
        'This violates the PF-005 invariant — file the bug in ' +
        'crates/mds-core/src/source_path.rs',
      );
    }
    // Windows drive-qualified path: C:\... or C:/...
    if (/^[A-Za-z]:/.test(src)) {
      throw new Error(
        `SECURITY: sources[] contains a drive-qualified path: ${JSON.stringify(src)}\n` +
        'This violates the PF-005 invariant — file the bug in ' +
        'crates/mds-core/src/source_path.rs',
      );
    }
  }
}

// ── Compile with a source map ───────────────────────────────────────────────
await mds.init();
console.log(`Backend: ${mds.getBackend()}\n`);

const result = await mds.compileFile(entry, { sourceMap: true });
if (result.kind !== 'markdown') {
  throw new Error(`expected markdown output, got ${result.kind}`);
}

const map = result.sourceMap;

// Assert the security invariant before printing anything.
assertNoAbsoluteSources(map.sources);
console.log(`Security check passed: sources[] has ${map.sources.length} entries, none absolute.\n`);

console.log('Source Map v3 document:');
console.log(`  version:  ${map.version}`);
console.log(`  sources:  ${JSON.stringify(map.sources)}`);
console.log(`  names:    ${JSON.stringify(map.names)}`);
console.log(`  file key present? ${'file' in map}  (bindings omit it; the CLI sets it)\n`);

// ── Resolve a chosen generated line back to its source ──────────────────────
const decoded = decodeMappings(map.mappings);
const outLines = result.output.split('\n');

function trace(genLine1Based) {
  const segs = decoded[genLine1Based - 1] ?? [];
  const text = outLines[genLine1Based - 1] ?? '';
  console.log(`generated L${genLine1Based}: ${JSON.stringify(text)}`);
  if (segs.length === 0) {
    console.log('  (no mapping — passthrough/frontmatter line)');
    return;
  }
  for (const s of segs) {
    if (s.srcIndex === undefined) continue;
    const src = map.sources[s.srcIndex];
    // Source Map v3 line/column fields are 0-based; add 1 for display.
    console.log(`  col ${s.genCol} → ${src}:${s.srcLine + 1}:${s.srcCol + 1}`);
  }
}

console.log('Tracing three output lines back to source:\n');
// A heading interpolated in the entry template (`# System prompt: {agent}`).
trace(10);
// A heading emitted by the imported `_style.mds` section() macro.
trace(14);
// A list item produced by a @for loop over the imported rule() macro.
trace(16);

console.log('\nEach imported-module line maps back to _style.mds, while the');
console.log('interpolated values map back to annotated-prompt.mds — exactly the');
console.log('provenance a debugger or prompt-inspection tool needs.');
