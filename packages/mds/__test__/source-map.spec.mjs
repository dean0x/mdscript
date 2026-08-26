/**
 * Source Map v3 tests for the @mdscript/mds universal package.
 *
 * Coverage:
 *   U-SM1: sourceMap:true → result.sourceMap is a valid SMv3 object
 *   U-SM2: default (no option) → sourceMap absent from result
 *   U-SM3: sourcesContent:true → sourcesContent present, index-aligned
 *   U-SM4: messages-mode + sourceMap:true → sourceMap absent, warning present
 *   U-SM5: sourcesContent:true without sourceMap:true → mds::invalid_options
 *   U-SM6: unknown option key still rejected (sourceMap/sourcesContent are now allowed)
 *   U-SM7: compileFile sourceMap structural validity
 *   U-SM8: JSON determinism — sourceMap round-trips and has correct types
 *   VLQ-SELF: self-tests for the hand-rolled Base64-VLQ decoder (TEST-2)
 *   W-SM: WASM backend source-map coverage — emission, cross-field guard, parity (TEST-1)
 *
 * Hand-rolled Base64-VLQ decoder — no new devDeps.
 */
import { test, describe, before } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm, mkdir } from 'node:fs/promises';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
import { existsSync, statSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { compile, compileFile, checkFile, lintFile, isMdsError, init } from '../dist/node.js';
import { SIMPLE_MDS } from './helpers.mjs';
import { initWasmNode, createWasmBackend } from '../dist/backend/wasm.js';
import { buildModulesMap } from '../dist/util/module-scanner.js';

// ---------------------------------------------------------------------------
// CF-SM helpers — locate CLI binary and Python interpreter for 4-surface
// differential tests.  Both searches follow the same priority as conftest.py:
// explicit env var > freshest build artifact > system PATH.
// ---------------------------------------------------------------------------

/** Absolute path to the repo root (three levels above this test file). */
const REPO_ROOT = fileURLToPath(new URL('../../../', import.meta.url));

/**
 * Return the path to the `mds` CLI binary, or null if none can be found.
 * Prefers the most recently modified of target/{release,debug}/mds.
 */
function findMdsCli() {
  const envBin = process.env.MDS_CLI_BIN;
  if (envBin) {
    if (!existsSync(envBin)) throw new Error(`MDS_CLI_BIN=${envBin} does not exist`);
    return envBin;
  }
  const exe = process.platform === 'win32' ? 'mds.exe' : 'mds';
  const candidates = ['release', 'debug']
    .map((p) => join(REPO_ROOT, 'target', p, exe))
    .filter(existsSync);
  if (!candidates.length) return null;
  return candidates.reduce((a, b) => statSync(a).mtimeMs >= statSync(b).mtimeMs ? a : b);
}

/**
 * Return the path to a Python interpreter that can import `markdown_script`, or null.
 *
 * Resolution order:
 *   1. MDS_PYTHON_BIN env var (set by CI to the pip-managed interpreter).
 *   2. Repo-local venv at .venv/bin/python3 (set up by `maturin develop`).
 *   3. System `python3` on PATH (best-effort fallback for local dev).
 *
 * Returns null only when none of the above is found.  In CI (process.env.CI)
 * the caller must treat null as a hard failure — see PF-007.
 */
function findPythonForMarkdownScript() {
  const envBin = process.env.MDS_PYTHON_BIN;
  if (envBin) {
    if (!existsSync(envBin)) throw new Error(`MDS_PYTHON_BIN=${envBin} does not exist`);
    return envBin;
  }
  const venvPy = join(REPO_ROOT, '.venv', 'bin', 'python3');
  if (existsSync(venvPy)) return venvPy;
  const res = spawnSync('which', ['python3'], { encoding: 'utf-8' });
  if (res.status === 0 && res.stdout.trim()) return res.stdout.trim();
  return null;
}

// ---------------------------------------------------------------------------
// Hand-rolled Base64-VLQ decoder (no external dependency)
// ---------------------------------------------------------------------------

const B64_CHARS = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
const B64_IDX = new Map(B64_CHARS.split('').map((c, i) => [c, i]));

/**
 * Decode a single Base64-VLQ sequence from `chars` starting at index `pos`.
 * Returns [value, nextPos].
 *
 * VLQ encoding: each 6-bit group has continuation in bit 5; the first group
 * has the sign bit in bit 0. Rejects on: unknown char, missing continuation,
 * or any sequence longer than 7 groups (> 32-bit range).
 */
function decodeVlqOne(chars, pos) {
  let result = 0;
  let shift = 0;
  const MAX_GROUPS = 7;
  for (let i = 0; i < MAX_GROUPS; i++) {
    if (pos >= chars.length) throw new Error(`VLQ truncated at pos ${pos}`);
    const idx = B64_IDX.get(chars[pos]);
    if (idx === undefined) throw new Error(`Unknown Base64 char ${JSON.stringify(chars[pos])} at pos ${pos}`);
    pos++;
    const digit = idx & 0x1f;
    const continued = (idx & 0x20) !== 0;
    result |= digit << shift;
    shift += 5;
    if (!continued) {
      // Zig-zag decode: LSB is sign.
      return [result & 1 ? -(result >> 1) : result >> 1, pos];
    }
  }
  throw new Error('VLQ sequence exceeds max 7 groups');
}

/**
 * Decode an SMv3 mappings string into an array of segments.
 *
 * Each segment is an array of 1 or 4 values:
 *   [generatedCol] or [generatedCol, sourceIdx, origLine, origCol]
 *
 * Returns the total segment count. Throws on invalid VLQ.
 */
function decodeMappings(mappings) {
  const segments = [];
  const lines = mappings.split(';');
  for (const line of lines) {
    if (line === '') continue;
    const groups = line.split(',');
    for (const group of groups) {
      if (group === '') continue;
      let pos = 0;
      const chars = group.split('');
      const fields = [];
      while (pos < chars.length) {
        const [val, next] = decodeVlqOne(chars, pos);
        fields.push(val);
        pos = next;
      }
      if (fields.length !== 1 && fields.length !== 4 && fields.length !== 5) {
        throw new Error(`Unexpected segment field count: ${fields.length} in "${group}"`);
      }
      segments.push(fields);
    }
  }
  return segments;
}

// ---------------------------------------------------------------------------
// Structural assertion (O(1) top-level checks + segment decode)
// ---------------------------------------------------------------------------

const VLQ_RE = /^[A-Za-z0-9+/,;]*$/;

/**
 * Assert structural invariants of a Source Map v3 object.
 *
 * O(1) top-level field checks, then decodes mappings to validate VLQ.
 */
function assertSmStructure(sm, { hasSourcesContent = false } = {}) {
  assert.ok(sm !== null && typeof sm === 'object', 'sourceMap must be a non-null object');
  assert.equal(sm.version, 3, 'version must be 3');
  assert.ok(Array.isArray(sm.sources), 'sources must be an array');
  assert.ok(Array.isArray(sm.names), 'names must be an array');
  assert.equal(typeof sm.mappings, 'string', 'mappings must be a string');
  assert.ok(VLQ_RE.test(sm.mappings), `mappings contains invalid chars: ${sm.mappings}`);
  assert.ok(!('file' in sm), 'file key must be absent for binding results');

  if (hasSourcesContent) {
    assert.ok(Array.isArray(sm.sourcesContent), 'sourcesContent must be an array');
    assert.equal(sm.sourcesContent.length, sm.sources.length, 'sourcesContent must align with sources');
  } else {
    assert.ok(!('sourcesContent' in sm), 'sourcesContent must be absent');
  }

  // Decode the VLQ mappings — throws on malformed encoding.
  decodeMappings(sm.mappings);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe('source maps (U-SM)', () => {
  before(() => init());

  // ── U-SM1: sourceMap:true produces a valid SMv3 object ──────────────────

  test('U-SM1: compile() with sourceMap:true returns valid sourceMap', () => {
    const result = compile('Hello World!\n', { sourceMap: true });
    assert.ok('sourceMap' in result, 'sourceMap key must be present');
    assertSmStructure(result.sourceMap);
    // String-source compilation uses "input.mds" as the entry label (unified
    // with the WASM backend via the STRING_SOURCE_MAP_LABEL choke-point fix).
    assert.deepEqual(result.sourceMap.sources, ['input.mds'],
      `string-source sources[0] must be "input.mds" after map_source_label fix; got: ${JSON.stringify(result.sourceMap.sources)}`);
    assert.ok(result.sourceMap.mappings.length > 0, 'mappings must be non-empty for non-trivial content');
  });

  // ── U-SM2: default → sourceMap absent ────────────────────────────────────

  test('U-SM2: compile() with no options → sourceMap absent', () => {
    const result = compile('Hello World!\n');
    assert.ok(!('sourceMap' in result), 'sourceMap must be absent from result');
  });

  test('U-SM2: compile() with sourceMap:false → sourceMap absent', () => {
    const result = compile('Hello World!\n', { sourceMap: false });
    assert.ok(!('sourceMap' in result), 'sourceMap must be absent when sourceMap:false');
  });

  // ── U-SM3: sourcesContent:true ─────────────────────────────────────────

  test('U-SM3: compile() with sourcesContent:true → sourcesContent present', () => {
    const src = 'Hello World!\n';
    const result = compile(src, { sourceMap: true, sourcesContent: true });
    assert.ok('sourceMap' in result, 'sourceMap must be present');
    assertSmStructure(result.sourceMap, { hasSourcesContent: true });
    const sc = result.sourceMap.sourcesContent;
    assert.ok(sc.some((v) => typeof v === 'string' && v.includes('Hello World')), 'sourcesContent should contain source text');
  });

  // ── U-SM4: messages-mode degradation ───────────────────────────────────

  test('U-SM4: messages-mode + sourceMap:true → sourceMap absent, warning present', () => {
    const src = '@message system:\nYou are helpful.\n@end\n@message user:\nHi!\n@end\n';
    const result = compile(src, { sourceMap: true });
    assert.equal(result.kind, 'messages');
    assert.ok(!('sourceMap' in result), 'sourceMap must be absent for messages-mode result');
    // Warning text may use "source map" (spaces) or "source_map" (underscore)
    // depending on the binding surface — match either form.
    assert.ok(
      result.warnings.some((w) => /source.?map/i.test(w)),
      `expected a "source map" warning, got: ${JSON.stringify(result.warnings)}`,
    );
  });

  // ── U-SM5: cross-field validation ──────────────────────────────────────

  test('U-SM5: compile() with sourcesContent:true without sourceMap:true → mds::invalid_options', () => {
    assert.throws(
      () => compile('Hello!\n', { sourcesContent: true }),
      (err) => {
        assert.ok(isMdsError(err), `expected MdsError, got: ${err}`);
        assert.equal(err.code, 'mds::invalid_options');
        assert.ok(/sourcesContent/i.test(err.message), `expected "sourcesContent" in message: ${err.message}`);
        return true;
      },
    );
  });

  test('U-SM5: compileFile() with sourcesContent:true without sourceMap:true → mds::invalid_options', async () => {
    await assert.rejects(
      () => compileFile(SIMPLE_MDS, { sourcesContent: true }),
      (err) => {
        assert.ok(isMdsError(err));
        assert.equal(err.code, 'mds::invalid_options');
        return true;
      },
    );
  });

  // ── U-SM6: valid option combinations ──────────────────────────────────
  //
  // Unknown key rejection at the binding level is tested in the napi spec
  // (F-SM6). At the universal package level assertKnownKeys() rejects
  // unknown option keys, with TypeScript type checking as the primary guard.

  test('U-SM6: sourceMap:true, sourcesContent:false is accepted', () => {
    const result = compile('Hello!\n', { sourceMap: true, sourcesContent: false });
    assert.ok('sourceMap' in result);
    assert.ok(!('sourcesContent' in result.sourceMap), 'sourcesContent must be absent');
  });

  test('U-SM6: sourceMap:false produces no sourceMap key', () => {
    const result = compile('Hello!\n', { sourceMap: false });
    assert.ok(!('sourceMap' in result), 'sourceMap must be absent when sourceMap:false');
  });

  // ── U-SM7: compileFile structural validity ─────────────────────────────

  test('U-SM7: compileFile() with sourceMap:true → valid SMv3, no file key', async () => {
    const result = await compileFile(SIMPLE_MDS, { sourceMap: true });
    assert.ok('sourceMap' in result, 'sourceMap must be present');
    assertSmStructure(result.sourceMap);
    // No `file` key for binding results.
    assert.ok(!('file' in result.sourceMap), 'file key must be absent');
    // Absolute path appears in sources[].
    assert.equal(result.sourceMap.sources.length, 1);
    assert.ok(
      result.sourceMap.sources[0].endsWith('simple.mds'),
      `expected sources[0] to end with "simple.mds", got: ${result.sourceMap.sources[0]}`,
    );
    // mappings must be valid Base64-VLQ.
    assert.ok(VLQ_RE.test(result.sourceMap.mappings));
  });

  // ── U-SM8: JSON determinism (source-map contract) ──────────────────────
  //
  // These tests verify that the sourceMap object produced by the native backend
  // is a well-formed, JSON-serializable SMv3 object whose content is stable
  // across round-trips. The native backend uses the shared core serializer;
  // cross-backend WASM parity (byte-identical mappings from both backends for
  // the same input) is asserted by the W-SM describe block in this file.

  test('U-SM8: sourceMap JSON round-trips to identical object', () => {
    const src = 'Hello World!\n';
    const result = compile(src, { sourceMap: true });
    const sm = result.sourceMap;
    assert.ok(sm != null);
    // JSON serialization must round-trip to an equal object.
    const roundTripped = JSON.parse(JSON.stringify(sm));
    assert.deepEqual(roundTripped, sm, 'sourceMap must survive JSON round-trip');
  });

  test('U-SM8: sourceMap fields are the correct types', () => {
    const result = compile('Hello!\n', { sourceMap: true });
    const sm = result.sourceMap;
    assert.ok(sm != null);
    assert.equal(typeof sm.version, 'number');
    assert.ok(Array.isArray(sm.sources));
    assert.ok(Array.isArray(sm.names));
    assert.equal(typeof sm.mappings, 'string');
  });
});

// ---------------------------------------------------------------------------
// VLQ decoder self-tests (TEST-2)
//
// The hand-rolled decodeVlqOne / decodeMappings functions are the spec's
// validation gate; a silent bug here would mis-validate all source maps.
// These tests mirror the CLI's sm_vlq_decoder_known_values Rust test so the
// two independent decoders agree on known vectors.
// ---------------------------------------------------------------------------

describe('VLQ decoder self-test (VLQ-SELF)', () => {
  // ── VLQ-SELF-1: single-group known values ──────────────────────────────
  //
  // Mirror of CLI sm_vlq_decoder_known_values:
  //   A → 0   (B64=0,  zig-zag → 0)
  //   C → 1   (B64=2,  zig-zag → 1)
  //   D → -1  (B64=3,  zig-zag → -1)
  //   E → 2   (B64=4,  zig-zag → 2)

  test('VLQ-SELF-1: A → 0', () => {
    const [val, next] = decodeVlqOne(['A'], 0);
    assert.equal(val, 0, 'A must decode to 0');
    assert.equal(next, 1, 'position must advance by 1');
  });

  test('VLQ-SELF-1: C → 1', () => {
    const [val] = decodeVlqOne(['C'], 0);
    assert.equal(val, 1, 'C must decode to 1');
  });

  test('VLQ-SELF-1: D → -1', () => {
    const [val] = decodeVlqOne(['D'], 0);
    assert.equal(val, -1, 'D must decode to -1');
  });

  test('VLQ-SELF-1: E → 2', () => {
    const [val] = decodeVlqOne(['E'], 0);
    assert.equal(val, 2, 'E must decode to 2');
  });

  test('VLQ-SELF-1: G → 3', () => {
    const [val] = decodeVlqOne(['G'], 0);
    assert.equal(val, 3, 'G must decode to 3');
  });

  // ── VLQ-SELF-2: position advancement in a multi-char sequence ──────────

  test('VLQ-SELF-2: decodeVlqOne advances pos correctly in multi-char sequence', () => {
    // Sequence ['A', 'C']: decode two values in order.
    const chars = ['A', 'C'];
    const [v0, pos1] = decodeVlqOne(chars, 0);
    assert.equal(v0, 0, 'first char A → 0');
    assert.equal(pos1, 1, 'position must be 1 after decoding A');
    const [v1, pos2] = decodeVlqOne(chars, pos1);
    assert.equal(v1, 1, 'second char C → 1');
    assert.equal(pos2, 2, 'position must be 2 after decoding C');
  });

  // ── VLQ-SELF-3: full-segment decode via decodeMappings ─────────────────
  //
  // Known 4-field segments:
  //   "AAAA" → [0, 0, 0, 0]  (all-zero offsets)
  //   "CCCC" → [1, 1, 1, 1]  (all-one offsets)
  //   "AAGA" → [0, 0, 3, 0]  (golden first segment for sm_basic.mds with
  //                            body starting 3 frontmatter lines in)

  test('VLQ-SELF-3: decodeMappings("AAAA") → [[0,0,0,0]]', () => {
    const segs = decodeMappings('AAAA');
    assert.equal(segs.length, 1, 'one segment');
    assert.deepEqual(segs[0], [0, 0, 0, 0], 'AAAA must decode to [0,0,0,0]');
  });

  test('VLQ-SELF-3: decodeMappings("CCCC") → [[1,1,1,1]]', () => {
    const segs = decodeMappings('CCCC');
    assert.deepEqual(segs[0], [1, 1, 1, 1], 'CCCC must decode to [1,1,1,1]');
  });

  test('VLQ-SELF-3: decodeMappings("AAGA") → [[0,0,3,0]] (golden first segment)', () => {
    const segs = decodeMappings('AAGA');
    assert.deepEqual(segs[0], [0, 0, 3, 0], 'AAGA must decode to [0,0,3,0]');
  });

  // ── VLQ-SELF-4: error cases ─────────────────────────────────────────────

  test('VLQ-SELF-4: decodeVlqOne throws on empty input', () => {
    assert.throws(
      () => decodeVlqOne([], 0),
      /VLQ truncated/,
      'empty input must throw VLQ truncated',
    );
  });

  test('VLQ-SELF-4: decodeVlqOne throws on unknown Base64 char', () => {
    assert.throws(
      () => decodeVlqOne(['!'], 0),
      /Unknown Base64 char/,
      'non-Base64 char must throw',
    );
  });
});

// ---------------------------------------------------------------------------
// WASM backend source-map coverage (TEST-1)
//
// The WASM backend has a separate code path from the native (napi) backend.
// These tests verify:
//   W-SM1: WASM compile() emits a valid SMv3 sourceMap for simple input
//   W-SM2: cross-field guard — sourcesContent:true without sourceMap:true
//          throws mds::invalid_options on the WASM backend
//   W-SM3: WASM and native backends produce IDENTICAL sourceMaps for the
//          same input (AC-API-04 / ADR-002 / PF-007 cross-surface parity)
//
// After the STRING_SOURCE_MAP_LABEL choke-point fix both backends emit
// "input.mds" in sources[0], so the full sourceMap (including sources[])
// is now comparable.  W-SM3 is now a true differential test (PF-007).
// ---------------------------------------------------------------------------

describe('source maps — WASM backend (W-SM)', () => {
  let wasmMod;
  let wasmBackend;

  before(async () => {
    wasmMod = await initWasmNode();
    wasmBackend = createWasmBackend(wasmMod);
  });

  // ── W-SM1: WASM compile produces valid SMv3 ─────────────────────────────

  test('W-SM1: WASM compile() with sourceMap:true returns valid sourceMap', () => {
    const result = wasmBackend.compile('Hello World!\n', { sourceMap: true });
    assert.ok('sourceMap' in result, 'sourceMap key must be present');
    // WASM default filename is "input.mds".
    assertSmStructure(result.sourceMap);
    assert.deepEqual(result.sourceMap.sources, ['input.mds'],
      'WASM default source label must be "input.mds"');
    assert.ok(result.sourceMap.mappings.length > 0, 'mappings must be non-empty');
  });

  test('W-SM1: WASM compile() with sourceMap:true, sourcesContent:true → both present', () => {
    const src = 'Hello World!\n';
    const result = wasmBackend.compile(src, { sourceMap: true, sourcesContent: true });
    assert.ok('sourceMap' in result, 'sourceMap must be present');
    assertSmStructure(result.sourceMap, { hasSourcesContent: true });
    const sc = result.sourceMap.sourcesContent;
    assert.ok(sc.some((v) => typeof v === 'string' && v.includes('Hello World')),
      'sourcesContent must embed source text');
  });

  test('W-SM1: WASM compile() with sourceMap:false → sourceMap absent', () => {
    const result = wasmBackend.compile('Hello!\n', { sourceMap: false });
    assert.ok(!('sourceMap' in result), 'sourceMap must be absent when sourceMap:false');
  });

  // ── W-SM2: cross-field guard on WASM backend ────────────────────────────

  test('W-SM2: WASM compile() with sourcesContent:true without sourceMap → mds::invalid_options', () => {
    assert.throws(
      () => wasmBackend.compile('Hello!\n', { sourcesContent: true }),
      (err) => {
        assert.equal(err.code, 'mds::invalid_options',
          `expected mds::invalid_options, got code: ${err.code}, message: ${err.message}`);
        return true;
      },
      'sourcesContent without sourceMap must throw mds::invalid_options',
    );
  });

  // ── W-SM3: WASM vs native full sourceMap parity (PF-007 differential) ─
  //
  // Both backends delegate to the same mds-core serializer.  After the
  // STRING_SOURCE_MAP_LABEL choke-point fix (map_source_label in
  // MapBuilder::new / source_index) both backends emit "input.mds" in
  // sources[0], so the ENTIRE sourceMap object is now identical.
  // This is a true differential test (PF-007): compare backends to each
  // other rather than to per-surface constants.

  test('W-SM3: WASM and native backends produce identical full sourceMap for same input', () => {
    const src = 'Hello World!\n';
    const nativeResult = compile(src, { sourceMap: true });
    const wasmResult = wasmBackend.compile(src, { sourceMap: true });

    assert.ok(nativeResult.sourceMap != null, 'native must produce sourceMap');
    assert.ok(wasmResult.sourceMap != null, 'wasm must produce sourceMap');

    assertSmStructure(nativeResult.sourceMap);
    assertSmStructure(wasmResult.sourceMap);

    // Full object comparison — avoids PF-007: per-field assertions cannot catch
    // new fields being added or fields that differ unexpectedly.  The ENTIRE
    // sourceMap object must be byte-identical across backends (ADR-002: shared
    // core serializer; PF-007: differential test rather than per-surface golden).
    assert.deepEqual(
      nativeResult.sourceMap,
      wasmResult.sourceMap,
      `full sourceMap must be identical across backends (PF-007); ` +
        `native=${JSON.stringify(nativeResult.sourceMap)}, wasm=${JSON.stringify(wasmResult.sourceMap)}`,
    );
  });

  // ── W-SM3b: cross-backend parity without sourcesContent ─────────────────
  //
  // Same input, sourcesContent:true — verify that both backends embed
  // identical source content and the labeling is consistent (PF-007).

  test('W-SM3b: WASM and native produce identical sourceMap with sourcesContent:true', () => {
    const src = 'Hello World!\n';
    const nativeResult = compile(src, { sourceMap: true, sourcesContent: true });
    const wasmResult = wasmBackend.compile(src, { sourceMap: true, sourcesContent: true });

    assert.ok(nativeResult.sourceMap != null, 'native must produce sourceMap');
    assert.ok(wasmResult.sourceMap != null, 'wasm must produce sourceMap');

    assert.deepEqual(
      nativeResult.sourceMap.sources,
      wasmResult.sourceMap.sources,
      `sources[] must match across backends with sourcesContent; native=${JSON.stringify(nativeResult.sourceMap.sources)}`,
    );
    assert.deepEqual(
      nativeResult.sourceMap.sourcesContent,
      wasmResult.sourceMap.sourcesContent,
      'sourcesContent must be identical across backends',
    );
  });

  // ── V-SM1: virtual module map path — WASM compile with filename option ─────
  //
  // PF-007: per-surface goldens cannot catch cross-surface divergence.  This
  // differential test verifies that when the WASM backend is given an explicit
  // `filename` (as buildModulesMap produces for the compileFile path), the
  // resulting sources[] is byte-identical to what the native compile produces
  // for the same source.
  //
  // The modules:{} (empty) arg exercises the modules-map code path without
  // requiring any @import directives.  The key assertion is deepEqual on
  // sources[], not per-surface goldens (catches future divergence between
  // STRING_SOURCE_MAP_LABEL and the WASM filename passthrough).

  test('V-SM1: WASM compile() with explicit filename + empty modules produces same sources[] as native', () => {
    const src = 'Hello World!\n';

    // Native compile: sources[0] = STRING_SOURCE_MAP_LABEL = "input.mds".
    const nativeResult = compile(src, { sourceMap: true });

    // WASM compile with the same filename the modules-map path would produce
    // (the buildModulesMap default when the project root is the entry's directory).
    const wasmResult = wasmBackend.compile(src, {
      filename: 'input.mds',
      modules: {},
      sourceMap: true,
    });

    assert.ok(nativeResult.sourceMap != null, 'native must produce sourceMap');
    assert.ok(wasmResult.sourceMap != null, 'wasm must produce sourceMap');

    // deepEqual comparison — avoids PF-007: per-field assertions cannot detect
    // future divergence in how native vs WASM label the virtual entry source.
    assert.deepEqual(
      nativeResult.sourceMap.sources,
      wasmResult.sourceMap.sources,
      `sources[] must be identical across backends on the modules-map path (PF-007); ` +
        `native=${JSON.stringify(nativeResult.sourceMap.sources)}, ` +
        `wasm-modules=${JSON.stringify(wasmResult.sourceMap.sources)}`,
    );
  });
});

// ---------------------------------------------------------------------------
// CF-SM1: compileFile differential — native napi vs WASM-via-buildModulesMap
//
// Catches the live PF-007 instance: the universal @mdscript/mds compileFile
// returned different sources[] depending on which backend init() loaded.
// WASM produced root-relative keys (buildModulesMap) while native produced
// absolute paths before the Phase A relativize_source choke-point fix.
//
// The test manually replicates the WASM compileFile code path (buildModulesMap
// + wasmModule.compile) and compares its sources[] against the native compileFile.
// After the fix both must produce identical root-relative sources[].
//
// A temp dir with .mdsroot is used so buildModulesMap can locate the project
// root and produce the correct root-relative entry filename.
// ---------------------------------------------------------------------------

describe('source maps — compileFile differential (CF-SM)', () => {
  let wasmMod;

  before(async () => {
    // init() loads the native backend; wasmMod gives us the WASM path.
    await init();
    wasmMod = await initWasmNode();
  });

  test('CF-SM1: native compileFile and WASM-via-buildModulesMap produce identical sources[]', async () => {
    // Create a temp dir with .mdsroot so buildModulesMap identifies it as the
    // project root, making the entry filename root-relative.
    const dir = await mkdtemp(join(tmpdir(), 'cf-sm1-'));
    try {
      await writeFile(join(dir, '.mdsroot'), '');
      await writeFile(join(dir, 'hello.mds'), 'Hello World!\n');
      const filePath = join(dir, 'hello.mds');

      // Native compileFile: uses NativeFs which now emits root-relative paths
      // (source_path.rs relativize_source choke-point, ADR-005 Phase A fix).
      const nativeResult = await compileFile(filePath, { sourceMap: true });

      // WASM compileFile simulation: the exact code path wrapWithFileOps uses.
      // buildModulesMap returns entryFilename as a root-relative slash path
      // (e.g. "hello.mds") and populates modules with the file source.
      const { entryFilename, modules } = await buildModulesMap(
        filePath,
        (src) => wasmMod.scanImports(src),
      );
      const entrySource = modules[entryFilename];
      // Remove entry from modules — WASM inserts it separately under filename.
      delete modules[entryFilename];
      const wasmResult = createWasmBackend(wasmMod).compile(entrySource, {
        filename: entryFilename,
        modules,
        sourceMap: true,
      });

      assert.ok(nativeResult.sourceMap != null, 'native compileFile must produce sourceMap');
      assert.ok(wasmResult.sourceMap != null, 'wasm compileFile path must produce sourceMap');

      // Full deepEqual on sources[] — the PF-007 failure mode was that native
      // produced absolute path while WASM produced root-relative key; now both
      // must produce the same root-relative value ("hello.mds").
      assert.deepEqual(
        nativeResult.sourceMap.sources,
        wasmResult.sourceMap.sources,
        `compileFile sources[] must be identical across backends (PF-007); ` +
          `native=${JSON.stringify(nativeResult.sourceMap.sources)}, ` +
          `wasm=${JSON.stringify(wasmResult.sourceMap.sources)}`,
      );

      // Extra guard: sources[0] must be root-relative (just the filename, no
      // absolute path prefix) — ensures the choke-point fix is actually active.
      const src0 = nativeResult.sourceMap.sources[0];
      assert.ok(
        !src0.includes('/') || src0.startsWith('./') || src0 === src0.replace(/^\//, ''),
        `sources[0] must not be an absolute path; got: ${src0}`,
      );
      assert.ok(
        src0.endsWith('hello.mds'),
        `sources[0] must end with "hello.mds"; got: ${src0}`,
      );
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  });

  // -------------------------------------------------------------------------
  // CF-SM2: all four surfaces produce identical sources[] for a nested fixture
  //
  // Closes the remaining PF-007 gap: CF-SM1 only compared napi ↔ WASM; the
  // CLI and Python surfaces each had per-surface goldens that could diverge
  // without detection.  This test extends the differential to all four surfaces
  // using a fixture that exercises cross-directory @import (not just a single
  // root-level file), so path relativization from a non-root source is tested.
  //
  // Fixture layout:
  //   <dir>/.mdsroot          ← project root marker
  //   <dir>/src/entry.mds     ← entry: @import "../partials/greeting.mds" as g
  //   <dir>/partials/greeting.mds ← @define greet(): Hello! @end @export greet
  //
  // CLI is invoked with -o <dir>/out.md so the sidecar map lands at the project
  // root level, making sources[] root-relative — identical to the binding output
  // (QA-verified byte-identical when map anchor equals project root).
  //
  // Python is invoked via the repo-local venv when available; the test is skipped
  // for the Python surface only when no usable interpreter is found, so it never
  // silently passes against a missing surface — the other three still run.
  // -------------------------------------------------------------------------
  test('CF-SM2: napi, WASM, CLI, and Python produce identical sources[] for nested @import fixture', async () => {
    const dir = await mkdtemp(join(tmpdir(), 'cf-sm2-'));
    try {
      // -- Setup fixture -------------------------------------------------------
      await writeFile(join(dir, '.mdsroot'), '');
      await mkdir(join(dir, 'src'), { recursive: true });
      await mkdir(join(dir, 'partials'), { recursive: true });
      await writeFile(
        join(dir, 'partials', 'greeting.mds'),
        '@define greet():\nHello!\n@end\n@export greet\n',
      );
      await writeFile(
        join(dir, 'src', 'entry.mds'),
        '@import "../partials/greeting.mds" as g\n{{g.greet()}}\n',
      );
      const entryPath = join(dir, 'src', 'entry.mds');

      // -- Surface 1: napi compileFile -----------------------------------------
      const napiResult = await compileFile(entryPath, { sourceMap: true });
      assert.ok(napiResult.sourceMap != null, 'napi compileFile must produce sourceMap');
      const napiSources = napiResult.sourceMap.sources;

      // -- Surface 2: WASM via buildModulesMap + wasmMod.compile ---------------
      const { entryFilename, modules } = await buildModulesMap(
        entryPath,
        (src) => wasmMod.scanImports(src),
      );
      const entrySource = modules[entryFilename];
      const wasmModules = { ...modules };
      delete wasmModules[entryFilename];
      const wasmResult = createWasmBackend(wasmMod).compile(entrySource, {
        filename: entryFilename,
        modules: wasmModules,
        sourceMap: true,
      });
      assert.ok(wasmResult.sourceMap != null, 'WASM path must produce sourceMap');
      const wasmSources = wasmResult.sourceMap.sources;

      // -- Surface 3: CLI sidecar at project root -------------------------------
      // Output at <dir>/out.md so the .map file anchors at the project root,
      // making CLI sources[] root-relative and byte-identical to the bindings.
      const mdsCli = findMdsCli();
      assert.ok(
        mdsCli != null,
        'mds CLI binary not found — set MDS_CLI_BIN or run `cargo build -p mds-cli`',
      );
      const outFile = join(dir, 'out.md');
      const mapFile = join(dir, 'out.md.map');
      const cliProc = spawnSync(mdsCli, ['build', '--source-map', '-o', outFile, entryPath], {
        encoding: 'utf-8',
      });
      assert.equal(
        cliProc.status,
        0,
        `CLI build --source-map failed (rc=${cliProc.status}): ${cliProc.stderr}`,
      );
      assert.ok(existsSync(mapFile), `CLI did not write sidecar map at ${mapFile}`);
      const cliMap = JSON.parse(readFileSync(mapFile, 'utf-8'));
      const cliSources = cliMap.sources;

      // -- Surface 4: Python binding compile_file -------------------------------
      // Use the repo-local venv Python which has the markdown_script module installed
      // by `maturin develop`, or the interpreter exported by MDS_PYTHON_BIN (CI).
      // In CI all four surfaces must run — a missing surface is a hard failure
      // (avoids PF-007: a gate that silently skips a surface reads as green).
      // Locally, a missing interpreter warns and skips the Python leg only.
      const python = findPythonForMarkdownScript();
      let pySources = null;
      if (python == null) {
        if (process.env.CI) {
          throw new Error(
            'CF-SM2: Python surface is required in CI but no interpreter was found. ' +
            'Set MDS_PYTHON_BIN to an interpreter that can import markdown_script, or ' +
            'install the binding with `pip install ./crates/mds-python`. ' +
            'A missing surface silently breaks the PF-007 cross-surface parity gate.',
          );
        }
        console.warn(
          'CF-SM2: skipping Python surface — no Python interpreter found ' +
          '(run `maturin develop` inside .venv to enable)',
        );
      } else {
        // Import markdown_script from the Python environment directly (site-packages).
        // Do NOT insert the source tree into sys.path: with `pip install`, the
        // compiled extension (_markdown_script.so) lands in site-packages, not in the
        // source crates/mds-python/python/ directory, so prepending the source
        // path causes `from ._markdown_script import` to fail (it finds __init__.py in
        // the source tree but the .so is elsewhere).  Importing from site-packages
        // works for both `pip install ./crates/mds-python` (CI) and
        // `maturin develop` (local), since both make `markdown_script` importable from
        // the standard path.  Pass entryPath as argv so no shell escaping is needed.
        const pyScript = [
          'import json, sys',
          'import markdown_script as m',
          'result = m.compile_file(sys.argv[1], source_map=True)',
          'print(json.dumps(result.source_map["sources"]))',
        ].join('\n');
        const pyProc = spawnSync(python, ['-c', pyScript, entryPath], {
          encoding: 'utf-8',
        });
        assert.equal(
          pyProc.status,
          0,
          `Python compile_file failed (rc=${pyProc.status}): ${pyProc.stderr}`,
        );
        pySources = JSON.parse(pyProc.stdout.trim());
      }

      // -- Differential assertions --------------------------------------------
      // All surfaces that ran must agree on sources[].
      assert.deepEqual(
        napiSources,
        wasmSources,
        `napi vs WASM sources[] mismatch (PF-007): ` +
          `napi=${JSON.stringify(napiSources)} wasm=${JSON.stringify(wasmSources)}`,
      );
      assert.deepEqual(
        napiSources,
        cliSources,
        `napi vs CLI sources[] mismatch: ` +
          `napi=${JSON.stringify(napiSources)} cli=${JSON.stringify(cliSources)}`,
      );
      if (pySources != null) {
        assert.deepEqual(
          napiSources,
          pySources,
          `napi vs Python sources[] mismatch: ` +
            `napi=${JSON.stringify(napiSources)} python=${JSON.stringify(pySources)}`,
        );
      }

      // Extra invariant: all sources[] entries must be root-relative (not absolute).
      for (const src of napiSources) {
        assert.ok(
          !src.startsWith('/') && !src.match(/^[A-Za-z]:[/\\]/),
          `sources[] entry must be root-relative, not absolute: ${src}`,
        );
      }
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  });
});

// ---------------------------------------------------------------------------
// Regression: file-op error-contract (issue #35)
//
// checkFile and lintFile must NOT be async functions.  When they were async,
// assertKnownKeys() (which runs synchronously before any I/O) would fire
// inside an async body, converting a synchronous throw into a promise
// rejection.  This silently changed the public error contract and was
// invisible to assert.rejects() because Node's assert.rejects(fn) also
// accepts synchronous throws from fn() — so the existing options-validation
// tests passed under BOTH the broken (async) and correct (sync) forms.
//
// The tests below are intentionally SYNCHRONOUS (not async functions) and
// use assert.throws, not assert.rejects.  assert.throws requires the callback
// to throw synchronously: if checkFile/lintFile were async they would return
// a rejected Promise instead of throwing, assert.throws would NOT catch it,
// and the test would fail with "Missing expected exception" — catching the
// regression that assert.rejects missed.
//
// assertKnownKeys fires before any I/O or backend access, so no real file
// path or init() call is required for these specific assertions.
// ---------------------------------------------------------------------------

describe('file-op error contract — sync throw regression (#35)', () => {
  before(() => init());

  test('checkFile throws synchronously (not a rejected promise) for an unknown option key', () => {
    // checkFile must be a plain function (not async).  If it were async,
    // this assert.throws call would see the function return without throwing
    // and fail with "Missing expected exception".
    assert.throws(
      () => { checkFile('/any.mds', { sourceMap: true }); },
      (err) => {
        assert.ok(isMdsError(err), `expected MdsError, got: ${err}`);
        assert.equal(err.code, 'mds::invalid_options',
          `expected mds::invalid_options, got: ${err.code}`);
        return true;
      },
      'checkFile must throw synchronously for unknown option keys (not reject a promise)',
    );
  });

  test('lintFile throws synchronously (not a rejected promise) for an unknown option key', () => {
    // Same contract for lintFile.
    assert.throws(
      () => { lintFile('/any.mds', { basePath: '.' }); },
      (err) => {
        assert.ok(isMdsError(err), `expected MdsError, got: ${err}`);
        assert.equal(err.code, 'mds::invalid_options',
          `expected mds::invalid_options, got: ${err.code}`);
        return true;
      },
      'lintFile must throw synchronously for unknown option keys (not reject a promise)',
    );
  });
});
