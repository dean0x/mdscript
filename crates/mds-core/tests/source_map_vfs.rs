//! Integration tests for Source Map v3 generation via the VirtualFs pipeline.
//!
//! These tests drive the full compiler pipeline (parse → evaluate → finalize)
//! through the public API and assert on the produced [`SourceMap`] structure.
//!
//! CP2 tests cover one acceptance criterion each:
//!
//! - Basic source map generation for a single-file template
//! - CR (`\r\n`) compensation
//! - Frontmatter prefix shift
//! - @extends / spliced regions (multi-source segments)
//! - `source_map: false` produces `source_map: None` in [`CompileResult`]
//! - `to_canonical_json` includes the `"sourceMap"` key only when present
//!
//! CP3 tests cover `@include` `FragmentMap` splicing (S6):
//!
//! - 2-file attribution: partial content maps to source index 1
//! - 3-file attribution: two independent partials, both referenced
//! - Dedup: same partial included twice → ONE `sources` entry (AC-FUNC-04)
//! - `@for` loop: N iterations do not duplicate `sources` (AC-PERF-05)
//! - Nested compose: outer includes inner, all three sources present bottom-up
//! - Determinism: repeated compilations produce identical mappings (AC-FUNC-05)
//! - Output invariance: compiled text byte-identical with/without source maps (AC-PERF-02)
//!
//! CP4 tests cover S7/S8/S9:
//!
//! - S8 body attribution: function defined in imported file attributed to that file
//! - S8 nested trim: f→g→h composition with leading/trailing whitespace
//! - AC-FUNC-06: sourcesContent present when on, absent when off
//! - AC-FUNC-07: messages-mode template → `source_map: None` + warning
//! - AC-PERF-03: segment cap overflow → `source_map: None` + warning
//! - AC-PERF-04: large multibyte line — map present and VLQ alphabet clean
//!
//! #114 tests pin `@extends` on every evaluation path it can take (maps on, maps
//! off, messages mode, imported module):
//!
//! - ADR-002: inherited output byte-identical with and without source maps
//! - REL-1: one loop-iteration budget per extends chain, not per spliced region
//! - AC-114-3: one message-byte budget per extends chain, not per spliced region
//! - PF-004: one output-size budget per extends chain, not per spliced region
//!
//! #415 tests pin that the output cap is checked before each append: a `@for` of
//! exactly the cap compiles, one byte more fails, and a crossing loop stops on the
//! pass that crosses the cap in an `@extends` region and in an included module.
//!
//! #416 pins that a source-mapped `@extends` compile stays linear in its region count:
//! the builder shares each module's source instead of copying it per region.
//!
//! #412 pins that an `@include` of an extending module is source-mapped: each included
//! byte maps to the chain file that wrote it, exactly as a direct compile of the chain
//! maps it; output is unchanged; no map is kept for an empty body; the segment-cap
//! warning is truthful; the sourcesContent ceiling covers the chain's files; and a
//! diagnostic in a file first registered by the splice names it root-relative.

use std::collections::HashMap;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use mds::{
    CompileOptions, CompileResult, CompiledOutput, MdsError, SerializedError, SerializedSpan, Value,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn vfs_opts(modules: HashMap<String, String>, entry: &str, opts: CompileOptions) -> CompileResult {
    mds::compile_virtual_with_deps_opts(modules, entry, None, opts)
        .expect("compilation should succeed")
}

fn vfs_with_map(modules: HashMap<String, String>, entry: &str) -> CompileResult {
    // include_sources_content: true so that existing sourcesContent assertions continue to pass.
    vfs_opts(
        modules,
        entry,
        CompileOptions::default()
            .with_source_map(true)
            .with_include_sources_content(true),
    )
}

fn vfs_no_map(modules: HashMap<String, String>, entry: &str) -> CompileResult {
    vfs_opts(modules, entry, CompileOptions::default())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// AC-API-01: basic source map is produced for a single-file template.
///
/// Template: "Hello {name}!\n" with frontmatter `name: World`.
/// Expected: source_map is present with version=3, sources=["main.mds"],
/// non-empty mappings.
#[test]
fn source_map_basic_single_file() {
    let mut modules = HashMap::new();
    modules.insert(
        "main.mds".to_string(),
        "---\nname: World\n---\nHello {{name}}!\n".to_string(),
    );

    let result = vfs_with_map(modules, "main.mds");
    let sm = result.source_map.expect("source_map should be present");

    assert_eq!(sm.version, 3, "version must be 3");
    assert_eq!(sm.sources, vec!["main.mds"], "source name must match entry");
    assert!(
        sm.sources_content.is_some(),
        "sourcesContent must be present"
    );
    assert!(!sm.mappings.is_empty(), "mappings must be non-empty");
    // Security: VLQ alphabet must not contain '-', '<', '>'
    assert!(
        !sm.mappings.contains('-'),
        "VLQ alphabet must not contain '-'"
    );
    assert!(
        !sm.mappings.contains('<'),
        "VLQ alphabet must not contain '<'"
    );
    assert!(
        !sm.mappings.contains('>'),
        "VLQ alphabet must not contain '>'"
    );
}

/// AC-API-02: source_map absent (not null) when source_map option is false.
///
/// Verifies the JSON `skip_serializing_if` contract: when source_map is disabled
/// the field must be entirely absent from JSON output, not serialized as null.
#[test]
fn source_map_absent_when_disabled() {
    let mut modules = HashMap::new();
    modules.insert(
        "main.mds".to_string(),
        "---\nname: World\n---\nHello {{name}}!\n".to_string(),
    );

    let result = vfs_no_map(modules, "main.mds");

    // In-memory: None
    assert!(
        result.source_map.is_none(),
        "source_map field must be None when disabled"
    );

    // In JSON: absent entirely (not serialized as null)
    let json = serde_json::to_string(&result).expect("should serialize");
    assert!(
        !json.contains("sourceMap"),
        "sourceMap key must not appear in JSON when disabled; got: {json}"
    );
}

/// AC-PERF-01: zero-cost when disabled — no MapBuilder allocated.
///
/// This is observable by ensuring compilation with source_map=false succeeds
/// and produces correct output with no source_map overhead.  The absence of
/// source_map in the result is the observable proxy for no allocation.
#[test]
fn source_map_disabled_zero_cost_observable() {
    let mut modules = HashMap::new();
    modules.insert(
        "main.mds".to_string(),
        "---\nname: World\n---\nHello {{name}}!\n".to_string(),
    );

    let result = vfs_no_map(modules.clone(), "main.mds");
    assert!(result.source_map.is_none());

    // Output must still be correct
    let output = result.into_markdown().expect("markdown output");
    assert!(output.contains("Hello World!"), "output: {output}");
}

/// S5/stage-2: CR compensation — raw output with `\r\n` produces a source map
/// with mappings that align to the clean (LF-only) output.
///
/// Template content with no frontmatter: "Line1\nLine2\n"
/// Source map must be present and non-empty.  The key invariant is that
/// mappings use LF-based offsets (not CRLF-based).
#[test]
fn source_map_cr_compensation() {
    // We inject CRLF by providing a template whose raw evaluator output would
    // contain \r\n.  Since the evaluator itself doesn't produce \r, we verify
    // via the finalize stage unit test; here we just ensure the pipeline
    // survives CRLF in the *source* template content without panicking and
    // produces a non-empty source map.
    let mut modules = HashMap::new();
    modules.insert(
        "main.mds".to_string(),
        "Hello World\r\nSecond line\r\n".to_string(),
    );

    let result = vfs_with_map(modules, "main.mds");
    let sm = result.source_map.expect("source_map should be present");

    assert_eq!(sm.version, 3);
    assert!(!sm.mappings.is_empty(), "mappings must be non-empty");
    // No '-' in VLQ output
    assert!(!sm.mappings.contains('-'), "VLQ must not contain '-'");
}

/// S5/stage-4: frontmatter shift — source map for a template with frontmatter
/// must have semicolons in mappings (multiple output lines from FM prefix).
///
/// "---\nfm: v\n---\nHello\n" produces a 4-line output. Lines 0–2 are
/// frontmatter; line 3 is the body content.  The mappings string must contain
/// semicolons separating frontmatter lines from body lines.
#[test]
fn source_map_frontmatter_shift() {
    let mut modules = HashMap::new();
    modules.insert(
        "main.mds".to_string(),
        "---\nfm: v\n---\nHello\n".to_string(),
    );

    let result = vfs_with_map(modules, "main.mds");
    let sm = result.source_map.expect("source_map should be present");

    // "---\nfm: v\n---\nHello\n" → 4 lines → 3 semicolons minimum in mappings
    let semicolons = sm.mappings.chars().filter(|&c| c == ';').count();
    assert!(
        semicolons >= 3,
        "mappings must have ≥3 semicolons for a 4-line output; got {semicolons}: {:?}",
        sm.mappings
    );
}

/// S5/stage-4: template with no frontmatter produces a single-line mappings
/// string (no semicolons).
#[test]
fn source_map_no_frontmatter_no_semicolons() {
    let mut modules = HashMap::new();
    modules.insert("main.mds".to_string(), "Hello World\n".to_string());

    let result = vfs_with_map(modules, "main.mds");
    let sm = result.source_map.expect("source_map should be present");

    assert!(
        !sm.mappings.contains(';'),
        "single-line output must not contain semicolons; got: {:?}",
        sm.mappings
    );
}

/// AC-API-03 / to_canonical_json: sourceMap key present when enabled.
///
/// Verifies that `to_canonical_json()` includes `"sourceMap"` when
/// `source_map: Some(...)` and omits it when `source_map: None`.
#[test]
fn to_canonical_json_includes_source_map_key_when_present() {
    let mut modules = HashMap::new();
    modules.insert(
        "main.mds".to_string(),
        "---\nname: World\n---\nHello {{name}}!\n".to_string(),
    );

    // With source map
    let result_with = vfs_with_map(modules.clone(), "main.mds");
    let json_with = result_with.to_canonical_json();
    assert!(
        json_with.get("sourceMap").is_some(),
        "to_canonical_json must include sourceMap when enabled; got: {json_with}"
    );

    // Without source map
    let result_without = vfs_no_map(modules, "main.mds");
    let json_without = result_without.to_canonical_json();
    assert!(
        json_without.get("sourceMap").is_none(),
        "to_canonical_json must omit sourceMap when disabled; got: {json_without}"
    );
}

/// @extends path: the source map of a template using @extends lists the base (the
/// skeleton template) first, then the child, and maps output to both.
#[test]
fn source_map_extends_multi_source() {
    let mut modules = HashMap::new();
    modules.insert(
        "base.mds".to_string(),
        "# Title\n\n@block body:\nDefault body\n@end\n".to_string(),
    );
    modules.insert(
        "child.mds".to_string(),
        "@extends \"./base.mds\"\n@block body:\nChild content\n@end\n".to_string(),
    );

    let result = vfs_with_map(modules, "child.mds");
    let sm = result
        .source_map
        .expect("source_map should be present for @extends");

    assert_eq!(sm.version, 3);
    // The base owns the skeleton text and seeds the map; the child owns its override.
    assert_eq!(
        sm.sources,
        vec!["base.mds", "child.mds"],
        "both base and child must appear as sources, base first"
    );
    assert_eq!(
        referenced_src_indices(&sm.mappings),
        std::collections::HashSet::from([0, 1]),
        "both base and child must own mapped output; mappings={:?}",
        sm.mappings
    );
}

/// AC-PERF-02 (@extends): compiled output for an `@extends` template must be
/// byte-identical whether source maps are on or off. This is the riskiest path
/// for divergence because both evaluate per spliced region
/// (`evaluate_regions_with_map`), but only map-on records segments and moves
/// the builder's cursor — the two must produce the same bytes.
#[test]
fn source_map_extends_output_unchanged() {
    let mut modules = HashMap::new();
    modules.insert(
        "base.mds".to_string(),
        "# Title\n\n@block intro:\nDefault intro\n@end\n\nMiddle\n\n@block body:\nDefault body\n@end\n\nFooter\n".to_string(),
    );
    modules.insert(
        "child.mds".to_string(),
        "@extends \"./base.mds\"\n@block body:\nChild content\n@end\n".to_string(),
    );

    let with_map = vfs_with_map(modules.clone(), "child.mds")
        .into_markdown()
        .expect("markdown with source map");
    let without_map = vfs_no_map(modules, "child.mds")
        .into_markdown()
        .expect("markdown without source map");

    assert_eq!(
        with_map, without_map,
        "@extends compiled output must be byte-identical regardless of source_map setting"
    );
}

/// S3/suppression: function-call interpolation records call-site only.
///
/// When a `@define`d function is invoked, the *body* nodes are suppressed
/// and only the Interpolation call-site is recorded.  The source map should
/// still be non-empty with at least one segment.
#[test]
fn source_map_function_call_suppression() {
    let mut modules = HashMap::new();
    modules.insert(
        "main.mds".to_string(),
        "@define greet(x):\nHello {{x}}!\n@end\n\n{{greet(\"World\")}}\n".to_string(),
    );

    let result = vfs_with_map(modules, "main.mds");
    let sm = result.source_map.expect("source_map should be present");

    assert_eq!(sm.version, 3);
    assert!(
        !sm.mappings.is_empty(),
        "function-call source map must not be empty"
    );
}

/// sourcesContent is populated with template source text.
#[test]
fn source_map_sources_content_populated() {
    let source_text = "---\nname: World\n---\nHello {{name}}!\n";
    let mut modules = HashMap::new();
    modules.insert("main.mds".to_string(), source_text.to_string());

    let result = vfs_with_map(modules, "main.mds");
    let sm = result.source_map.expect("source_map should be present");

    let contents = sm.sources_content.expect("sourcesContent must be present");
    assert_eq!(contents.len(), 1, "one source → one sourcesContent entry");
    assert_eq!(
        contents[0], source_text,
        "sourcesContent must match original template source"
    );
}

// ══════════════════════════════════════════════════════════════════════════════
// CP3 tests — @include FragmentMap splice (S6)
// ══════════════════════════════════════════════════════════════════════════════

// ── VLQ decode helpers ────────────────────────────────────────────────────────

/// Decode one Base64-VLQ signed integer from the start of `bytes`.
/// Returns `(decoded_value, remaining_bytes)`.
fn vlq_decode_one(bytes: &[u8]) -> (i64, &[u8]) {
    let mut acc: u64 = 0;
    let mut shift: u32 = 0;
    let mut i = 0;
    loop {
        let v: u64 = match bytes[i] {
            b'A'..=b'Z' => (bytes[i] - b'A') as u64,
            b'a'..=b'z' => (bytes[i] - b'a') as u64 + 26,
            b'0'..=b'9' => (bytes[i] - b'0') as u64 + 52,
            b'+' => 62,
            b'/' => 63,
            c => panic!("invalid VLQ char: {}", c as char),
        };
        i += 1;
        acc |= (v & 0x1F) << shift;
        shift += 5;
        if v & 0x20 == 0 {
            break;
        }
    }
    // zigzag decode: LSB is sign
    let n: i64 = if acc & 1 == 1 {
        -((acc >> 1) as i64)
    } else {
        (acc >> 1) as i64
    };
    (n, &bytes[i..])
}

/// Collect the set of all source-file indices referenced by mapped segments.
///
/// Correctly accumulates `src_base` across lines (semicolons reset only
/// `gen_col`; `src_base` is cumulative across the entire mappings string).
fn referenced_src_indices(mappings: &str) -> std::collections::HashSet<u32> {
    let mut seen = std::collections::HashSet::new();
    let mut src_base: i64 = 0;
    for line_str in mappings.split(';') {
        for seg_str in line_str.split(',') {
            if seg_str.is_empty() {
                continue;
            }
            let (_, rest) = vlq_decode_one(seg_str.as_bytes()); // genCol delta
            if rest.is_empty() {
                continue; // 1-field (unmapped) segment — no source info
            }
            let (ds, _) = vlq_decode_one(rest); // srcIdx delta
            src_base += ds;
            seen.insert(src_base as u32);
        }
    }
    seen
}

// ── CP3 integration tests ─────────────────────────────────────────────────────

/// S6 / 2-file attribution: entry @import s + @include s a partial; the
/// produced source map must list both files in `sources` and attribute the
/// partial's output bytes to source index 1.
#[test]
fn source_map_include_two_file_attribution() {
    let entry_src = "@import \"./partial.mds\" as p\n@include p\n";
    let partial_src = "Hello from partial\n";

    let mut modules = HashMap::new();
    modules.insert("entry.mds".to_string(), entry_src.to_string());
    modules.insert("partial.mds".to_string(), partial_src.to_string());

    let result = vfs_with_map(modules, "entry.mds");
    let sm = result.source_map.expect("source_map must be present");

    // Source list: entry seeded at 0, partial added on first splice.
    assert_eq!(
        sm.sources,
        vec!["entry.mds", "partial.mds"],
        "sources must list entry first, then partial"
    );

    // sourcesContent mirrors sources.
    let contents = sm.sources_content.expect("sourcesContent must be present");
    assert_eq!(contents.len(), 2);
    assert_eq!(
        contents[0], entry_src,
        "sourcesContent[0] must be entry source"
    );
    assert_eq!(
        contents[1], partial_src,
        "sourcesContent[1] must be partial source"
    );

    // At least one output byte attributed to source 1 (the partial).
    assert!(!sm.mappings.is_empty(), "mappings must be non-empty");
    let src_indices = referenced_src_indices(&sm.mappings);
    assert!(
        src_indices.contains(&1),
        "partial content must be attributed to source index 1; indices={:?} mappings={:?}",
        src_indices,
        sm.mappings
    );
}

/// S6 / 3-file, two independent partials: entry includes a.mds then b.mds;
/// all three files appear in `sources` in interner order (entry, a, b) and
/// both partial source indices appear in the decoded mappings.
#[test]
fn source_map_include_three_file_two_partials() {
    let mut modules = HashMap::new();
    modules.insert(
        "entry.mds".to_string(),
        "@import \"./a.mds\" as a\n@import \"./b.mds\" as b\n@include a\n@include b\n".to_string(),
    );
    modules.insert("a.mds".to_string(), "Part A\n".to_string());
    modules.insert("b.mds".to_string(), "Part B\n".to_string());

    let result = vfs_with_map(modules, "entry.mds");
    let sm = result.source_map.expect("source_map must be present");

    // Interner order: entry (seed), a (first splice), b (second splice).
    assert_eq!(
        sm.sources,
        vec!["entry.mds", "a.mds", "b.mds"],
        "sources must list all three files in splice order"
    );

    // Both partials referenced in segments.
    let src_indices = referenced_src_indices(&sm.mappings);
    assert!(
        src_indices.contains(&1),
        "a.mds content must be attributed to source 1"
    );
    assert!(
        src_indices.contains(&2),
        "b.mds content must be attributed to source 2"
    );
}

/// S6 / AC-FUNC-04 dedup: the same partial included twice produces exactly ONE
/// `sources` entry for that partial, not two.  Both output regions map to
/// source index 1.
#[test]
fn source_map_include_dedup_same_partial_twice() {
    let mut modules = HashMap::new();
    modules.insert(
        "entry.mds".to_string(),
        "@import \"./partial.mds\" as p\n@include p\n@include p\n".to_string(),
    );
    modules.insert("partial.mds".to_string(), "Line\n".to_string());

    let result = vfs_with_map(modules, "entry.mds");
    let sm = result.source_map.expect("source_map must be present");

    // Two @include s of the same partial → only ONE extra sources entry.
    assert_eq!(
        sm.sources.len(),
        2,
        "dedup: two includes of the same partial must yield exactly 2 sources; got {:?}",
        sm.sources
    );
    assert_eq!(sm.sources[1], "partial.mds");

    let contents = sm.sources_content.expect("sourcesContent must be present");
    assert_eq!(contents.len(), 2, "sourcesContent must mirror sources");

    // All segments map to source 1 only.
    let src_indices = referenced_src_indices(&sm.mappings);
    assert_eq!(
        src_indices.len(),
        1,
        "only source index 1 must be referenced; got {:?}",
        src_indices
    );
    assert!(src_indices.contains(&1), "source 1 must be referenced");
}

/// S6 / AC-PERF-05: @include inside @for — N iterations must not duplicate
/// `sources`.  The local→global remap is built once and reused across
/// iterations; the observable invariant is `sources.len() == 2` regardless
/// of iteration count.
#[test]
fn source_map_include_for_loop_sources_not_duplicated() {
    // 5 iterations; each @include p splices the same partial once.
    let entry_src = "---\nitems: [a, b, c, d, e]\n---\n\
                     @import \"./partial.mds\" as p\n\
                     @for item in items:\n\
                     @include p\n\
                     @end\n";

    let mut modules = HashMap::new();
    modules.insert("entry.mds".to_string(), entry_src.to_string());
    modules.insert("partial.mds".to_string(), "Repeated line\n".to_string());

    let result = vfs_with_map(modules, "entry.mds");
    let sm = result.source_map.expect("source_map must be present");

    // 5 iterations must not produce 6 sources — interner dedup keeps it at 2.
    assert_eq!(
        sm.sources.len(),
        2,
        "@for with 5 iterations must not duplicate sources; got {:?}",
        sm.sources
    );
    assert_eq!(sm.sources[1], "partial.mds");

    // Body segments all attributed to source 1 (the partial).
    let src_indices = referenced_src_indices(&sm.mappings);
    assert!(
        src_indices.contains(&1),
        "partial segments must be attributed to source 1; got {:?}",
        src_indices
    );
}

/// S6 / nested compose: outer partial @include s inner partial; the outer's
/// `FragmentMap` is built bottom-up so it already carries inner's segments.
/// When entry splices outer, all three sources appear in the final map.
#[test]
fn source_map_include_nested_compose() {
    let mut modules = HashMap::new();
    modules.insert("inner.mds".to_string(), "Inner text\n".to_string());
    modules.insert(
        "outer.mds".to_string(),
        "@import \"./inner.mds\" as inner\n@include inner\nOuter text\n".to_string(),
    );
    modules.insert(
        "entry.mds".to_string(),
        "@import \"./outer.mds\" as outer\n@include outer\n".to_string(),
    );

    let result = vfs_with_map(modules, "entry.mds");
    let sm = result.source_map.expect("source_map must be present");

    // All three sources present: entry (seed), outer, inner (nested splice).
    assert_eq!(
        sm.sources.len(),
        3,
        "nested @include must bring all 3 sources; got {:?}",
        sm.sources
    );
    assert!(
        sm.sources.contains(&"outer.mds".to_string()),
        "outer.mds must be in sources; got {:?}",
        sm.sources
    );
    assert!(
        sm.sources.contains(&"inner.mds".to_string()),
        "inner.mds must be in sources; got {:?}",
        sm.sources
    );

    // Segments reference both outer and inner source indices.
    let src_indices = referenced_src_indices(&sm.mappings);
    assert!(
        src_indices.len() >= 2,
        "must reference at least outer and inner source indices; got {:?}",
        src_indices
    );
}

/// S6 / AC-FUNC-05 (core half): two separate compilations of the same template
/// produce identical `sources` ordering and identical `mappings` strings.
#[test]
fn source_map_include_deterministic() {
    let mut modules = HashMap::new();
    modules.insert(
        "entry.mds".to_string(),
        "@import \"./a.mds\" as a\n@import \"./b.mds\" as b\n@include a\n@include b\n".to_string(),
    );
    modules.insert("a.mds".to_string(), "Alpha\n".to_string());
    modules.insert("b.mds".to_string(), "Beta\n".to_string());

    let sm1 = vfs_with_map(modules.clone(), "entry.mds")
        .source_map
        .expect("first compilation must produce source_map");
    let sm2 = vfs_with_map(modules, "entry.mds")
        .source_map
        .expect("second compilation must produce source_map");

    assert_eq!(
        sm1.sources, sm2.sources,
        "sources order must be deterministic"
    );
    assert_eq!(sm1.mappings, sm2.mappings, "mappings must be deterministic");
}

/// AC-PERF-02: compiled output is byte-identical whether source maps are
/// enabled or disabled — the `MapBuilder` is transparent to the evaluator.
#[test]
fn source_map_include_output_unchanged() {
    let mut modules = HashMap::new();
    modules.insert(
        "entry.mds".to_string(),
        "@import \"./a.mds\" as a\n@import \"./b.mds\" as b\n@include a\n@include b\n".to_string(),
    );
    modules.insert("a.mds".to_string(), "Part A\n".to_string());
    modules.insert("b.mds".to_string(), "Part B\n".to_string());

    let with_map = vfs_with_map(modules.clone(), "entry.mds")
        .into_markdown()
        .expect("markdown with source map");
    let without_map = vfs_no_map(modules, "entry.mds")
        .into_markdown()
        .expect("markdown without source map");

    assert_eq!(
        with_map, without_map,
        "compiled output must be byte-identical regardless of source_map setting"
    );
}

// ══════════════════════════════════════════════════════════════════════════════
// CP4 tests — S7/S8/S9: provenance, fine-grained body mapping, bounds
// ══════════════════════════════════════════════════════════════════════════════

/// S8 body attribution: a function defined in an imported file must appear in
/// `sources` when its body is evaluated under source-map mode (S8 path).
///
/// Without S8 provenance, the library file would never appear in `sources`
/// because the body is suppressed (S3 path attributes output to the call site).
/// With S8, the body tokens are recorded against the definition file, so
/// `sources` must contain both `entry.mds` and `lib.mds`.
#[test]
fn source_map_s8_cross_file_function_attribution() {
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define greet(who):\nHello {{who}}!\n@end\n".to_string(),
    );
    modules.insert(
        "entry.mds".to_string(),
        "@import \"./lib.mds\" as lib\n{{lib.greet(\"World\")}}\n".to_string(),
    );

    let result = vfs_with_map(modules.clone(), "entry.mds");
    let sm = result
        .source_map
        .clone()
        .expect("source_map must be present");

    // Compiled text must still be correct (AC-PERF-02: output unchanged).
    let text = result.into_markdown().expect("markdown output");
    assert_eq!(
        text.trim(),
        "Hello World!",
        "S8 must not alter compiled output"
    );

    // S8: the definition file must appear in sources.
    assert!(
        sm.sources.contains(&"lib.mds".to_string()),
        "lib.mds must appear in sources (S8 body attribution); got: {:?}",
        sm.sources
    );
    assert!(
        sm.sources.contains(&"entry.mds".to_string()),
        "entry.mds must appear in sources; got: {:?}",
        sm.sources
    );

    // sourcesContent must include lib.mds content.
    let sc = sm
        .sources_content
        .as_ref()
        .expect("sourcesContent must be present");
    assert_eq!(
        sc.len(),
        sm.sources.len(),
        "sourcesContent entries must match sources count"
    );
    let lib_idx = sm
        .sources
        .iter()
        .position(|s| s == "lib.mds")
        .expect("lib.mds position");
    assert!(
        sc[lib_idx].contains("@define greet"),
        "sourcesContent for lib.mds must contain its source"
    );

    // Without source maps, output is identical.
    let no_map_text = vfs_no_map(modules, "entry.mds")
        .into_markdown()
        .expect("markdown without map");
    assert_eq!(text, no_map_text, "output must be map-independent");
}

/// S8 nested trim: function composition f→g→h where each level adds
/// leading/trailing whitespace.  After rebase_trim at each level, only
/// the actual content tokens are attributed to the definition files.
///
/// Verifies:
/// - Compilation succeeds and output is trimmed correctly.
/// - All three definition files appear in `sources`.
/// - VLQ alphabet is valid (no `-`, `<`, `>`).
#[test]
fn source_map_s8_nested_trim_composition() {
    let mut modules = HashMap::new();
    // h produces content with extra surrounding whitespace.
    modules.insert(
        "h.mds".to_string(),
        "@define h(x):\n   inner {{x}}   \n@end\n".to_string(),
    );
    // g calls h, adding its own wrapper whitespace.
    modules.insert(
        "g.mds".to_string(),
        "@import \"./h.mds\" as hm\n@define g(x):\n  {{hm.h(x)}}  \n@end\n".to_string(),
    );
    // f calls g.
    modules.insert(
        "f.mds".to_string(),
        "@import \"./g.mds\" as gm\n@define f(x):\n{{gm.g(x)}}\n@end\n".to_string(),
    );
    // entry calls f.
    modules.insert(
        "entry.mds".to_string(),
        "@import \"./f.mds\" as fm\nResult: {{fm.f(\"test\")}}\n".to_string(),
    );

    let result = vfs_with_map(modules, "entry.mds");
    let sm = result
        .source_map
        .clone()
        .expect("source_map must be present");

    // Compiled output must be correct.
    let text = result
        .into_markdown()
        .expect("markdown for nested-trim test");
    assert!(
        text.contains("inner test"),
        "nested trim must produce correct content; got: {text:?}"
    );

    // All definition files must appear in sources.
    for expected in &["h.mds", "g.mds", "f.mds", "entry.mds"] {
        assert!(
            sm.sources.iter().any(|s| s == expected),
            "{expected} must appear in sources for nested-trim S8; got: {:?}",
            sm.sources
        );
    }

    // VLQ alphabet must be valid.
    assert!(
        !sm.mappings.contains('-'),
        "VLQ mappings must not contain '-'"
    );
    assert!(
        !sm.mappings.contains('<'),
        "VLQ mappings must not contain '<'"
    );
    assert!(
        !sm.mappings.contains('>'),
        "VLQ mappings must not contain '>'"
    );
}

/// AC-FUNC-06: `sourcesContent` is present when source_map=true and absent
/// when source_map=false (i.e. source_map field is None).
///
/// Also verifies that `sourcesContent[0]` matches the template source byte-for-byte.
#[test]
fn source_map_sources_content_on_off() {
    let source_text = "Hello world\n";
    let mut modules = HashMap::new();
    modules.insert("main.mds".to_string(), source_text.to_string());

    // source_map=true → sources_content is Some with matching content.
    let with_map = vfs_with_map(modules.clone(), "main.mds");
    let sm = with_map
        .source_map
        .expect("source_map must be Some when source_map=true");
    let sc = sm
        .sources_content
        .expect("sourcesContent must be present when source_map=true");
    assert_eq!(
        sc.len(),
        1,
        "single-source compilation must have one sourcesContent entry"
    );
    assert_eq!(
        sc[0], source_text,
        "sourcesContent must match original template source byte-for-byte"
    );

    // source_map=false → source_map field is None (AC-PERF-01 zero-cost path).
    let no_map = vfs_no_map(modules, "main.mds");
    assert!(
        no_map.source_map.is_none(),
        "source_map must be None when source_map=false"
    );
}

/// AC-FUNC-07: a template with `@message` blocks in messages mode combined
/// with `source_map: true` must return `source_map: None` and emit a warning.
///
/// Source maps operate on the flat text output stream; messages-mode boundaries
/// are not representable in the SMv3 segment model, so we degrade gracefully.
#[test]
fn source_map_messages_mode_degrades_to_none() {
    let mut modules = HashMap::new();
    modules.insert(
        "chat.mds".to_string(),
        "@message role=user:\nHello!\n@end\n".to_string(),
    );

    let result = vfs_opts(
        modules,
        "chat.mds",
        CompileOptions::default().with_source_map(true),
    );

    // source_map must be None (messages-mode degrades gracefully).
    assert!(
        result.source_map.is_none(),
        "messages-mode with source_map=true must degrade to None"
    );

    // A warning must be emitted explaining the degradation (AC-FUNC-07).
    // The warning uses MSG_MODE_SOURCE_MAP_WARNING (surface-neutral wording).
    let matching_warnings: Vec<&String> = result
        .warnings
        .iter()
        .filter(|w| w.contains("messages-mode") && w.contains("no source map will be generated"))
        .collect();
    assert!(
        !matching_warnings.is_empty(),
        "AC-FUNC-07: must emit a warning for messages-mode + source_map=true; \
         got warnings: {:?}",
        result.warnings
    );
    // Deduplicated: the warning must appear EXACTLY ONCE per compilation.
    // (Previously the same literal string was present in two code paths; MSG_MODE_SOURCE_MAP_WARNING
    // const was introduced to enforce a single canonical string — this test guards against regression
    // where both paths fire for the same input.)
    assert_eq!(
        matching_warnings.len(),
        1,
        "AC-FUNC-07: messages-mode degradation warning must appear exactly once; \
         got {} occurrences in warnings: {:?}",
        matching_warnings.len(),
        result.warnings
    );
}

/// AC-PERF-03: when the segment cap (`MAX_SOURCEMAP_SEGMENTS`) is exceeded,
/// the result degrades to `source_map: None` rather than emitting a partial
/// (and potentially misleading) map.  A warning is emitted to the caller.
///
/// This test generates >1 000 000 segments by iterating over a large array
/// with a multi-token loop body (11 segment-producing nodes per iteration ×
/// 100 000 iterations = 1 100 000 > 1 000 000 cap).
#[test]
fn source_map_segment_cap_degrades_to_none() {
    use mds::Value;

    // Build a 100 000-element array at runtime (avoids a giant template literal).
    let items: Vec<Value> = (0..100_000)
        .map(|_| Value::String("x".to_string()))
        .collect();
    let mut vars = std::collections::HashMap::new();
    vars.insert("items".to_string(), Value::Array(items));

    let mut modules = std::collections::HashMap::new();
    // Body has 6 Text nodes + 5 Interpolation nodes = 11 segments per iteration.
    // 100 000 iters × 11 = 1 100 000 > MAX_SOURCEMAP_SEGMENTS (1 000 000).
    modules.insert(
        "big.mds".to_string(),
        "@for item in items:\nA{{item}}B{{item}}C{{item}}D{{item}}E{{item}}F\n@end\n".to_string(),
    );

    let result = mds::compile_virtual_with_deps_opts(
        modules,
        "big.mds",
        Some(vars),
        CompileOptions::default().with_source_map(true),
    )
    .expect("compilation must succeed even when cap is hit");

    // Extract source_map before consuming result via into_markdown.
    let source_map_none = result.source_map.is_none();
    let warnings = result.warnings.clone();

    // Compilation succeeds: output is present (no error).
    let _text = result
        .into_markdown()
        .expect("markdown output must be present");

    // Source map degrades to None (AC-PERF-03).
    assert!(
        source_map_none,
        "source_map must be None when segment cap is exceeded (AC-PERF-03)"
    );

    // A warning must be emitted.
    let has_warning = warnings.iter().any(|w| w.contains("segment cap"));
    assert!(
        has_warning,
        "AC-PERF-03: must emit a warning when segment cap is exceeded; \
         got warnings: {:?}",
        warnings
    );
}

/// AC-PERF-04: a template with long lines containing multibyte (UTF-8)
/// characters must produce a valid source map with clean VLQ alphabet.
///
/// This is a shape test: we verify the map is present, non-empty, and that
/// the VLQ encoding does not accidentally emit invalid characters.
#[test]
fn source_map_multibyte_line_vlq_alphabet() {
    let mut modules = HashMap::new();
    // Mix ASCII and CJK (3-byte UTF-8) characters on one long line.
    // The VLQ encoder must handle byte-position arithmetic correctly for
    // multi-byte codepoints (AC-PERF-04).
    let long_cjk_line = "你好世界".repeat(100); // 400 CJK chars = 1200 bytes
                                                // Embed as a frontmatter value so the template renders a long CJK string.
    let source_with_fm = format!("---\nval: \"{long_cjk_line}\"\n---\n{{{{val}}}}\n");
    modules.insert("cjk.mds".to_string(), source_with_fm);

    let result = vfs_with_map(modules, "cjk.mds");
    let sm = result.source_map.expect("source_map must be present");

    assert!(
        !sm.mappings.is_empty(),
        "mappings must be non-empty for multibyte template"
    );
    // VLQ alphabet must not contain forbidden characters.
    assert!(
        !sm.mappings.contains('-'),
        "VLQ must not contain '-' (AC-PERF-04)"
    );
    assert!(
        !sm.mappings.contains('<'),
        "VLQ must not contain '<' (AC-PERF-04)"
    );
    assert!(
        !sm.mappings.contains('>'),
        "VLQ must not contain '>' (AC-PERF-04)"
    );
}

/// S8 output invariance: function defined in an imported file compiles to the
/// same text regardless of whether source maps are enabled (AC-PERF-02).
#[test]
fn source_map_s8_output_unchanged() {
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define greet(who):\nHello {{who}}!\n@end\n".to_string(),
    );
    modules.insert(
        "entry.mds".to_string(),
        "@import \"./lib.mds\" as lib\n{{lib.greet(\"World\")}}\n".to_string(),
    );

    let with_map = vfs_with_map(modules.clone(), "entry.mds")
        .into_markdown()
        .expect("markdown with map");
    let without_map = vfs_no_map(modules, "entry.mds")
        .into_markdown()
        .expect("markdown without map");

    assert_eq!(
        with_map, without_map,
        "S8 must not change compiled output (AC-PERF-02)"
    );
}

// ── #114: @extends pins across every evaluation path ─────────────────────────
//
// Inheritance is declared ONLY by a body `@extends "./base.mds"` directive; the
// frontmatter key `extends:` is reserved and never triggers it. Every test below
// therefore proves the directive ran before trusting any other assertion (PF-013):
// the output must hold the base skeleton text around the child's override and must
// not hold the overridden base default. `reserved_frontmatter_extends_key_does_not_inherit`
// shows that check rejecting output compiled without inheritance.
//
// An @extends chain is evaluated as a sequence of spliced regions (base skeleton
// nodes, base-default blocks, child overrides), and it can be reached with source maps
// on, with them off, in messages mode, or as an imported module. Each cumulative
// resource budget is pinned on every path it governs: a budget seeded afresh per
// region would be multiplied by the region count (applies PF-004).

/// Mirrors the evaluator's private `MAX_TOTAL_ITERATIONS`. The tripping assertions
/// require the limit message, which prints the real value, so a drift between the
/// two fails every iteration-budget test below.
const MAX_TOTAL_ITERATIONS: usize = 1_000_000;

/// Mirrors the private `limits::MAX_OUTPUT_SIZE` (50 MiB = 52,428,800 bytes). Pinned
/// the same way: the over-cap assertions require the limit message, which prints the
/// real value.
const MAX_OUTPUT_SIZE: usize = 50 * 1024 * 1024;

/// Mirrors the private `limits::MAX_MESSAGES_TOTAL_SIZE` (= `MAX_OUTPUT_SIZE`). Pinned
/// the same way, and the at-cap control compiles.
const MAX_MESSAGES_TOTAL_SIZE: usize = MAX_OUTPUT_SIZE;

/// Mirrors the private `limits::MAX_MESSAGE_COUNT`. Pinned the same way: the over-cap
/// assertion requires the limit message, which prints the real value.
const MAX_MESSAGE_COUNT: usize = 10_000;

/// Outer-loop length for the iteration-budget tests. One loop region runs
/// `LOOP_OUTER * (inner + 1)` iterations: every outer and every inner pass counts.
const LOOP_OUTER: usize = 500;

/// `inner` length at which the chain's two loop regions together spend EXACTLY
/// `MAX_TOTAL_ITERATIONS` (2 × 500 × 1 000 = 1 000 000).
const AT_BUDGET_INNER: usize = MAX_TOTAL_ITERATIONS / (2 * LOOP_OUTER) - 1;

// The at-budget chain spends the budget exactly; one more inner pass per outer pass
// leaves each region well under the budget alone, so only a budget shared by both
// regions can trip. Both lengths stay under the evaluator's 100 000 per-loop cap.
const _: () = assert!(2 * LOOP_OUTER * (AT_BUDGET_INNER + 1) == MAX_TOTAL_ITERATIONS);
const _: () = assert!(LOOP_OUTER * (AT_BUDGET_INNER + 2) < MAX_TOTAL_ITERATIONS);
const _: () = assert!(2 * LOOP_OUTER * (AT_BUDGET_INNER + 2) > MAX_TOTAL_ITERATIONS);
const _: () = assert!(AT_BUDGET_INNER + 1 < 100_000);

/// Output-parity base: skeleton text around one overridable block.
const PARITY_BASE: &str =
    "BASE-SKELETON-HEAD\n\n@block content:\nBASE-DEFAULT-CONTENT\n@end\n\nBASE-SKELETON-TAIL\n";
const PARITY_CHILD: &str =
    "@extends \"./base.mds\"\n@block content:\nCHILD-OVERRIDE-CONTENT\n@end\n";
const PARITY_EXPECTED: &str =
    "BASE-SKELETON-HEAD\n\nCHILD-OVERRIDE-CONTENT\n\nBASE-SKELETON-TAIL\n";

/// Messages-mode parity base: a skeleton message, one overridable block, a
/// closing skeleton message.
const PARITY_BASE_MESSAGES: &str = "@message system:\nBASE-SKELETON-HEAD\n@end\n\n\
     @block turn:\n@message assistant:\nBASE-DEFAULT-CONTENT\n@end\n@end\n\n\
     @message system:\nBASE-SKELETON-TAIL\n@end\n";
const PARITY_CHILD_MESSAGES: &str =
    "@extends \"./base.mds\"\n@block turn:\n@message user:\nCHILD-OVERRIDE-CONTENT\n@end\n@end\n";

/// Iteration-budget base: one loop region in a base-default block (`warmup`, not
/// overridden) and one overridable block (`work`).
const BUDGET_BASE: &str = "BASE-SKELETON-HEAD\n\
     @block warmup:\n@for o in outer:\n@for i in inner:\n.\n@end\n@end\n@end\n\
     @block work:\nBASE-DEFAULT-WORK\n@end\n\
     BASE-SKELETON-TAIL\n";
/// The child's `work` override is the second loop region.
const BUDGET_CHILD: &str = "@extends \"./base.mds\"\n\
     @block work:\nCHILD-OVERRIDE-WORK\n@for o in outer:\n@for i in inner:\n.\n@end\n@end\n@end\n";

/// Messages-mode twin of `BUDGET_BASE`: each loop runs inside a `@message`.
const BUDGET_BASE_MESSAGES: &str = "@message system:\nBASE-SKELETON-HEAD\n@end\n\
     @block warmup:\n@message user:\n@for o in outer:\n@for i in inner:\n.\n@end\n@end\n@end\n@end\n\
     @block work:\n@message user:\nBASE-DEFAULT-WORK\n@end\n@end\n\
     @message system:\nBASE-SKELETON-TAIL\n@end\n";
const BUDGET_CHILD_MESSAGES: &str = "@extends \"./base.mds\"\n\
     @block work:\n@message assistant:\nCHILD-OVERRIDE-WORK\n\
     @for o in outer:\n@for i in inner:\n.\n@end\n@end\n@end\n@end\n";

/// Message-bytes base: the skeleton's `system` message is the base region; the
/// child overrides `turn`, whose base default has a different role (`assistant`).
const MESSAGE_BYTES_BASE: &str = "@message system:\n{{half}}\n@end\n\n\
     @block turn:\n@message assistant:\nBASE-DEFAULT-TURN\n@end\n@end\n";
const MESSAGE_BYTES_CHILD: &str =
    "@extends \"./base.mds\"\n@block turn:\n@message user:\n{{half}}{{tail}}\n@end\n@end\n";

/// Output-size base: the skeleton's first line is the base region; the child
/// overrides `body`.
const OUTPUT_SIZE_BASE: &str = "{{half}}\n@block body:\nBASE-DEFAULT-BODY\n@end\n";
const OUTPUT_SIZE_CHILD: &str = "@extends \"./base.mds\"\n@block body:\n{{half}}{{tail}}\n@end\n";

/// Message-count base: the skeleton's loop emits one `system` message per `base_items`
/// element; the child overrides `turn`, whose base default has a different role.
const MESSAGE_COUNT_BASE: &str = "@for i in base_items:\n@message system:\n.\n@end\n@end\n\
     @block turn:\n@message assistant:\nBASE-DEFAULT-TURN\n@end\n@end\n";
/// The child's `turn` override emits one `user` message per `child_items` element.
const MESSAGE_COUNT_CHILD: &str =
    "@extends \"./base.mds\"\n@block turn:\n@for i in child_items:\n@message user:\n.\n@end\n@end\n@end\n";

/// A `base.mds` + `child.mds` module set.
fn extends_chain(base: &str, child: &str) -> HashMap<String, String> {
    HashMap::from([
        ("base.mds".to_string(), base.to_string()),
        ("child.mds".to_string(), child.to_string()),
    ])
}

/// True only for output an honored `@extends` can produce: both base skeleton
/// markers and the child override are present, and the overridden default is not.
fn honors_extends(text: &str) -> bool {
    text.contains("BASE-SKELETON-HEAD")
        && text.contains("BASE-SKELETON-TAIL")
        && text.contains("CHILD-OVERRIDE")
        && !text.contains("BASE-DEFAULT")
}

/// Flatten either output kind to text so the marker checks read the same in every
/// mode: Markdown as-is, Messages as one `role: content` entry per message.
fn output_text(output: CompiledOutput) -> String {
    match output {
        CompiledOutput::Markdown(text) => text,
        CompiledOutput::Messages(messages) => messages
            .iter()
            .map(|m| format!("{}: {}\n", m.role, m.content))
            .collect(),
        other => panic!("unexpected output kind: {other:?}"),
    }
}

/// Which `honors_extends` markers `text` holds — a bounded failure message for
/// outputs that run to megabytes.
fn marker_report(text: &str) -> String {
    [
        "BASE-SKELETON-HEAD",
        "BASE-SKELETON-TAIL",
        "CHILD-OVERRIDE",
        "BASE-DEFAULT",
    ]
    .iter()
    .map(|marker| format!("{marker}={} ", text.contains(marker)))
    .collect()
}

/// Assert `result` failed with `mds::resource_limit` and a message containing
/// `expected`. An `Ok` is reported by kind only: `expect_err` would print the whole
/// compiled output, which here runs to tens of megabytes.
fn assert_resource_limit<T>(result: Result<T, MdsError>, context: &str, expected: &str) {
    let err = match result {
        Ok(_) => panic!("{context}: compiled successfully, expected a resource limit"),
        Err(err) => err,
    };
    assert!(
        matches!(err, MdsError::ResourceLimit { .. }),
        "{context}: expected mds::resource_limit, got: {err}"
    );
    assert!(
        err.to_string().contains(expected),
        "{context}: expected {expected:?}, got: {err}"
    );
}

/// Runtime vars for the size-cap chains: `half` fills each region, `tail` is appended
/// to the child's.
fn size_vars(half: &str, tail: &str) -> HashMap<String, Value> {
    HashMap::from([
        ("half".to_string(), Value::String(half.to_string())),
        ("tail".to_string(), Value::String(tail.to_string())),
    ])
}

/// Runtime vars for the iteration-budget chains.
fn loop_vars(inner_len: usize) -> HashMap<String, Value> {
    let numbers = |n: usize| Value::Array((0..n).map(|i| Value::Number(i as f64)).collect());
    HashMap::from([
        ("outer".to_string(), numbers(LOOP_OUTER)),
        ("inner".to_string(), numbers(inner_len)),
    ])
}

/// Compile `entry` from `modules` with runtime `vars`, flattening the output to text.
fn compile_chain(
    modules: &HashMap<String, String>,
    entry: &str,
    vars: HashMap<String, Value>,
    opts: CompileOptions,
) -> Result<String, MdsError> {
    mds::compile_virtual_with_deps_opts(modules.clone(), entry, Some(vars), opts)
        .map(|result| output_text(result.output))
}

/// Pin one cumulative iteration budget per extends chain on one evaluation path.
///
/// `compile` compiles the path's chain (two loop regions, see `BUDGET_BASE`) with
/// the given runtime vars. The at-budget control proves the chain honors
/// `@extends` and runs both loop regions to completion; the tripping case adds one
/// inner pass per outer pass, which no single region can exceed alone.
fn assert_iteration_budget_spans_extends_chain(
    path: &str,
    compile: impl Fn(HashMap<String, Value>) -> Result<String, MdsError>,
) {
    let text = compile(loop_vars(AT_BUDGET_INNER)).unwrap_or_else(|err| {
        panic!("{path}: a chain spending exactly MAX_TOTAL_ITERATIONS must compile: {err}")
    });
    assert!(
        honors_extends(&text),
        "{path}: output must come from an honored @extends; markers: {}",
        marker_report(&text)
    );
    assert_eq!(
        text.matches('.').count(),
        2 * LOOP_OUTER * AT_BUDGET_INNER,
        "{path}: the base-default and the child-override loop regions must both run to completion"
    );

    assert_resource_limit(
        compile(loop_vars(AT_BUDGET_INNER + 1)),
        &format!(
            "{path}: REL-1: one iteration budget per extends chain — two regions, each \
             under MAX_TOTAL_ITERATIONS alone, must trip it together"
        ),
        &format!(
            "total loop iterations exceeded maximum of {MAX_TOTAL_ITERATIONS} across all loops"
        ),
    );
}

/// RUST-4 / guards ADR-002: @extends compiled output must be byte-identical whether
/// `source_map` is `true` or `false`, and both must be the inherited output.
///
/// This test would catch output drift introduced by the REL-1 budget-threading fix
/// or by region-wise evaluation — if either path ever altered the output string the
/// assertions below would fail.
#[test]
fn extends_output_byte_identical_with_and_without_source_map() {
    let modules = extends_chain(PARITY_BASE, PARITY_CHILD);
    let with_map = vfs_opts(
        modules.clone(),
        "child.mds",
        CompileOptions::default().with_source_map(true),
    );
    let without_map = vfs_opts(modules, "child.mds", CompileOptions::default());

    // PF-013: prove @extends ran on each path before trusting the parity assertion.
    for (path, result) in [("maps on", &with_map), ("maps off", &without_map)] {
        assert_eq!(
            output_text(result.output.clone()),
            PARITY_EXPECTED,
            "{path}: @extends must splice the child override into the base skeleton"
        );
    }
    // ADR-002: byte-identical output regardless of source-map mode.
    assert_eq!(
        with_map.output, without_map.output,
        "ADR-002: @extends compiled output must be byte-identical with and without source maps"
    );
    assert!(
        with_map.source_map.is_some(),
        "source_map must be present when source_map=true"
    );
    assert!(
        without_map.source_map.is_none(),
        "source_map must be absent when source_map=false"
    );
}

/// Messages-mode twin of `extends_output_byte_identical_with_and_without_source_map`:
/// an @extends chain that compiles to messages yields the inherited messages whether
/// or not a source map was requested (messages mode builds no map and warns instead).
#[test]
fn extends_output_byte_identical_with_and_without_source_map_messages_mode() {
    let modules = extends_chain(PARITY_BASE_MESSAGES, PARITY_CHILD_MESSAGES);
    let with_map = vfs_opts(
        modules.clone(),
        "child.mds",
        CompileOptions::default().with_source_map(true),
    );
    let without_map = vfs_opts(modules, "child.mds", CompileOptions::default());

    for (path, result) in [("maps on", &with_map), ("maps off", &without_map)] {
        assert_eq!(
            output_text(result.output.clone()),
            "system: BASE-SKELETON-HEAD\nuser: CHILD-OVERRIDE-CONTENT\nsystem: BASE-SKELETON-TAIL\n",
            "{path}: @extends must splice the child's override message between the base's messages"
        );
        assert!(
            result.source_map.is_none(),
            "{path}: messages mode never produces a source map"
        );
    }
    assert_eq!(
        with_map.output, without_map.output,
        "messages-mode @extends output must be identical with and without source maps"
    );
    assert!(
        with_map
            .warnings
            .iter()
            .any(|w| w.contains("source maps are not supported for messages-mode templates")),
        "a requested source map must degrade with a warning; got: {:?}",
        with_map.warnings
    );
}

/// PF-013 control for `honors_extends`: the frontmatter key `extends:` is reserved
/// and does not trigger inheritance, so the child renders only its own block and
/// the marker check rejects the output. A test that declares its base this way
/// never exercises @extends.
#[test]
fn reserved_frontmatter_extends_key_does_not_inherit() {
    let modules = extends_chain(
        PARITY_BASE,
        "---\nextends: base.mds\n---\n@block content:\nCHILD-OVERRIDE-CONTENT\n@end\n",
    );
    let text = vfs_no_map(modules, "child.mds")
        .into_markdown()
        .expect("markdown output");

    assert!(
        text.contains("CHILD-OVERRIDE-CONTENT"),
        "the child's own block still renders; got: {text:?}"
    );
    assert!(
        !text.contains("BASE-SKELETON-HEAD"),
        "a reserved frontmatter key must not pull in the base skeleton; got: {text:?}"
    );
    assert!(
        !honors_extends(&text),
        "honors_extends must reject output compiled without inheritance; got: {text:?}"
    );
    assert!(
        honors_extends(PARITY_EXPECTED),
        "honors_extends must accept the inherited output"
    );
}

/// REL-1 regression / applies PF-004: the cumulative loop-iteration budget must be
/// shared across ALL @extends regions when `source_map: true`.
///
/// Each spliced region is evaluated separately; seeding a fresh budget per region
/// would give K regions an independent 1 M budget — CPU/DoS amplification ∝ region
/// count. The chain has two loop regions — a base-default block and a child
/// override — each well under the cap alone and exactly at it together; one more
/// inner pass per outer pass must trip `MAX_TOTAL_ITERATIONS`. The loop arrays are
/// injected via runtime_vars to avoid large YAML in the template.
#[test]
fn for_max_total_iterations_across_extends_regions_source_map() {
    let modules = extends_chain(BUDGET_BASE, BUDGET_CHILD);
    assert_iteration_budget_spans_extends_chain("maps on", |vars| {
        compile_chain(
            &modules,
            "child.mds",
            vars,
            CompileOptions::default().with_source_map(true),
        )
    });
}

/// REL-1 twin of `for_max_total_iterations_across_extends_regions_source_map` with
/// source maps disabled.
#[test]
fn for_max_total_iterations_across_extends_regions_maps_off() {
    let modules = extends_chain(BUDGET_BASE, BUDGET_CHILD);
    assert_iteration_budget_spans_extends_chain("maps off", |vars| {
        compile_chain(&modules, "child.mds", vars, CompileOptions::default())
    });
}

/// REL-1 twin of `for_max_total_iterations_across_extends_regions_source_map` for a
/// chain that compiles to messages: the loops run inside `@message` bodies spread
/// over a base-default block and a child override.
#[test]
fn for_max_total_iterations_across_extends_regions_messages_mode() {
    let modules = extends_chain(BUDGET_BASE_MESSAGES, BUDGET_CHILD_MESSAGES);
    assert_iteration_budget_spans_extends_chain("messages mode", |vars| {
        compile_chain(&modules, "child.mds", vars, CompileOptions::default())
    });
}

/// REL-1 twin of `for_max_total_iterations_across_extends_regions_source_map` for an
/// extending module reached through `@import` + `@include`: the imported chain's
/// regions share one budget too (the importer's own evaluation is separate).
#[test]
fn for_max_total_iterations_across_extends_regions_imported_module() {
    let mut modules = extends_chain(BUDGET_BASE, BUDGET_CHILD);
    modules.insert(
        "main.mds".to_string(),
        "@import \"./child.mds\" as child\n@include child\n".to_string(),
    );
    assert_iteration_budget_spans_extends_chain("imported module", |vars| {
        compile_chain(&modules, "main.mds", vars, CompileOptions::default())
    });
}

/// #115 / applies PF-004: the message-count cap (`MAX_MESSAGE_COUNT`) covers the whole
/// extends chain, not each region: every region appends to one message list.
///
/// The base skeleton's loop and the child's override loop each emit half the cap (the
/// child's one more in the tripping case), so only a count shared by both regions can
/// reject the sum.
#[test]
fn message_count_cumulative_across_regions() {
    let modules = extends_chain(MESSAGE_COUNT_BASE, MESSAGE_COUNT_CHILD);
    let items = |n: usize| Value::Array((0..n).map(|i| Value::Number(i as f64)).collect());
    let vars = |child_len: usize| {
        HashMap::from([
            ("base_items".to_string(), items(MAX_MESSAGE_COUNT / 2)),
            ("child_items".to_string(), items(child_len)),
        ])
    };

    // Control: the two regions emit exactly the cap — the largest admitted count.
    let messages = mds::compile_virtual_with_deps_opts(
        modules.clone(),
        "child.mds",
        Some(vars(MAX_MESSAGE_COUNT / 2)),
        CompileOptions::default(),
    )
    .expect("a chain emitting exactly MAX_MESSAGE_COUNT messages must compile")
    .into_messages()
    .expect("messages output");
    // PF-013: the base skeleton's messages come first, then the child override's (role
    // `user`, not the base default's `assistant`).
    let roles: Vec<&str> = messages.iter().map(|m| m.role.as_str()).collect();
    let mut expected = vec!["system"; MAX_MESSAGE_COUNT / 2];
    expected.extend(vec!["user"; MAX_MESSAGE_COUNT / 2]);
    assert!(
        roles == expected,
        "@extends must emit the base skeleton's messages then the child override's; got {} \
         messages",
        roles.len()
    );

    // One message over the cap in total; each region alone stays at about half of it.
    assert_resource_limit(
        mds::compile_virtual_with_deps_opts(
            modules,
            "child.mds",
            Some(vars(MAX_MESSAGE_COUNT / 2 + 1)),
            CompileOptions::default(),
        ),
        "one message count per extends chain — two regions, each under MAX_MESSAGE_COUNT \
         alone, must exceed it together",
        &format!("message count exceeded maximum of {MAX_MESSAGE_COUNT}"),
    );
}

/// AC-114-3 / applies PF-004: the cumulative message-content cap
/// (`MAX_MESSAGES_TOTAL_SIZE`) covers the whole extends chain, not each region.
///
/// The base skeleton's `system` message and the child's `user` override each carry
/// half the cap (the child's one byte more when `tail` is set). Neither message comes
/// near the cap alone, so only a budget shared by both regions can reject the sum.
/// Content is fed through runtime variables so the 10 MiB per-file cap and the
/// iteration budget cannot trip first.
#[test]
fn messages_total_bytes_cumulative_across_regions() {
    let modules = extends_chain(MESSAGE_BYTES_BASE, MESSAGE_BYTES_CHILD);
    let half = "a".repeat(MAX_MESSAGES_TOTAL_SIZE / 2);

    // Control: the two regions sum to exactly the cap — the largest admitted total.
    let messages = mds::compile_virtual_with_deps_opts(
        modules.clone(),
        "child.mds",
        Some(size_vars(&half, "")),
        CompileOptions::default(),
    )
    .expect("a chain whose messages total exactly MAX_MESSAGES_TOTAL_SIZE must compile")
    .into_messages()
    .expect("messages output");
    // PF-013: the base skeleton's message and the child's override (role `user`, not
    // the base default's `assistant`) are both present.
    let summary: Vec<(&str, usize)> = messages
        .iter()
        .map(|m| (m.role.as_str(), m.content.len()))
        .collect();
    assert_eq!(
        summary,
        [
            ("system", MAX_MESSAGES_TOTAL_SIZE / 2),
            ("user", MAX_MESSAGES_TOTAL_SIZE / 2)
        ],
        "@extends must emit the base skeleton message then the child override"
    );
    assert!(
        messages.iter().all(|m| m.content == half),
        "both messages must carry the injected content"
    );

    // One byte over the cap in total; each region alone stays at about half of it.
    for source_map in [false, true] {
        assert_resource_limit(
            mds::compile_virtual_with_deps_opts(
                modules.clone(),
                "child.mds",
                Some(size_vars(&half, "b")),
                CompileOptions::default().with_source_map(source_map),
            ),
            &format!(
                "source_map={source_map}: one message-byte budget per extends chain — two \
                 regions, each under MAX_MESSAGES_TOTAL_SIZE alone, must exceed it together"
            ),
            &format!(
                "total message content exceeds maximum cumulative size of \
                 {MAX_MESSAGES_TOTAL_SIZE} bytes"
            ),
        );
    }
}

/// Applies PF-004: the cumulative output cap (`MAX_OUTPUT_SIZE`) covers the whole
/// extends chain, not each region, on every Markdown path.
///
/// The base skeleton's first line and the child's `body` override each carry half
/// the cap (the child's one byte more when `tail` is set), so only an output guard
/// shared by both regions can reject the sum. Content is fed through runtime
/// variables so the 10 MiB per-file cap and the iteration budget cannot trip first.
///
/// The imported chain's over-cap case is only imported, never `@include`d: an
/// included prompt lands in the importer's own output, whose guard would reject it
/// even if the chain's did not (PF-013 — name the layer that rejects).
#[test]
fn output_size_cumulative_across_regions() {
    let mut modules = extends_chain(OUTPUT_SIZE_BASE, OUTPUT_SIZE_CHILD);
    modules.insert(
        "main.mds".to_string(),
        "@import \"./child.mds\" as child\n@include child\n".to_string(),
    );
    modules.insert(
        "import_only.mds".to_string(),
        "@import \"./child.mds\" as child\nIMPORTER\n".to_string(),
    );
    // (path, control entry, over-cap entry, source maps)
    let paths = [
        ("maps on", "child.mds", "child.mds", true),
        ("maps off", "child.mds", "child.mds", false),
        ("imported module", "main.mds", "import_only.mds", false),
    ];

    // Control: each region 16 bytes under half the cap leaves room for the few
    // newlines around them, so the chain compiles just under the cap.
    let under = "a".repeat(MAX_OUTPUT_SIZE / 2 - 16);
    let expected = format!("{under}\n{under}\n");
    for (path, entry, _, source_map) in paths {
        let text = compile_chain(
            &modules,
            entry,
            size_vars(&under, ""),
            CompileOptions::default().with_source_map(source_map),
        )
        .unwrap_or_else(|err| {
            panic!("{path}: a chain just under MAX_OUTPUT_SIZE must compile: {err}")
        });
        // PF-013: exactly the base region then the child override — the base default
        // is gone. Compared whole (a memcmp); lengths only on failure, never 50 MiB.
        assert!(
            text == expected,
            "{path}: expected the base region then the child override ({} bytes), got {} \
             bytes; {}",
            expected.len(),
            text.len(),
            marker_report(&text)
        );
    }

    // One byte over the cap in total; each region alone stays at about half of it.
    let half = "a".repeat(MAX_OUTPUT_SIZE / 2);
    for (path, _, entry, source_map) in paths {
        assert_resource_limit(
            compile_chain(
                &modules,
                entry,
                size_vars(&half, "b"),
                CompileOptions::default().with_source_map(source_map),
            ),
            &format!(
                "{path}: one output-size budget per extends chain — two regions, each \
                 under MAX_OUTPUT_SIZE alone, must exceed it together"
            ),
            &format!("output exceeds maximum size of {MAX_OUTPUT_SIZE} bytes"),
        );
    }
}

// ── #415: the output cap is checked before each append ─────────────────────────
//
// The error is the same whether the cap is checked before each append or after a
// whole node has finished, so the crossing-loop tests assert WHERE the loop stopped
// (applies PF-013). The public API does not expose the iteration count, so the
// iteration budget is the probe: a warm-up spends all but `CAP_CROSSING_PASS`
// iterations, so an evaluator that ran even one pass past the crossing one would
// report the iteration cap instead of the output cap.

/// The exact output-cap error, as `Display` renders it.
fn output_cap_error() -> String {
    format!("resource limit exceeded: output exceeds maximum size of {MAX_OUTPUT_SIZE} bytes")
}

/// The exact iteration-cap error, as `Display` renders it.
fn iteration_cap_error() -> String {
    format!(
        "resource limit exceeded: total loop iterations exceeded maximum of \
         {MAX_TOTAL_ITERATIONS} across all loops in this compilation"
    )
}

/// The error of `result`, reported by kind only on success (the output runs to tens
/// of megabytes).
fn err_of<T>(result: Result<T, MdsError>, context: &str) -> MdsError {
    match result {
        Ok(_) => panic!("{context}: compiled successfully, expected an error"),
        Err(err) => err,
    }
}

/// One crossing-loop pass renders `{{x}}\n`: `x` is 1 MiB, plus the newline.
const CAP_PASS: usize = 1024 * 1024 + 1;

/// The pass whose append would take the loop's buffer past `MAX_OUTPUT_SIZE`: 49
/// passes fit (51,380,273 bytes), the 50th does not (52,428,850).
const CAP_CROSSING_PASS: usize = MAX_OUTPUT_SIZE / CAP_PASS + 1;

/// Warm-up outer-loop length; with `WARMUP_INNER` it spends every iteration but
/// the last `CAP_CROSSING_PASS`.
const WARMUP_OUTER: usize = 50;
const WARMUP_INNER: usize = (MAX_TOTAL_ITERATIONS - CAP_CROSSING_PASS) / WARMUP_OUTER - 1;

const _: () = assert!((CAP_CROSSING_PASS - 1) * CAP_PASS <= MAX_OUTPUT_SIZE);
const _: () = assert!(CAP_CROSSING_PASS * CAP_PASS > MAX_OUTPUT_SIZE);
const _: () =
    assert!(WARMUP_OUTER * (WARMUP_INNER + 1) + CAP_CROSSING_PASS == MAX_TOTAL_ITERATIONS);
const _: () = assert!(WARMUP_INNER + 1 < 100_000);

/// Crossing-loop base: the warm-up runs in the skeleton, before the `work` block.
const CROSSING_BASE: &str = "BASE-SKELETON-HEAD\n\
     @for o in warmup_outer:\n@for i in warmup_inner:\n@end\n@end\n\
     @for i in warmup_extra:\n@end\n\
     @block work:\nBASE-DEFAULT-WORK\n@end\n\
     BASE-SKELETON-TAIL\n";
/// The child's `work` override holds the crossing loop.
const CROSSING_CHILD: &str = "@extends \"./base.mds\"\n\
     @block work:\nCHILD-OVERRIDE-WORK\n@for i in items:\n{{x}}\n@end\n@end\n";
/// A standalone module with the same warm-up and crossing loop, for `@include`. It
/// carries the chain's markers (and no `BASE-DEFAULT`), so the control's
/// `honors_extends` check proves its text reached the importer's output.
const CROSSING_MODULE: &str = "BASE-SKELETON-HEAD\n\
     @for o in warmup_outer:\n@for i in warmup_inner:\n@end\n@end\n\
     @for i in warmup_extra:\n@end\n\
     CHILD-OVERRIDE-WORK\n@for i in items:\n{{x}}\n@end\n\
     BASE-SKELETON-TAIL\n";

/// Runtime vars for the crossing-loop fixtures: `x` is the pass body, `items` the
/// crossing loop's array, and `extra` extra warm-up passes (0 or 1).
fn crossing_vars(x: &str, items: usize, extra: usize) -> HashMap<String, Value> {
    let numbers = |n: usize| Value::Array((0..n).map(|i| Value::Number(i as f64)).collect());
    HashMap::from([
        ("x".to_string(), Value::String(x.to_string())),
        ("items".to_string(), numbers(items)),
        ("warmup_outer".to_string(), numbers(WARMUP_OUTER)),
        ("warmup_inner".to_string(), numbers(WARMUP_INNER)),
        ("warmup_extra".to_string(), numbers(extra)),
    ])
}

/// Pin, on one path, that a loop crossing the output cap stops on the crossing pass.
///
/// `compile` compiles the path's fixture with the given runtime vars.
fn assert_loop_stops_at_the_crossing_pass(
    path: &str,
    compile: impl Fn(HashMap<String, Value>) -> Result<String, MdsError>,
) {
    // Control: warm-up plus `CAP_CROSSING_PASS` small passes spend the iteration
    // budget exactly, and the output comes from the fixture's container.
    let text = compile(crossing_vars("tick", CAP_CROSSING_PASS, 0))
        .unwrap_or_else(|err| panic!("{path}: the at-budget control must compile: {err}"));
    assert!(
        honors_extends(&text),
        "{path}: output must come from the crossing fixture; markers: {}",
        marker_report(&text)
    );
    assert_eq!(
        text.matches("tick\n").count(),
        CAP_CROSSING_PASS,
        "{path}: the control's loop must run every pass"
    );

    // The crossing pass is the last one the budget allows: only the output cap can
    // stop the loop there. One pass more would trip the iteration cap.
    let x = "a".repeat(CAP_PASS - 1);
    let err = err_of(
        compile(crossing_vars(&x, 2 * CAP_CROSSING_PASS, 0)),
        &format!("{path}: crossing loop"),
    );
    assert_eq!(
        err.to_string(),
        output_cap_error(),
        "{path}: the loop must stop on the pass that crosses the output cap"
    );

    // One warm-up pass more: the crossing pass is now one past the iteration budget,
    // and the iteration cap fires first, before that pass's body runs.
    let err = err_of(
        compile(crossing_vars(&x, 2 * CAP_CROSSING_PASS, 1)),
        &format!("{path}: crossing loop past the iteration budget"),
    );
    assert_eq!(
        err.to_string(),
        iteration_cap_error(),
        "{path}: the iteration cap must still win on the crossing pass"
    );
}

/// #415: a `@for` whose output is exactly `MAX_OUTPUT_SIZE` compiles; one
/// byte more fails with the exact output-cap error, with source maps on and off.
#[test]
fn for_loop_output_of_exactly_the_cap_compiles_one_byte_more_fails() {
    let modules = HashMap::from([(
        "main.mds".to_string(),
        "@for s in seps:\n{{x}}{{s}}\n@end\n".to_string(),
    )]);
    // Each pass renders `x`, then `s`, then a newline: 1 MiB when `s` is empty.
    let passes = MAX_OUTPUT_SIZE / (1024 * 1024);
    let x = "a".repeat(1024 * 1024 - 1);
    let vars = |last: &str| {
        let mut seps = vec![Value::String(String::new()); passes - 1];
        seps.push(Value::String(last.to_string()));
        HashMap::from([
            ("x".to_string(), Value::String(x.clone())),
            ("seps".to_string(), Value::Array(seps)),
        ])
    };
    for source_map in [false, true] {
        let opts = CompileOptions::default().with_source_map(source_map);
        let text =
            compile_chain(&modules, "main.mds", vars(""), opts.clone()).unwrap_or_else(|err| {
                panic!("source_map={source_map}: output of exactly the cap must compile: {err}")
            });
        assert_eq!(text.len(), MAX_OUTPUT_SIZE, "source_map={source_map}");
        assert!(
            text.bytes().filter(|&b| b == b'\n').count() == passes && !text.contains('b'),
            "source_map={source_map}: every pass must render `x` and a newline"
        );

        let err = err_of(
            compile_chain(&modules, "main.mds", vars("b"), opts),
            &format!("source_map={source_map}: one byte over the cap"),
        );
        assert!(
            matches!(err, MdsError::ResourceLimit { .. }),
            "source_map={source_map}: expected mds::resource_limit, got: {err}"
        );
        assert_eq!(
            err.to_string(),
            output_cap_error(),
            "source_map={source_map}"
        );
    }
}

/// #415: a crossing loop in a child's `@extends` override stops on the
/// crossing pass, source maps off; the warm-up in the base skeleton shares its budget.
#[test]
fn for_loop_output_cap_stops_at_the_crossing_pass_in_extends_region_maps_off() {
    let modules = extends_chain(CROSSING_BASE, CROSSING_CHILD);
    assert_loop_stops_at_the_crossing_pass("extends region, maps off", |vars| {
        compile_chain(&modules, "child.mds", vars, CompileOptions::default())
    });
}

/// #415: the twin of
/// `for_loop_output_cap_stops_at_the_crossing_pass_in_extends_region_maps_off` with
/// source maps on.
#[test]
fn for_loop_output_cap_stops_at_the_crossing_pass_in_extends_region_source_map() {
    let modules = extends_chain(CROSSING_BASE, CROSSING_CHILD);
    assert_loop_stops_at_the_crossing_pass("extends region, maps on", |vars| {
        compile_chain(
            &modules,
            "child.mds",
            vars,
            CompileOptions::default().with_source_map(true),
        )
    });
}

/// #415: a crossing loop in an `@include`d module stops on the crossing pass;
/// the module is evaluated with its own budget, which its warm-up spends.
#[test]
fn for_loop_output_cap_stops_at_the_crossing_pass_in_included_module() {
    let modules = HashMap::from([
        ("lib.mds".to_string(), CROSSING_MODULE.to_string()),
        (
            "main.mds".to_string(),
            "@import \"./lib.mds\" as lib\n@include lib\n".to_string(),
        ),
    ]);
    assert_loop_stops_at_the_crossing_pass("included module", |vars| {
        compile_chain(&modules, "main.mds", vars, CompileOptions::default())
    });
}

/// Evaluation-error fixtures, one per kind of spliced region: `n` is a string, so the
/// region's `@if n == 5:` (11 bytes, always at column 1) is a cross-type comparison.
/// Columns: region kind, base, child, the file the comparison is written in, and the
/// comparison's byte offset and line in that file.
const EXTENDS_EVAL_ERROR_CASES: [(&str, &str, &str, &str, usize, usize); 3] = [
    (
        "base skeleton",
        "---\nn: hi\n---\n@if n == 5:\nx\n@end\n@block body:\nBASE-DEFAULT\n@end\n",
        "@extends \"./base.mds\"\n@block body:\nCHILD-OVERRIDE\n@end\n",
        "base.mds",
        14,
        4,
    ),
    (
        "base-default block",
        "---\nn: hi\n---\n@block head:\n@if n == 5:\nx\n@end\n@end\n\
         @block body:\nBASE-DEFAULT\n@end\n",
        "@extends \"./base.mds\"\n@block body:\nCHILD-OVERRIDE\n@end\n",
        "base.mds",
        27,
        5,
    ),
    (
        "child override",
        "---\nn: hi\n---\n@block body:\nBASE-DEFAULT\n@end\n",
        "@extends \"./base.mds\"\n@block body:\n@if n == 5:\nx\n@end\n@end\n",
        "child.mds",
        35,
        3,
    ),
];

/// Assert `result` is the cross-type comparison of `EXTENDS_EVAL_ERROR_CASES`,
/// reported against `file` with a span on the `@if` line at `offset` / `line`, and
/// return the serialized error.
fn assert_eval_error_spans(
    result: Result<CompileResult, MdsError>,
    context: &str,
    file: &str,
    offset: usize,
    line: usize,
) -> SerializedError {
    let err = result.expect_err(&format!("{context}: a cross-type comparison must fail"));
    let serialized = err.serialize();
    assert_eq!(serialized.code, "mds::type_mismatch", "{context}: {err}");
    assert_eq!(
        err.source_name(),
        Some(file),
        "{context}: the error must name the file the comparison is written in"
    );
    assert_eq!(
        serialized.span,
        Some(
            SerializedSpan::new(offset, 11)
                .with_line(line)
                .with_column(1)
        ),
        "{context}: the span must underline the comparison's @if line"
    );
    serialized
}

/// AC-114-1 / AC-114-5: an evaluation error in any spliced region of an @extends chain
/// (base skeleton, base-default block, child override) is reported against the file the
/// region came from, and the serialized error is identical with source maps on and off.
#[test]
fn extends_eval_error_spans_its_own_file_with_and_without_source_map() {
    for (region, base, child, file, offset, line) in EXTENDS_EVAL_ERROR_CASES {
        let [off, on] = [false, true].map(|source_map| {
            assert_eval_error_spans(
                mds::compile_virtual_with_deps_opts(
                    extends_chain(base, child),
                    "child.mds",
                    None,
                    CompileOptions::default().with_source_map(source_map),
                ),
                &format!("{region}, source_map={source_map}"),
                file,
                offset,
                line,
            )
        });
        assert_eq!(
            off, on,
            "{region}: the serialized error must not depend on source maps"
        );
    }
}

/// One messages-mode error case (#115): an extends chain whose child overrides `turn`,
/// with the error placed in a different spliced region per case.
struct MessagesErrorCase {
    region: &'static str,
    base: &'static str,
    child: &'static str,
    code: &'static str,
    /// The file the offending node is written in.
    file: &'static str,
    /// Byte offset, length and line of the span in `file` (column is always 1).
    span: (usize, usize, usize),
}

/// A child that overrides `turn` with one message.
const MESSAGES_CHILD: &str =
    "@extends \"./base.mds\"\n@block turn:\n@message user:\ny\n@end\n@end\n";

const EXTENDS_MESSAGES_ERROR_CASES: [MessagesErrorCase; 5] = [
    MessagesErrorCase {
        region: "base skeleton stray text",
        base: "@message system:\nhi\n@end\nSTRAY TEXT\n@block turn:\n@message user:\nx\n@end\n@end\n",
        child: MESSAGES_CHILD,
        code: "mds::mixed_content",
        file: "base.mds",
        span: (25, 10, 4),
    },
    MessagesErrorCase {
        // The frontmatter pushes the stray text past the end of the child source.
        region: "base skeleton stray text past the child's length",
        base: "---\npadding: ppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppp\
               ppppppppppppppppppp\n---\n\
               @message system:\nhi\n@end\nSTRAY TEXT\n@block turn:\n@message user:\nx\n@end\n@end\n",
        child: MESSAGES_CHILD,
        code: "mds::mixed_content",
        file: "base.mds",
        span: (123, 10, 7),
    },
    MessagesErrorCase {
        region: "base-default block stray text",
        base: "@message system:\nhi\n@end\n@block head:\nSTRAY TEXT\n@end\n\
               @block turn:\n@message user:\nx\n@end\n@end\n",
        child: MESSAGES_CHILD,
        code: "mds::mixed_content",
        file: "base.mds",
        span: (38, 10, 5),
    },
    MessagesErrorCase {
        region: "child override stray text",
        base: "@message system:\nhi\n@end\n@block turn:\n@message user:\nx\n@end\n@end\n",
        child: "@extends \"./base.mds\"\n@block turn:\nCHILD STRAY\n@message user:\ny\n@end\n@end\n",
        code: "mds::mixed_content",
        file: "child.mds",
        span: (35, 11, 3),
    },
    MessagesErrorCase {
        region: "base skeleton type mismatch",
        base: "---\nn: hi\n---\n@if n == 5:\n@message system:\nx\n@end\n@end\n\
               @block turn:\n@message user:\nx\n@end\n@end\n",
        child: MESSAGES_CHILD,
        code: "mds::type_mismatch",
        file: "base.mds",
        span: (14, 11, 4),
    },
];

/// #115 / AC-114-1: in messages mode, an error in any spliced region of an @extends
/// chain — orphan text outside a `@message` (`mds::mixed_content`) or a cross-type
/// comparison — is reported against the file the region came from, whether or not a
/// source map was requested (messages mode builds none).
#[test]
fn extends_messages_error_spans_its_own_file() {
    for case in EXTENDS_MESSAGES_ERROR_CASES {
        let (offset, length, line) = case.span;
        let [off, on] = [false, true].map(|source_map| {
            let context = format!("{}, source_map={source_map}", case.region);
            let err = mds::compile_virtual_with_deps_opts(
                extends_chain(case.base, case.child),
                "child.mds",
                None,
                CompileOptions::default().with_source_map(source_map),
            )
            .expect_err(&format!("{context}: the chain must fail"));
            let serialized = err.serialize();
            assert_eq!(serialized.code, case.code, "{context}: {err}");
            assert_eq!(
                err.source_name(),
                Some(case.file),
                "{context}: the error must name the file the offending node is written in"
            );
            assert_eq!(
                serialized.span,
                Some(
                    SerializedSpan::new(offset, length)
                        .with_line(line)
                        .with_column(1)
                ),
                "{context}: the span must underline the offending node"
            );
            serialized
        });
        assert_eq!(
            off, on,
            "{}: messages-mode errors ignore source maps",
            case.region
        );
    }
}

/// #114: an extending module reached through `@import` is evaluated region by region
/// too, so an evaluation error inside it is reported against the file each region came
/// from — not dropped to spanless — whether or not the importer asked for a source map.
#[test]
fn imported_extending_module_eval_error_spans_its_own_file() {
    for (region, base, child, file, offset, line) in EXTENDS_EVAL_ERROR_CASES {
        let mut modules = extends_chain(base, child);
        modules.insert(
            "main.mds".to_string(),
            "@import \"./child.mds\" as child\n@include child\n".to_string(),
        );
        for source_map in [false, true] {
            assert_eval_error_spans(
                mds::compile_virtual_with_deps_opts(
                    modules.clone(),
                    "main.mds",
                    None,
                    CompileOptions::default().with_source_map(source_map),
                ),
                &format!("imported {region}, source_map={source_map}"),
                file,
                offset,
                line,
            );
        }
    }
}

// ── D1: STRING_SOURCE_MAP_LABEL cross-surface parity (PF-007) ────────────────
//
// These tests verify that the choke-point fix in MapBuilder::new and
// source_index ensures the "<source>" diagnostic sentinel never appears in
// sources[] for any code path.

/// D1-CORE-1: string-source compile with sourceMap → sources[0] == "input.mds".
///
/// Verifies the MapBuilder::new choke-point: map_source_label("<source>") →
/// STRING_SOURCE_MAP_LABEL so the default string-source label matches WASM.
#[test]
fn d1_string_source_sources_label_is_input_mds() {
    let result = mds::compile_str_with_deps_opts(
        "Hello World!\n",
        None,
        None,
        CompileOptions::default().with_source_map(true),
    )
    .expect("should compile");
    let sm = result.source_map.expect("source_map must be present");
    assert_eq!(
        sm.sources,
        vec!["input.mds"],
        "string-source sources[0] must be \"input.mds\" after map_source_label fix; got: {:?}",
        sm.sources
    );
}

/// D1-CORE-2: S8 function-body attribution — locally-defined function in a
/// string-source template must not add a second "<source>" entry to sources[].
///
/// Without the source_index choke-point fix:
///   - MapBuilder::new("<source>") was stored as "<source>" at index 0.
///   - S8 path called source_index("<source>", ...) → found it → OK.
///   - After the MapBuilder::new fix alone:
///     - MapBuilder::new("<source>") → stores "input.mds" at index 0.
///     - S8 path called source_index("<source>", ...) → NOT found → added
///       as a NEW entry "input.mds"... but with old code it would have been
///       "<source>" at index 1.
///   - With both choke-points fixed: source_index("<source>") →
///     map_source_label → "input.mds" → found at index 0 → no new entry.
#[test]
fn d1_s8_locally_defined_function_no_source_sentinel() {
    // Define a function in the entry (string-source) template and call it.
    // In S8 path: source_index(func.origin.file) is called with "<source>".
    // After the fix both MapBuilder::new and source_index canonicalize it to
    // "input.mds", so sources must be exactly ["input.mds"] — no duplicates.
    let result = mds::compile_str_with_deps_opts(
        "@define greet():\nHello!\n@end\n{{greet()}}\n",
        None,
        None,
        CompileOptions::default().with_source_map(true),
    )
    .expect("should compile");
    let sm = result.source_map.expect("source_map must be present");
    assert_eq!(
        sm.sources,
        vec!["input.mds"],
        "S8 path must not add a second \"<source>\" entry; got: {:?}",
        sm.sources
    );
}

/// D1-CORE-3: @extends child that is a string-source → no "<source>" sentinel
/// in sources[].
///
/// The child's origin (file="<source>") flows into override_origin for any
/// blocks it overrides. When evaluate_with_map_seeded processes spliced regions
/// it calls source_index(origin.file, ...) for each region. After the fix,
/// origin.file="<source>" → map_source_label → "input.mds" at index 0 (the
/// same entry already seeded by MapBuilder::new with skeleton_origin.file).
#[test]
fn d1_extends_from_string_no_source_sentinel() {
    let dir = tempfile::tempdir().unwrap();
    // Write the base template to disk so @extends can resolve it.
    std::fs::write(
        dir.path().join("base.mds"),
        "@block content:\ndefault content\n@end\n",
    )
    .unwrap();

    let child = "@extends \"./base.mds\"\n@block content:\noverridden\n@end\n";
    let result = mds::compile_str_with_deps_opts(
        child,
        Some(dir.path()),
        None,
        CompileOptions::default().with_source_map(true),
    )
    .expect("should compile");
    let sm = result.source_map.expect("source_map must be present");
    // "<source>" must not appear anywhere in sources[].
    for src in &sm.sources {
        assert_ne!(
            src.as_str(),
            "<source>",
            "\"<source>\" must not appear in sources[] after map_source_label fix; got: {:?}",
            sm.sources
        );
    }
    // The child's blocks should be attributed to "input.mds" (not "<source>").
    assert!(
        sm.sources.contains(&"input.mds".to_string()),
        "child source must be labeled \"input.mds\"; got: {:?}",
        sm.sources
    );
}

// ── #416: source-mapped @extends evaluation is linear in the region count ─────

/// Skeleton lines in the many-regions base. Each `{{v}}\n` line is two spliced
/// regions (an interpolation node and a text node), all owned by the base.
const MANY_REGIONS_LINES: usize = 262_144;

/// The value each many-regions line renders to — a whole output line nothing else
/// in the chain can produce.
const MANY_REGIONS_VALUE: &str = "V416";

/// Absolute smoke bound on one compile of the many-regions chain. Calibrated against
/// the builder before #416, which copied the base's whole source once per region: that
/// build runs past five times this bound, while the shared-source builder finishes well
/// inside a fifth of it (debug profile).
const MANY_REGIONS_BOUND: Duration = Duration::from_secs(10);

/// #416: a source-mapped compile of a base with very many top-level skeleton
/// nodes finishes under an absolute bound; the same chain with maps off is the control.
///
/// The compile runs on its own thread and is awaited with `recv_timeout`, so a
/// quadratic regression fails at the bound instead of hanging the suite.
#[test]
fn extends_many_regions_source_map_is_not_quadratic() {
    let base = format!(
        "---\nv: {MANY_REGIONS_VALUE}\n---\n{}",
        "{{v}}\n".repeat(MANY_REGIONS_LINES)
    );
    for source_map in [false, true] {
        let modules = extends_chain(&base, "@extends \"./base.mds\"\n");
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let started = Instant::now();
            let outcome = mds::compile_virtual_with_deps_opts(
                modules,
                "child.mds",
                None,
                CompileOptions::default().with_source_map(source_map),
            )
            .map(|result| {
                let lines = output_text(result.output)
                    .lines()
                    .filter(|line| *line == MANY_REGIONS_VALUE)
                    .count();
                (lines, result.source_map.map(|sm| sm.sources))
            })
            .map_err(|err| err.to_string());
            // The receiver is gone only when the bound already failed the test.
            let _ = tx.send((outcome, started.elapsed()));
        });
        let (outcome, elapsed) = match rx.recv_timeout(MANY_REGIONS_BOUND) {
            Ok(received) => received,
            Err(RecvTimeoutError::Timeout) => panic!(
                "source_map={source_map}: {MANY_REGIONS_LINES} skeleton lines did not \
                 compile within {MANY_REGIONS_BOUND:?}"
            ),
            Err(RecvTimeoutError::Disconnected) => {
                panic!("source_map={source_map}: the compile thread panicked")
            }
        };
        eprintln!("source_map={source_map}: {MANY_REGIONS_LINES} skeleton lines in {elapsed:?}");
        let (lines, sources) =
            outcome.unwrap_or_else(|err| panic!("source_map={source_map}: {err}"));
        // Non-vacuity: every line came from the base through `@extends` — the child's
        // whole body is the directive, so nothing else can render the value.
        assert_eq!(
            lines, MANY_REGIONS_LINES,
            "source_map={source_map}: every base line must be rendered"
        );
        let expected_sources = source_map.then(|| vec!["base.mds".to_string()]);
        assert_eq!(
            sources, expected_sources,
            "source_map={source_map}: only the base owns a region"
        );
    }
}

// ── #412: an @include of an extending module is source-mapped ─────────────────

/// Root of the #412 three-level chain: skeleton text around three blocks.
const CHAIN_A: &str = "A-HEAD\n@block b1:\nA-ONE {{v}}\n@end\n\
     @block b2:\nA-TWO\n@end\n@block b3:\nA-THREE\n@end\nA-TAIL\n";
/// The intermediate file overrides `b2`.
const CHAIN_B: &str = "@extends \"./a.mds\"\n@block b2:\nB-OVERRIDE {{v}}\n@end\n";
/// The leaf overrides `b3`. Its override text starts at the same byte offset as the
/// intermediate's, so a segment attributed to the wrong one of the two still lands
/// on text: attribution must be checked by file, not by bounds.
const CHAIN_C: &str = "@extends \"./b.mds\"\n@block b3:\nC-OVERRIDE {{v}}\n@end\n";
/// The importer: one line of its own on each side of the included chain.
const CHAIN_MAIN: &str = "@import \"./c.mds\" as c\nMAIN-HEAD\n@include c\nMAIN-TAIL\n";

/// Mirrors the private `limits::MAX_SOURCEMAP_SEGMENTS`. Pinned by the segment-cap
/// warning, which prints the real value.
const MAX_SOURCEMAP_SEGMENTS: usize = 1_000_000;

/// Mirrors the private `limits::MAX_SOURCES_CONTENT_BYTES` (= `MAX_OUTPUT_SIZE`).
/// Pinned by the ceiling warning, which prints the real value.
const MAX_SOURCES_CONTENT_BYTES: usize = MAX_OUTPUT_SIZE;

/// The #412 chain and its importer as a module set.
fn chain_modules() -> HashMap<String, String> {
    HashMap::from([
        ("a.mds".to_string(), CHAIN_A.to_string()),
        ("b.mds".to_string(), CHAIN_B.to_string()),
        ("c.mds".to_string(), CHAIN_C.to_string()),
        ("main.mds".to_string(), CHAIN_MAIN.to_string()),
    ])
}

/// Compile `entry` of the #412 chain, with `v` = `V`.
fn compile_chain_entry(entry: &str, opts: CompileOptions) -> CompileResult {
    mds::compile_virtual_with_deps_opts(
        chain_modules(),
        entry,
        Some(HashMap::from([(
            "v".to_string(),
            Value::String("V".to_string()),
        )])),
        opts,
    )
    .unwrap_or_else(|err| panic!("{entry} must compile: {err}"))
}

/// One mapped point: generated line and column, source file, source line and column.
type MappedPoint = (usize, i64, String, i64, i64);

/// Every mapped point of `sm`, in mappings order. Source indices are resolved to
/// names, so two maps that list their sources in different orders compare equal.
fn mapped_points(sm: &mds::SourceMap) -> Vec<MappedPoint> {
    let mut points = Vec::new();
    let (mut src, mut src_line, mut src_col) = (0i64, 0i64, 0i64);
    for (line, line_str) in sm.mappings.split(';').enumerate() {
        let mut col = 0i64;
        for seg_str in line_str.split(',').filter(|s| !s.is_empty()) {
            let (d_col, rest) = vlq_decode_one(seg_str.as_bytes());
            col += d_col;
            if rest.is_empty() {
                continue; // 1-field (unmapped) segment
            }
            let (d_src, rest) = vlq_decode_one(rest);
            let (d_line, rest) = vlq_decode_one(rest);
            let (d_src_col, _) = vlq_decode_one(rest);
            src += d_src;
            src_line += d_line;
            src_col += d_src_col;
            points.push((
                line,
                col,
                sm.sources[src as usize].clone(),
                src_line,
                src_col,
            ));
        }
    }
    points
}

/// The names of the sources `sm`'s mappings reference.
fn referenced_sources(sm: &mds::SourceMap) -> std::collections::BTreeSet<String> {
    referenced_src_indices(&sm.mappings)
        .into_iter()
        .map(|idx| sm.sources[idx as usize].clone())
        .collect()
}

/// A set of owned names.
fn name_set<const N: usize>(names: [&str; N]) -> std::collections::BTreeSet<String> {
    names.into_iter().map(String::from).collect()
}

/// #412 / AC-4 (TP-5): an `@include` of a three-level extending chain maps every
/// included byte to the chain file that wrote it — root, intermediate or leaf — and
/// the included lines map exactly as a direct compile of the leaf maps them, one line
/// down.
#[test]
fn include_of_extending_chain_attributes_each_byte() {
    // Colliding offsets: only a per-file comparison tells the two overrides apart.
    assert_eq!(CHAIN_B.find("B-OVERRIDE"), CHAIN_C.find("C-OVERRIDE"));
    let opts = CompileOptions::default().with_source_map(true);
    let direct = compile_chain_entry("c.mds", opts.clone());
    let imported = compile_chain_entry("main.mds", opts);
    let direct_text = output_text(direct.output);
    let imported_text = output_text(imported.output);
    // Non-vacuity: the direct compile honors the whole chain, and the importer renders
    // it whole between its own two lines.
    for marker in [
        "A-HEAD",
        "A-ONE V",
        "B-OVERRIDE V",
        "C-OVERRIDE V",
        "A-TAIL",
    ] {
        assert!(
            direct_text.contains(marker),
            "{marker} missing: {direct_text:?}"
        );
    }
    assert!(
        !direct_text.contains("A-TWO") && !direct_text.contains("A-THREE"),
        "overridden defaults must not render: {direct_text:?}"
    );
    assert_eq!(
        imported_text,
        format!("MAIN-HEAD\n{direct_text}MAIN-TAIL\n")
    );

    let direct_sm = direct
        .source_map
        .expect("the direct compile must produce a map");
    let imported_sm = imported
        .source_map
        .expect("the importer must produce a map");
    assert_eq!(
        referenced_sources(&imported_sm),
        name_set(["a.mds", "b.mds", "c.mds", "main.mds"]),
        "every file of the included chain must own mapped output; mappings={:?}",
        imported_sm.mappings
    );

    let chain_lines = direct_text.lines().count();
    let direct_points = mapped_points(&direct_sm);
    let (spliced, own): (Vec<_>, Vec<_>) = mapped_points(&imported_sm)
        .into_iter()
        .partition(|point| (1..=chain_lines).contains(&point.0));
    let spliced: Vec<MappedPoint> = spliced
        .into_iter()
        .map(|(line, col, src, src_line, src_col)| (line - 1, col, src, src_line, src_col))
        .collect();
    assert!(
        !direct_points.is_empty(),
        "non-vacuity: the chain is mapped"
    );
    assert_eq!(
        spliced, direct_points,
        "the included lines must map as a direct compile of the chain maps them"
    );
    assert!(
        !own.is_empty() && own.iter().all(|point| point.2 == "main.mds"),
        "the importer's own lines map to the importer: {own:?}"
    );
}

/// #412 / AC-5 (TP-6) — a guard, no RED claim: output never depended on the map. An
/// importer's output is byte-identical with source maps on and off when it includes an
/// extending chain, and holds the text of all three chain files.
#[test]
fn include_of_extending_chain_output_is_identical_with_and_without_source_map() {
    let [off, on] = [false, true].map(|source_map| {
        output_text(
            compile_chain_entry(
                "main.mds",
                CompileOptions::default().with_source_map(source_map),
            )
            .output,
        )
    });
    for marker in ["A-HEAD", "B-OVERRIDE V", "C-OVERRIDE V", "MAIN-TAIL"] {
        assert!(on.contains(marker), "{marker} missing: {on:?}");
    }
    assert!(
        !on.contains("A-TWO") && !on.contains("A-THREE"),
        "overridden defaults must not render: {on:?}"
    );
    assert_eq!(
        on, off,
        "output must be byte-identical with and without source maps"
    );
}

/// The WARN-A text an `@include` of `alias` gives when the module has no body text.
fn empty_include_warning(alias: &str) -> String {
    format!(
        "warning: @include of '{alias}' produced empty output — module has no body text; \
         to use the module's functions, call them directly: {{{{{alias}.fn()}}}}"
    )
}

/// #412 / AC-6 (TP-7) — a guard, no RED claim: 633e518 kept no map for any extending
/// module. An included module whose `prompt` adds no text contributes nothing to the
/// importer's map, is still evaluated, and warns exactly as with source maps off. That
/// the module keeps no map at all is not visible here — an importer drops a
/// not-exported map and splices nothing for an empty body — and is pinned on the module
/// by `resolver::tests::prompt_map_is_kept_only_for_an_exported_prompt_that_renders_text`.
///
/// Rows: an extending module whose body is empty — its override `@include`s an empty
/// module, a warning only evaluation can raise — and a standalone module that does not
/// export `prompt` (WARN-B needs its body evaluated). The not-exported form of the
/// extending row is unreachable: an extending template may contain only `@block`
/// overrides, so it cannot `@export` and always exports `prompt`. Each row's control
/// makes the module's `prompt` add text, and its map is then spliced.
#[test]
fn include_that_adds_no_text_keeps_no_map_and_warns_the_same() {
    let rows = [
        (
            "extending module, empty body",
            vec![
                ("empty.mds", ""),
                ("base.mds", "@block body:\nBASE-DEFAULT\n@end\n"),
                (
                    "child.mds",
                    "---\nimports:\n  - path: ./empty.mds\n    as: e\n---\n\
                     @extends \"./base.mds\"\n@block body:\n@include e\n@end\n",
                ),
            ],
            vec![empty_include_warning("e"), empty_include_warning("m")],
            ("empty.mds", "E-TEXT\n"),
        ),
        (
            "standalone module, prompt not exported",
            vec![("child.mds", "@define f():\nF\n@end\n@export f\nLIB-BODY\n")],
            vec![
                "warning: @include of 'm' produced empty output — module has body text, but \
                 its explicit @export list does not export 'prompt'; add `@export prompt` to \
                 the module, or call its functions directly: {{m.fn()}}"
                    .to_string(),
            ],
            (
                "child.mds",
                "@define f():\nF\n@end\n@export f\n@export prompt\nLIB-BODY\n",
            ),
        ),
    ];
    let compile = |modules: &HashMap<String, String>, source_map: bool, row: &str| {
        mds::compile_virtual_with_deps_opts(
            modules.clone(),
            "main.mds",
            None,
            CompileOptions::default().with_source_map(source_map),
        )
        .unwrap_or_else(|err| panic!("{row}, source_map={source_map}: {err}"))
    };
    for (row, files, expected_warnings, (control_key, control_text)) in rows {
        let mut modules: HashMap<String, String> = files
            .into_iter()
            .map(|(key, text)| (key.to_string(), text.to_string()))
            .collect();
        modules.insert(
            "main.mds".to_string(),
            "@import \"./child.mds\" as m\nMAIN\n@include m\n".to_string(),
        );
        let [off, on] = [false, true].map(|source_map| compile(&modules, source_map, row));
        assert_eq!(on.warnings, expected_warnings, "{row}: maps on");
        assert_eq!(
            off.warnings, on.warnings,
            "{row}: warnings must not depend on maps"
        );
        assert_eq!(output_text(on.output.clone()), "MAIN\n", "{row}");
        assert_eq!(on.output, off.output, "{row}");
        let sm = on.source_map.expect("the importer's own map is produced");
        assert_eq!(sm.sources, vec!["main.mds"], "{row}: nothing is spliced");
        assert_eq!(
            referenced_src_indices(&sm.mappings),
            std::collections::HashSet::from([0]),
            "{row}: the importer's own text stays mapped"
        );

        // Control (PF-013): once the module's `prompt` adds text, its map is spliced, so
        // the importer's map lists it and the check above can fail.
        modules.insert(control_key.to_string(), control_text.to_string());
        let control = compile(&modules, true, row);
        assert!(control.warnings.is_empty(), "{row}: {:?}", control.warnings);
        let sm = control.source_map.expect("maps on");
        assert!(
            sm.sources.iter().any(|source| source == "child.mds"),
            "{row}: control: the module's map is spliced; sources={:?}",
            sm.sources
        );
    }
}

/// Segment-cap module body: 6 text and 5 interpolation segments per pass, so
/// `SEGMENT_CAP_PASSES` passes record 1,100,000 segments, past the cap.
const SEGMENT_CAP_LOOP: &str =
    "@for item in items:\nA{{item}}B{{item}}C{{item}}D{{item}}E{{item}}F\n@end\n";
const SEGMENT_CAP_PASSES: usize = 100_000;
const _: () = assert!(11 * SEGMENT_CAP_PASSES > MAX_SOURCEMAP_SEGMENTS);

/// One segment-cap case: its label, the included module's key, and the module set
/// (without the importer) that defines it.
type SegmentCapRow = (&'static str, &'static str, Vec<(&'static str, String)>);

/// #412 / AC-7 (TP-8): an included module whose own map passes the segment cap —
/// standalone or extending — gives a warning saying exactly what happens: the text it
/// contributes is left unmapped and the importer's map is still produced. The same
/// modules under the cap are mapped (control).
#[test]
fn segment_cap_in_an_included_module_leaves_its_text_unmapped() {
    let rows: [SegmentCapRow; 2] = [
        (
            "standalone",
            "lib.mds",
            vec![("lib.mds", SEGMENT_CAP_LOOP.to_string())],
        ),
        (
            "extending",
            "child.mds",
            vec![
                ("base.mds", "@block body:\nBASE-DEFAULT\n@end\n".to_string()),
                (
                    "child.mds",
                    format!("@extends \"./base.mds\"\n@block body:\n{SEGMENT_CAP_LOOP}@end\n"),
                ),
            ],
        ),
    ];
    let compile = |module: &str, files: &[(&str, String)], passes: usize| {
        let mut modules: HashMap<String, String> = files
            .iter()
            .map(|(key, text)| (key.to_string(), text.clone()))
            .collect();
        modules.insert(
            "main.mds".to_string(),
            format!("@import \"./{module}\" as m\nMAIN\n@include m\n"),
        );
        let items = Value::Array(vec![Value::String("x".to_string()); passes]);
        mds::compile_virtual_with_deps_opts(
            modules,
            "main.mds",
            Some(HashMap::from([("items".to_string(), items)])),
            CompileOptions::default().with_source_map(true),
        )
        .unwrap_or_else(|err| panic!("{module}, {passes} passes: {err}"))
    };

    let mut observed = Vec::new();
    let mut expected = Vec::new();
    let mut maps = Vec::new();
    for (row, module, files) in &rows {
        let result = compile(module, files, SEGMENT_CAP_PASSES);
        // Non-vacuity: every pass of the included loop reached the importer's output.
        let text = output_text(result.output);
        assert_eq!(
            text.matches("AxBxCxDxExF\n").count(),
            SEGMENT_CAP_PASSES,
            "{row}"
        );
        observed.push((*row, result.warnings));
        expected.push((
            *row,
            vec![format!(
                "source map segment cap ({MAX_SOURCEMAP_SEGMENTS} segments) exceeded in \
                 imported module '{module}'; text included from it is left unmapped in the \
                 source map"
            )],
        ));
        maps.push((*row, result.source_map));
    }
    assert_eq!(observed, expected, "the warning must say what happens");
    for (row, map) in maps {
        let sm = map.unwrap_or_else(|| panic!("{row}: the importer's map is still produced"));
        assert_eq!(
            sm.sources,
            vec!["main.mds"],
            "{row}: the module's files are not listed"
        );
        // Line 0 is the importer's own `MAIN`; every later line is included text.
        let mapped_lines: std::collections::BTreeSet<usize> = mapped_points(&sm)
            .into_iter()
            .map(|point| point.0)
            .collect();
        assert_eq!(
            mapped_lines,
            std::collections::BTreeSet::from([0]),
            "{row}: the importer's own text stays mapped and the included text is unmapped"
        );
    }

    // Control (PF-013): under the cap the same modules are mapped and nothing warns.
    for (row, module, files) in &rows {
        let result = compile(module, files, 10);
        assert!(result.warnings.is_empty(), "{row}: {:?}", result.warnings);
        let sm = result.source_map.expect("maps on");
        assert!(
            referenced_sources(&sm).contains(*module),
            "{row}: under the cap the module is mapped; sources={:?}",
            sm.sources
        );
    }
}

/// Levels of the sourcesContent-ceiling chain, and the unrendered padding each level
/// carries: six levels pass the ceiling, five would not.
const CEILING_LEVELS: usize = 6;
const CEILING_PADDING: usize = 9 * 1024 * 1024;
const _: () = assert!(CEILING_LEVELS * CEILING_PADDING > MAX_SOURCES_CONTENT_BYTES);
const _: () = assert!((CEILING_LEVELS - 1) * CEILING_PADDING < MAX_SOURCES_CONTENT_BYTES);

/// A `CEILING_LEVELS`-level chain `l1.mds` ← … ← `l6.mds` and an importer that
/// includes its leaf. Level `k` renders one block (`bk`) and carries `padding` bytes it
/// never renders: the root in an unused `@define`, each extending level as blank space
/// between its directives (an extending template may hold only `@block` overrides and
/// whitespace).
fn ceiling_chain(padding: usize) -> HashMap<String, String> {
    let pad = " ".repeat(padding);
    let blocks: String = (1..=CEILING_LEVELS)
        .map(|k| format!("@block b{k}:\nL1-B{k}\n@end\n"))
        .collect();
    let mut modules = HashMap::from([
        (
            "l1.mds".to_string(),
            format!("@define unused():\n{pad}\n@end\n{blocks}"),
        ),
        (
            "main.mds".to_string(),
            format!("@import \"./l{CEILING_LEVELS}.mds\" as m\nMAIN\n@include m\n"),
        ),
    ]);
    for k in 2..=CEILING_LEVELS {
        modules.insert(
            format!("l{k}.mds"),
            format!(
                "@extends \"./l{}.mds\"\n{pad}\n@block b{k}:\nL{k}-B{k}\n@end\n",
                k - 1
            ),
        );
    }
    modules
}

/// #412 / AC-8 (TP-9): the importer's map now carries the included chain's files, so
/// the sourcesContent ceiling covers them — the first test of that ceiling. A chain
/// whose files pass it drops `sourcesContent` from the importer's map with the ceiling
/// warning and keeps every source and mapping; a small copy of the chain keeps
/// `sourcesContent` (control).
#[test]
fn include_of_extending_chain_over_the_sources_content_ceiling_drops_sources_content() {
    let all_files: std::collections::BTreeSet<String> = (1..=CEILING_LEVELS)
        .map(|k| format!("l{k}.mds"))
        .chain(["main.mds".to_string()])
        .collect();
    let opts = CompileOptions::default()
        .with_source_map(true)
        .with_include_sources_content(true);
    for (padding, over) in [(CEILING_PADDING, true), (16, false)] {
        let modules = ceiling_chain(padding);
        let total: usize = modules.values().map(String::len).sum();
        let result = mds::compile_virtual_with_deps_opts(modules, "main.mds", None, opts.clone())
            .unwrap_or_else(|err| panic!("padding={padding}: {err}"));
        let expected_warnings = if over {
            vec![format!(
                "sourcesContent total size ({total} bytes) exceeds the \
                 {MAX_SOURCES_CONTENT_BYTES} byte ceiling; sourcesContent will be omitted \
                 from the source map"
            )]
        } else {
            vec![]
        };
        assert_eq!(result.warnings, expected_warnings, "padding={padding}");
        let text = output_text(result.output);
        let sm = result.source_map.expect("the importer's map is produced");
        assert_eq!(
            sm.sources_content.as_ref().map(Vec::len),
            (!over).then_some(all_files.len()),
            "padding={padding}: sourcesContent is kept only under the ceiling"
        );
        assert_eq!(
            referenced_sources(&sm),
            all_files,
            "padding={padding}: every chain file must own mapped output"
        );
        let rendered: Vec<String> = (1..=CEILING_LEVELS).map(|k| format!("L{k}-B{k}")).collect();
        assert_eq!(
            text,
            format!("MAIN\n{}\n", rendered.join("\n")),
            "padding={padding}: each level renders its block and none of its padding"
        );
    }
}

/// #412 / AC-9 (TP-10, on disk): a runtime error in an `@extends` region whose file the
/// importer's map first registered through an `@include` splice names that file by its
/// root-relative path, never its absolute one, identically with source maps on and off.
///
/// `c.mds` extends `b.mds` and overrides `first` with `@include m`, where `m` is `e.mds`
/// — which extends `b.mds` too, with `n` = 5. The splice of `m`'s map registers
/// `b.mds` in `c.mds`'s builder, before `c.mds`'s `second` region — `b.mds`'s override,
/// where `n` is `hi` — raises the cross-type comparison.
#[test]
fn diagnostic_in_a_file_first_registered_by_a_splice_names_it_root_relative() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".mdsroot"), "").unwrap();
    let g5 = dir.path().join("g5");
    std::fs::create_dir(&g5).unwrap();
    let write = |name: &str, text: &str| std::fs::write(g5.join(name), text).unwrap();
    write(
        "a.mds",
        "@define helper():\nfrom a\n@end\n@block first:\nA-FIRST\n@end\n\
         @block second:\nA-SECOND\n@end\n",
    );
    write(
        "b.mds",
        "---\nn: 5\n---\n@extends \"./a.mds\"\n@block second:\n@if n == 5:\nfive\n@end\n@end\n",
    );
    write("e.mds", "@extends \"./b.mds\"\n");
    let leaf = |n: &str| {
        format!(
            "---\nn: {n}\nimports:\n  - path: ./e.mds\n    as: m\n---\n\
             @extends \"./b.mds\"\n@block first:\n@include m\n@end\n"
        )
    };
    write("c.mds", &leaf("hi"));

    // The root-relative name below is not vacuous: the control at the end shows that the
    // included module's splice carries `b.mds`, which it registers before the region
    // raising the error runs (PF-013).
    let [off, on] = [false, true].map(|source_map| {
        let err = mds::compile_with_deps_opts(
            g5.join("c.mds"),
            None,
            CompileOptions::default().with_source_map(source_map),
        )
        .expect_err("a cross-type comparison must fail");
        assert_eq!(
            err.serialize().code,
            "mds::type_mismatch",
            "source_map={source_map}: {err}"
        );
        assert_eq!(
            err.source_name(),
            Some("g5/b.mds"),
            "source_map={source_map}: the error must name the file root-relative"
        );
        err.serialize()
    });
    assert_eq!(off, on, "the error must not depend on source maps");

    // Non-vacuity: the splice carries `b.mds`. With `n` = 6 in `c.mds` its own `second`
    // region renders nothing, so the one `five` line comes from the included module,
    // and the map attributes it to `b.mds`.
    write("c.mds", &leaf("6"));
    let result = mds::compile_with_deps_opts(
        g5.join("c.mds"),
        None,
        CompileOptions::default().with_source_map(true),
    )
    .expect("the control must compile");
    let text = output_text(result.output);
    assert_eq!(text.matches("five").count(), 1, "{text:?}");
    let five_line = text.lines().position(|line| line == "five").expect("five");
    let sm = result.source_map.expect("maps on");
    assert!(
        mapped_points(&sm)
            .iter()
            .any(|point| point.0 == five_line && point.2 == "g5/b.mds"),
        "the included `five` line must map to g5/b.mds; sources={:?}",
        sm.sources
    );
}

/// #412 behaviour decision: a string compile of an extending child seeds its map with
/// the root base, as a direct compile of the same file does, so `sources[0]` is the
/// base and the string's own `input.mds` label follows it (a guard for the READMEs).
#[test]
fn extends_string_compile_names_the_base_first() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".mdsroot"), "").unwrap();
    std::fs::write(
        dir.path().join("base.mds"),
        "BASE-HEAD\n@block body:\nBASE-DEFAULT\n@end\n",
    )
    .unwrap();
    let result = mds::compile_str_with_deps_opts(
        "@extends \"./base.mds\"\n@block body:\nCHILD\n@end\n",
        Some(dir.path()),
        None,
        CompileOptions::default().with_source_map(true),
    )
    .expect("should compile");
    assert_eq!(output_text(result.output), "BASE-HEAD\nCHILD\n");
    let sm = result.source_map.expect("source_map must be present");
    assert_eq!(sm.sources, vec!["base.mds", "input.mds"]);
}
