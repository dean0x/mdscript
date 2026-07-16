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
import { compile, compileFile, isMdsError, init } from '../dist/node.js';
import { SIMPLE_MDS } from './helpers.mjs';
import { initWasmNode, createWasmBackend } from '../dist/backend/wasm.js';

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
    // String-source compilation uses "<source>" as the entry label.
    assert.deepEqual(result.sourceMap.sources, ['<source>']);
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
  // (F-SM6). At the universal package level the adapter's compileOpt()
  // filters to known keys, so TypeScript type checking is the guard.

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
//   W-SM3: WASM and native backends produce byte-identical mappings for the
//          same input (AC-API-04 / ADR-002: shared core serializer)
//
// The WASM default filename is "input.mds" (vs "<source>" for the native
// backend), so sources[] will differ by convention; mappings are compared
// without the filename entry.
// ---------------------------------------------------------------------------

describe('source maps — WASM backend (W-SM)', () => {
  let wasmBackend;

  before(async () => {
    const wasmMod = await initWasmNode();
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

  // ── W-SM3: WASM vs native byte-identical parity (AC-API-04) ───────────
  //
  // Both backends delegate to the same mds-core serializer, so mappings,
  // version, and names must be byte-identical. sources[] is intentionally
  // excluded from this check because the WASM backend uses a different
  // default filename convention ("input.mds" vs "<source>").

  test('W-SM3: WASM and native backends produce byte-identical mappings for same input', () => {
    const src = 'Hello World!\n';
    const nativeResult = compile(src, { sourceMap: true });
    const wasmResult = wasmBackend.compile(src, { sourceMap: true });

    assert.ok(nativeResult.sourceMap != null, 'native must produce sourceMap');
    assert.ok(wasmResult.sourceMap != null, 'wasm must produce sourceMap');

    assertSmStructure(wasmResult.sourceMap);

    assert.equal(
      nativeResult.sourceMap.version,
      wasmResult.sourceMap.version,
      'version must match across backends',
    );
    assert.deepEqual(
      nativeResult.sourceMap.names,
      wasmResult.sourceMap.names,
      'names must match across backends',
    );
    assert.equal(
      nativeResult.sourceMap.mappings,
      wasmResult.sourceMap.mappings,
      'mappings must be byte-identical across backends (ADR-002: shared core serializer)',
    );
  });
});
